//! The darkside binary.
//!
//! A flag-driven live server: it serves one network-flavored chain and mines
//! it forward on a wall clock. The declaration language and scenario runner
//! live in the library (`darkside-decl`, `darkside-serve::run_scenario`) for
//! Rust-authored worlds and deterministic tests.

use std::net::SocketAddr;

use anyhow::{Context, anyhow};
use clap::{Parser, ValueEnum};
use darkside_chain::{Chain, ChainParams, Seed};
#[cfg(feature = "experimental")]
use darkside_chain::{FundSpec, Recipient};
use darkside_serve::command::{BootChain, Dispatcher};
use darkside_serve::{
    Darkside, LiveConfig, MinerControl, Tick, canonical, control, crosslink, run_live,
};
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio_stream::wrappers::TcpListenerStream;
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

#[derive(Parser)]
#[command(
    name = "darkside",
    version,
    about = "darkside: a deterministic Zcash lightwallet backend serving a fabricated chain"
)]
enum Cli {
    /// Serve a live synthetic chain over gRPC.
    Serve(ServeArgs),

    /// Turnkey ironwood demo: a preset chain with sapling, orchard, and
    /// ironwood live from boot, two accounts from fixed BIP-39 mnemonics
    /// (one pre-funded in orchard), served live. Import a printed mnemonic
    /// into a wallet and connect.
    #[cfg(feature = "experimental")]
    Experimental(ExperimentalArgs),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum Variant {
    Canonical,
    Crosslink,
}

/// A named network darkside stands in for. Each pins its encoding, its
/// variant, and a default activation schedule, so a named deployment cannot
/// be an impossible pairing. `custom` opens the raw encoding/variant/schedule
/// choices instead.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum Deployment {
    ZcashMainnet,
    /// Zcash testnet. Doubles as the network's staging environment.
    ZcashTestnet,
    CrosslinkFeaturenet,
    Custom,
}

impl Deployment {
    fn as_str(self) -> &'static str {
        match self {
            Deployment::ZcashMainnet => "zcash-mainnet",
            Deployment::ZcashTestnet => "zcash-testnet",
            Deployment::CrosslinkFeaturenet => "crosslink-featurenet",
            Deployment::Custom => "custom",
        }
    }
}

/// The address-prefix and consensus-branch scheme for a custom chain.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum Encoding {
    Main,
    Test,
    Regtest,
}

#[derive(Parser)]
struct ServeArgs {
    /// Which network darkside presents as. Pins the encoding, variant,
    /// and default schedule. Use `custom` for a bespoke chain.
    #[arg(long, value_enum, default_value_t = Deployment::ZcashMainnet)]
    deployment: Deployment,

    /// Address encoding for a custom chain. Rejected for a named deployment,
    /// which pins its own. Defaults to regtest.
    #[arg(long, value_enum)]
    encoding: Option<Encoding>,

    /// Protocol variant to serve for a custom chain. Rejected for a named
    /// deployment, which pins its own. Defaults to canonical.
    #[arg(long, value_enum)]
    variant: Option<Variant>,

    /// Chain seed for deterministic fabrication.
    #[arg(long, default_value_t = 0)]
    seed: u64,

    /// Boot height: the synthetic present, where the chain starts and mines
    /// forward. Blocks below it are served as empty prehistory. Defaults to
    /// the deployment's NU5 activation (0 for a regtest-encoded chain), after
    /// any overrides.
    #[arg(long)]
    start_height: Option<u32>,

    /// Activation-height overrides, e.g. `all=1, nu6.3=on`. Comma-separated
    /// `key=value`. The value is a height, `off`, or `on` (earliest legal).
    /// Honored for every deployment, the lying axis over its base schedule.
    #[arg(long)]
    activation_heights: Option<String>,

    /// Block cadence in seconds: a fixed `N`, or a range `LOW..HIGH` drawn
    /// afresh before each block. Timestamps remain the real wall clock.
    #[arg(long, default_value = "10", value_parser = parse_tick)]
    tick: Tick,

    /// Mine the moment a transaction is accepted.
    #[arg(long)]
    instamine: bool,

    /// Accept wallet submissions but never mine them.
    #[arg(long)]
    withhold: bool,

    /// Address to bind. Defaults to loopback. Pass a non-loopback address
    /// to expose the server to the network.
    #[arg(long, default_value = "127.0.0.1:9067")]
    listen: SocketAddr,

    /// Address the HTTP control surface binds. Loopback only: this surface
    /// fabricates value and rewrites history, and there is nothing to
    /// authenticate with yet.
    #[arg(long, default_value = "127.0.0.1:9068")]
    control_listen: SocketAddr,

    /// Most blocks one control command may mine, so a mistyped height
    /// fails instead of hanging the connection.
    #[arg(long, default_value_t = 10_000)]
    max_blocks: u32,
}

/// The preset ironwood demo. Network encoding is a knob so the same demo
/// works against whatever a given wallet accepts; everything else (seed,
/// schedule, accounts, funding) is pinned.
#[cfg(feature = "experimental")]
#[derive(Parser)]
struct ExperimentalArgs {
    /// Address encoding the preset chain presents. Testnet suits most
    /// wallets; regtest is rejected by many.
    #[arg(long, value_enum, default_value_t = Encoding::Test)]
    encoding: Encoding,

    /// Address to bind. Defaults to loopback; pass a non-loopback address
    /// to expose the demo to other machines.
    #[arg(long, default_value = "127.0.0.1:9067")]
    listen: SocketAddr,

    /// Block cadence in seconds: a fixed `N`, or a range `LOW..HIGH`.
    #[arg(long, default_value = "10", value_parser = parse_tick)]
    tick: Tick,

    /// Wait for the cadence instead of mining each submission the moment it
    /// arrives. The demo instamines by default so self-sends confirm at once.
    #[arg(long)]
    no_instamine: bool,
}

/// Parse `--tick`: a bare `N` for a fixed cadence, or `LOW..HIGH` for a
/// uniform range. A range must satisfy `1 <= LOW <= HIGH`, since a zero
/// wait would let two blocks race the same wall-clock second.
fn parse_tick(spec: &str) -> Result<Tick, String> {
    spec.parse()
}

/// Install the tracing subscriber that renders the per-call events the RPC
/// handlers log (in `darkside-serve`). Default shows them at info; RUST_LOG
/// overrides, e.g. `darkside=debug` for the full request bodies.
fn init_tracing() {
    tracing_subscriber::registry()
        .with(fmt::layer().with_writer(std::io::stderr))
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("darkside=info")))
        .init();
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    #[cfg(feature = "experimental")]
    let args = match cli {
        Cli::Serve(args) => args,
        Cli::Experimental(exp) => return run_experimental(exp).await,
    };
    #[cfg(not(feature = "experimental"))]
    let Cli::Serve(args) = cli;

    init_tracing();

    let (mut params, variant) = resolve_deployment(&args)?;
    if let Some(spec) = &args.activation_heights {
        params
            .network
            .apply_overrides(spec)
            .map_err(|e| anyhow!("--activation-heights: {e}"))?;
    }
    params.start_height = args
        .start_height
        .unwrap_or_else(|| params.default_start_height());

    if !control::is_loopback(&args.control_listen) {
        return Err(anyhow!(
            "--control-listen {} is not loopback; the control surface fabricates value and \
             rewrites history, and has no authentication yet",
            args.control_listen
        ));
    }

    let seed = Seed::from(args.seed);
    let boot = BootChain::new(params.clone(), seed);
    let chain = Chain::new(params, seed);
    let boot_height = chain.tip_height();
    let chain_name = chain.params().chain_name.clone();
    let darkside = Darkside::new(chain);

    // Claim both sockets before announcing anything, so an address clash
    // surfaces as an error instead of a false "serving" line.
    let listener = TcpListener::bind(args.listen)
        .await
        .with_context(|| format!("binding {}", args.listen))?;
    let control_listener = TcpListener::bind(args.control_listen)
        .await
        .with_context(|| format!("binding {}", args.control_listen))?;

    let (miner, miner_rx) = watch::channel(MinerControl {
        paused: false,
        tick: args.tick,
    });
    let driver = tokio::spawn(run_live(
        darkside.clone(),
        LiveConfig {
            instamine: args.instamine,
            withhold: args.withhold,
        },
        miner_rx,
    ));

    let dispatcher = Dispatcher::new(darkside.clone(), miner, boot, args.max_blocks);
    let control =
        tokio::spawn(
            async move { axum::serve(control_listener, control::router(dispatcher)).await },
        );

    eprintln!(
        "darkside: serving {chain_name} ({variant:?} surface) at http://{}, boot height \
         {boot_height}, cadence {}",
        args.listen, args.tick
    );
    eprintln!("darkside: control at http://{}", args.control_listen);
    serve(darkside, variant, listener, shutdown_signal()).await?;
    control.abort();
    driver.abort();
    Ok(())
}

/// Resolve the deployment into its chain parameters and served variant. A
/// named deployment pins both, so passing `--encoding` or `--variant`
/// alongside one is a mistake worth surfacing rather than ignoring.
fn resolve_deployment(args: &ServeArgs) -> anyhow::Result<(ChainParams, Variant)> {
    if args.deployment == Deployment::Custom {
        let params = match args.encoding.unwrap_or(Encoding::Regtest) {
            Encoding::Main => ChainParams::main(),
            Encoding::Test => ChainParams::test(),
            Encoding::Regtest => ChainParams::regtest(),
        };
        return Ok((params, args.variant.unwrap_or(Variant::Canonical)));
    }

    if args.encoding.is_some() || args.variant.is_some() {
        return Err(anyhow!(
            "--encoding and --variant apply only to `--deployment custom`; the {} deployment \
             pins them",
            args.deployment.as_str()
        ));
    }
    let resolved = match args.deployment {
        Deployment::ZcashMainnet => (ChainParams::main(), Variant::Canonical),
        Deployment::ZcashTestnet => (ChainParams::test(), Variant::Canonical),
        Deployment::CrosslinkFeaturenet => (ChainParams::crosslink_testnet(), Variant::Crosslink),
        Deployment::Custom => unreachable!("custom handled above"),
    };
    Ok(resolved)
}

/// Resolves on Ctrl-C, then arms a second Ctrl-C to force-quit. tokio keeps its
/// SIGINT handler installed once armed, so a wedged shutdown would otherwise
/// swallow every further Ctrl-C and leave `kill` as the only way out.
async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
    eprintln!("darkside: shutting down (Ctrl-C again to force-quit)");
    tokio::spawn(async {
        let _ = tokio::signal::ctrl_c().await;
        std::process::exit(130);
    });
}

async fn serve(
    darkside: Darkside,
    variant: Variant,
    listener: TcpListener,
    shutdown: impl Future<Output = ()>,
) -> anyhow::Result<()> {
    let incoming = TcpListenerStream::new(listener);
    let mut builder = tonic::transport::Server::builder();
    // Race serving against Ctrl-C instead of handing the signal to tonic's
    // graceful shutdown: a connected wallet holds a streaming RPC open, so the
    // graceful drain would wait on it forever. Dropping the server future on
    // signal closes every connection at once and returns promptly.
    tokio::select! {
        result = async {
            match variant {
                Variant::Canonical => builder
                    .add_service(canonical::service(darkside))
                    .serve_with_incoming(incoming)
                    .await
                    .context("serving the canonical surface"),
                Variant::Crosslink => builder
                    .add_service(crosslink::service(darkside))
                    .serve_with_incoming(incoming)
                    .await
                    .context("serving the crosslink surface"),
            }
        } => result?,
        _ = shutdown => {}
    }
    Ok(())
}

/// Wallet A's fixed seed: the 24-word all-zero-entropy BIP-39 test vector.
/// Funded in orchard. Import this to hold the demo funds.
#[cfg(feature = "experimental")]
const WALLET_A_MNEMONIC: &str = "abandon abandon abandon abandon abandon abandon abandon \
    abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon \
    abandon abandon abandon abandon abandon art";

/// Wallet B's fixed seed: the 24-word all-`0xff`-entropy BIP-39 test vector.
/// Declared but unfunded, the target for the ironwood send.
#[cfg(feature = "experimental")]
const WALLET_B_MNEMONIC: &str = "zoo zoo zoo zoo zoo zoo zoo zoo zoo zoo zoo zoo zoo zoo zoo \
    zoo zoo zoo zoo zoo zoo zoo zoo vote";

/// How much lands in wallet A's orchard pool, in zatoshis.
#[cfg(feature = "experimental")]
const DEMO_FUND_ZATS: u64 = 10 * 100_000_000;

/// A BIP-39 mnemonic's 64-byte seed (empty passphrase) as the `0x<hex>` string
/// darkside's verbatim seed branch consumes. A wallet importing the same
/// mnemonic derives the identical seed, so it becomes the account.
#[cfg(feature = "experimental")]
fn mnemonic_seed_phrase(mnemonic: &str) -> String {
    let seed = bip0039::Mnemonic::<bip0039::English>::from_phrase(mnemonic)
        .expect("preset demo mnemonic is a valid BIP-39 phrase")
        .to_seed("");
    format!("0x{}", hex::encode(seed))
}

/// Build the preset chain: the chosen encoding with every upgrade forced live
/// from boot, two accounts from the fixed mnemonics, wallet A funded in
/// orchard, mined to realize the fund.
#[cfg(feature = "experimental")]
fn build_experimental_chain(encoding: Encoding) -> anyhow::Result<Chain> {
    let mut params = match encoding {
        Encoding::Main => ChainParams::main(),
        Encoding::Test => ChainParams::test(),
        Encoding::Regtest => ChainParams::regtest(),
    };
    // `all=1` flattens every upgrade to height 1, including Nu6.3 (ironwood),
    // which the mainnet/testnet tables leave inactive.
    params
        .network
        .apply_overrides("all=1")
        .map_err(|e| anyhow!("preset activation override: {e}"))?;
    params.start_height = params.default_start_height();

    let mut chain = Chain::new(params, Seed::from(0));
    chain.declare_account("wallet-a", &mnemonic_seed_phrase(WALLET_A_MNEMONIC), 0)?;
    chain.declare_account("wallet-b", &mnemonic_seed_phrase(WALLET_B_MNEMONIC), 0)?;
    chain.fund(FundSpec {
        recipient: Recipient::Declared("wallet-a".into()),
        pool: None,
        zats: DEMO_FUND_ZATS,
        outputs: 1,
        at: 2,
        via_coinbase: false,
        corruption: None,
    })?;
    chain.mine(2)?;
    Ok(chain)
}

/// Run the preset ironwood demo: build the chain, print the mnemonics and
/// wallet A's addresses, and serve live over the canonical surface.
#[cfg(feature = "experimental")]
async fn run_experimental(args: ExperimentalArgs) -> anyhow::Result<()> {
    init_tracing();

    let chain = build_experimental_chain(args.encoding)?;

    let account = chain
        .account("wallet-a")
        .expect("build_experimental_chain declares wallet-a");
    let ua = account.ua().encode(&chain.params().network);
    let taddr = chain.params().encode_taddr(account.taddr());
    let chain_name = chain.params().chain_name.clone();
    let boot = chain.tip_height();

    let darkside = Darkside::new(chain);

    let listener = TcpListener::bind(args.listen)
        .await
        .with_context(|| format!("binding {}", args.listen))?;

    let instamine = !args.no_instamine;
    let (_miner, miner_rx) = watch::channel(MinerControl {
        paused: false,
        tick: args.tick,
    });
    let driver = tokio::spawn(run_live(
        darkside.clone(),
        LiveConfig {
            instamine,
            withhold: false,
        },
        miner_rx,
    ));

    eprintln!(
        "darkside experimental demo: serving {chain_name} at http://{}",
        args.listen
    );
    eprintln!("  pools live from boot: sapling, orchard, ironwood");
    eprintln!(
        "  boot height {boot}, cadence {}, instamine {instamine}",
        args.tick
    );
    eprintln!("  import a mnemonic (BIP-39, empty passphrase, account 0):");
    eprintln!(
        "  wallet A, funded {} ZEC in orchard:",
        DEMO_FUND_ZATS / 100_000_000
    );
    eprintln!("    {WALLET_A_MNEMONIC}");
    eprintln!("    UA:    {ua}");
    eprintln!("    taddr: {taddr}");
    eprintln!("  wallet B, empty, the ironwood send target:");
    eprintln!("    {WALLET_B_MNEMONIC}");

    serve(darkside, Variant::Canonical, listener, shutdown_signal()).await?;
    driver.abort();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_value_is_a_fixed_cadence() {
        assert_eq!(parse_tick("30").unwrap(), Tick::fixed(30));
    }

    #[test]
    fn dotted_value_is_a_range() {
        assert_eq!(parse_tick("15..90").unwrap(), Tick::range(15, 90));
    }

    #[test]
    fn range_rejects_zero_and_inverted_bounds() {
        assert!(parse_tick("0..90").is_err());
        assert!(parse_tick("90..15").is_err());
        assert!(parse_tick("abc").is_err());
    }

    #[test]
    fn cli_definition_is_consistent() {
        use clap::CommandFactory;
        Cli::command().debug_assert();
    }
}

#[cfg(all(test, feature = "experimental"))]
mod experimental_tests {
    use super::*;
    use darkside_chain::Pool;

    #[test]
    fn preset_funds_orchard_for_wallet_a() {
        let chain = build_experimental_chain(Encoding::Test).unwrap();
        assert_eq!(
            chain
                .expected_balance_in("wallet-a", Pool::Orchard)
                .unwrap(),
            DEMO_FUND_ZATS
        );
        assert_eq!(
            chain
                .expected_balance_in("wallet-a", Pool::Sapling)
                .unwrap(),
            0
        );
        assert_eq!(chain.expected_balance("wallet-b").unwrap(), 0);
        assert!(chain.params().ironwood_active(chain.tip_height()));
    }

    #[test]
    fn demo_seeds_match_the_known_vectors() {
        // Trezor BIP-39 vectors, empty passphrase. Pins the derivation so a
        // dependency bump or an edited mnemonic is caught.
        assert_eq!(
            mnemonic_seed_phrase(WALLET_A_MNEMONIC),
            "0x408b285c123836004f4b8842c89324c1f01382450c0d439af345ba7fc49acf705489c6fc77dbd4e3dc1dd8cc6bc9f043db8ada1e243c4a0eafb290d399480840"
        );
        assert_eq!(
            mnemonic_seed_phrase(WALLET_B_MNEMONIC),
            "0xe28a37058c7f5112ec9e16a3437cf363a2572d70b6ceb3b6965447623d620f14d06bb321a26b33ec15fcd84a3b5ddfd5520e230c924c87aaa0d559749e044fef"
        );
    }
}
