//! `lwcli`: a one-shot gRPC client for Zcash lightwallet indexers. Each RPC is
//! a typed subcommand calling the `lightwallet-core` indexers, so this binary
//! also dogfoods the crate's public API. Design decisions live in cli-plan.md
//! at the repo root.

mod render;

use anyhow::{Context, Result, anyhow, bail};
use clap::{CommandFactory, Parser, Subcommand, ValueEnum};
use clap_complete::Shell;
use futures_util::StreamExt;
use futures_util::stream::BoxStream;
use lightwallet_core::{
    CanonicalIdentityClient, CanonicalIndexer, CrosslinkIdentityClient, CrosslinkIndexer,
    NetworkParams, TestnetIndexer,
};
use render::{OutputMode, Renderer};
use std::io::Read;
#[cfg(feature = "nym")]
use std::net::SocketAddr;
use std::time::Duration;
use tonic::transport::{Channel, ClientTlsConfig, Endpoint};

#[derive(Parser)]
#[command(
    name = "lwcli",
    version,
    about = "One-shot gRPC client for Zcash lightwallet indexers",
    after_help = "Txids and block hashes are hex in display order (as explorers show them),\n\
                  both as arguments and in output. Other byte fields are wire-order hex."
)]
struct Cli {
    /// Indexer endpoint, e.g. `https://zec.rocks:443` (https verifies against
    /// webpki roots, http is plaintext)
    #[arg(short, long, global = true)]
    url: Option<String>,

    /// Protocol variant the endpoint serves [default: canonical; crosslink-only
    /// commands imply crosslink]
    #[arg(long, global = true, value_enum)]
    variant: Option<Variant>,

    /// Response format
    #[arg(long, global = true, value_enum, default_value_t = OutputMode::Json)]
    output: OutputMode,

    /// Connection timeout in seconds, covering the SOCKS5 handshake or circuit
    /// dial when a tunnel is in play (tor bootstrap runs unbounded). RPCs get
    /// no deadline: streams like get-mempool-stream legitimately run until the
    /// next block
    #[arg(long, global = true, default_value_t = 10)]
    timeout: u64,

    /// Route to the endpoint [default: direct]
    #[arg(long, global = true, value_enum)]
    transport: Option<Transport>,

    /// Address of a running nym-socks5-client. Passing it implies
    /// --transport nym [default: 127.0.0.1:1080]
    #[cfg(feature = "nym")]
    #[arg(long, global = true, value_name = "ADDR")]
    nym_socks5: Option<SocketAddr>,

    #[command(subcommand)]
    command: Cmd,
}

impl Cli {
    /// The resolved transport. An explicit `--nym-socks5` implies nym, the
    /// same rule crosslink-only commands use for the variant, and an explicit
    /// contradiction is an error.
    #[cfg(feature = "nym")]
    fn transport(&self) -> Result<Transport> {
        match (self.transport, self.nym_socks5) {
            (Some(chosen), Some(_)) if chosen != Transport::Nym => {
                bail!("--nym-socks5 applies only to the nym transport")
            }
            (_, Some(_)) => Ok(Transport::Nym),
            (chosen, None) => Ok(chosen.unwrap_or(Transport::Direct)),
        }
    }

    #[cfg(not(feature = "nym"))]
    fn transport(&self) -> Result<Transport> {
        Ok(self.transport.unwrap_or(Transport::Direct))
    }

    #[cfg(feature = "nym")]
    fn nym_socks5(&self) -> SocketAddr {
        self.nym_socks5
            .unwrap_or_else(|| SocketAddr::from(([127, 0, 0, 1], 1080)))
    }
}

#[derive(Clone, Copy, PartialEq, ValueEnum)]
enum Variant {
    Canonical,
    Crosslink,
}

#[derive(Clone, Copy, PartialEq, ValueEnum)]
enum Transport {
    /// Plain TCP, TLS by scheme
    Direct,
    /// Through the Tor network via an in-process arti client
    #[cfg(feature = "tor")]
    Tor,
    /// Through a running nym-socks5-client (experimental, pending the
    /// promotion gate)
    #[cfg(feature = "nym")]
    Nym,
}

#[derive(Clone, Copy, ValueEnum)]
enum Protocol {
    Sapling,
    Orchard,
    Ironwood,
}

#[derive(Subcommand)]
enum Cmd {
    /// Height of the block at the tip of the best chain
    GetLatestHeight,
    /// Compact block at the given height
    GetBlock { height: u64 },
    /// Compact blocks in [start, end], streamed as NDJSON
    GetBlockRange { start: u64, end: u64 },
    /// Note-commitment tree state at the given height
    GetTreeState { height: u64 },
    /// Note-commitment tree state at the chain tip
    GetLatestTreeState,
    /// Full transaction for a txid
    GetTransaction { txid: String },
    /// Submit a raw transaction (hex, or `-` to read hex from stdin)
    SendTransaction { tx: String },
    /// Full transactions involving a transparent address in [start, end]
    GetTaddressTransactions {
        address: String,
        start: u64,
        end: u64,
    },
    /// (deprecated upstream) Misnamed form of get-taddress-transactions
    GetTaddressTxids {
        address: String,
        start: u64,
        end: u64,
    },
    /// Total balance across transparent addresses
    GetTaddressBalance {
        #[arg(required = true)]
        addresses: Vec<String>,
    },
    /// Same balance lookup through the client-streaming RPC
    GetTaddressBalanceStream {
        #[arg(required = true)]
        addresses: Vec<String>,
    },
    /// Compact mempool transactions, streamed as NDJSON
    GetMempoolTx {
        /// Suppress transactions by txid suffix (wire-order hex, repeatable)
        #[arg(long = "exclude", value_name = "HEX")]
        exclude: Vec<String>,
    },
    /// Full mempool transactions, streamed as NDJSON until the next block
    GetMempoolStream,
    /// Note-commitment subtree roots for one shielded protocol, as NDJSON
    GetSubtreeRoots {
        #[arg(long, value_enum)]
        protocol: Protocol,
        #[arg(long, default_value_t = 0)]
        start_index: u32,
        /// 0 returns all entries
        #[arg(long, default_value_t = 0)]
        max_entries: u32,
    },
    /// Unspent transparent outputs for the given addresses
    GetAddressUtxos {
        #[arg(required = true)]
        addresses: Vec<String>,
        #[arg(long, default_value_t = 0)]
        start_height: u64,
        /// 0 returns all entries
        #[arg(long, default_value_t = 0)]
        max_entries: u32,
    },
    /// Streaming form of get-address-utxos, as NDJSON
    GetAddressUtxosStream {
        #[arg(required = true)]
        addresses: Vec<String>,
        #[arg(long, default_value_t = 0)]
        start_height: u64,
        /// 0 returns all entries
        #[arg(long, default_value_t = 0)]
        max_entries: u32,
    },
    /// Version and chain-state metadata for the indexer
    GetLightdInfo,
    /// Latency check; needs the server's insecure ping flag
    Ping {
        #[arg(default_value_t = 0)]
        interval_us: i64,
    },
    /// (deprecated upstream) Compact block with nullifiers at the given height
    GetBlockNullifiers { height: u64 },
    /// (deprecated upstream) Compact blocks with nullifiers in [start, end]
    GetBlockRangeNullifiers { start: u64, end: u64 },
    /// (crosslink) Current BFT finalizer roster, opaque bytes
    GetRoster,
    /// (crosslink) Delegation bond info for a 32-byte bond key (wire-order hex)
    GetBondInfo { bond_key: String },
    /// (crosslink, featurenet) Ask the faucet to fund a unified address
    RequestFaucetDonation { address: String },
    /// Print a shell completion script to stdout
    Completions { shell: Shell },
}

impl Cmd {
    fn crosslink_only(&self) -> bool {
        matches!(
            self,
            Cmd::GetRoster | Cmd::GetBondInfo { .. } | Cmd::RequestFaucetDonation { .. }
        )
    }
}

/// Run `$body` against whichever concrete indexer `$variant` selects, with
/// `$proto` bound to that variant's generated crate. The same trick as core's
/// `impl_streamer_methods!`: the shared surface is identical modulo the proto
/// path, so one body serves both.
macro_rules! dispatch {
    ($variant:expr, $channel:expr, |$ix:ident, $proto:ident| $body:expr) => {
        match $variant {
            Variant::Canonical => {
                #[allow(unused_imports)]
                use lightwallet_proto_canonical as $proto;
                let $ix = CanonicalIndexer::new($channel, cli_params());
                $body
            }
            Variant::Crosslink => {
                #[allow(unused_imports)]
                use lightwallet_proto_crosslink as $proto;
                let $ix = CrosslinkIndexer::new($channel, cli_params());
                $body
            }
        }
    };
}

/// Same dispatch for the identity-bearing RPCs, which live on the identity
/// clients rather than the indexers (docs/adr/0001). lwcli is one process,
/// one channel, one RPC, so an invocation is a single unlinkability domain
/// and handing the identity client the process's only channel is sound.
macro_rules! dispatch_identity {
    ($variant:expr, $channel:expr, |$ix:ident, $proto:ident| $body:expr) => {
        match $variant {
            Variant::Canonical => {
                #[allow(unused_imports)]
                use lightwallet_proto_canonical as $proto;
                let $ix = CanonicalIdentityClient::new($channel);
                $body
            }
            Variant::Crosslink => {
                #[allow(unused_imports)]
                use lightwallet_proto_crosslink as $proto;
                let $ix = CrosslinkIdentityClient::new($channel);
                $body
            }
        }
    };
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    if let Cmd::Completions { shell } = cli.command {
        clap_complete::generate(shell, &mut Cli::command(), "lwcli", &mut std::io::stdout());
        return Ok(());
    }

    let variant = match (cli.command.crosslink_only(), cli.variant) {
        (true, Some(Variant::Canonical)) => {
            bail!("this command exists only on the crosslink variant")
        }
        (true, _) => Variant::Crosslink,
        (false, chosen) => chosen.unwrap_or(Variant::Canonical),
    };

    let transport = cli.transport()?;

    let renderer = Renderer::new(cli.output)?;
    let url = cli
        .url
        .as_deref()
        .context("--url is required (there is no default endpoint)")?;
    let channel = connect(&cli, url, transport).await?;

    match cli.command {
        Cmd::GetLatestHeight => dispatch!(variant, channel, |ix, proto| {
            let height = ix.get_latest_height().await.map_err(rpc_err)?;
            render::emit(&height.to_string())
        }),
        Cmd::GetBlock { height } => dispatch!(variant, channel, |ix, proto| {
            let block = ix.get_block(height).await.map_err(rpc_err)?;
            renderer.unary(&block, "CompactBlock")
        }),
        Cmd::GetBlockRange { start, end } => dispatch!(variant, channel, |ix, proto| {
            let blocks = ix.get_block_range(start, end).await.map_err(rpc_err)?;
            drain(blocks, &renderer, "CompactBlock").await
        }),
        Cmd::GetTreeState { height } => dispatch!(variant, channel, |ix, proto| {
            let state = ix.get_tree_state(height).await.map_err(rpc_err)?;
            renderer.unary(&state, "TreeState")
        }),
        Cmd::GetLatestTreeState => dispatch!(variant, channel, |ix, proto| {
            let state = ix.get_latest_tree_state().await.map_err(rpc_err)?;
            renderer.unary(&state, "TreeState")
        }),
        Cmd::GetTransaction { txid } => {
            let txid = parse_txid(&txid)?;
            dispatch_identity!(variant, channel, |ix, proto| {
                let tx = ix.get_transaction(txid).await.map_err(rpc_err)?;
                renderer.unary(&tx, "RawTransaction")
            })
        }
        Cmd::SendTransaction { tx } => {
            let data = parse_tx_hex(&tx)?;
            dispatch_identity!(variant, channel, |ix, proto| {
                let response = ix.send_transaction(data).await.map_err(rpc_err)?;
                renderer.unary(&response, "SendResponse")
            })
        }
        Cmd::GetTaddressTransactions {
            address,
            start,
            end,
        } => {
            dispatch_identity!(variant, channel, |ix, proto| {
                let txs = ix
                    .get_taddress_transactions(address, start, end)
                    .await
                    .map_err(rpc_err)?;
                drain(txs, &renderer, "RawTransaction").await
            })
        }
        Cmd::GetTaddressTxids {
            address,
            start,
            end,
        } => {
            dispatch_identity!(variant, channel, |ix, proto| {
                #[allow(deprecated)]
                let txs = ix
                    .get_taddress_txids(address, start, end)
                    .await
                    .map_err(rpc_err)?;
                drain(txs, &renderer, "RawTransaction").await
            })
        }
        Cmd::GetTaddressBalance { addresses } => {
            dispatch_identity!(variant, channel, |ix, proto| {
                let balance = ix.get_taddress_balance(addresses).await.map_err(rpc_err)?;
                renderer.unary(&balance, "Balance")
            })
        }
        Cmd::GetTaddressBalanceStream { addresses } => {
            dispatch_identity!(variant, channel, |ix, proto| {
                let addresses = futures_util::stream::iter(
                    addresses
                        .into_iter()
                        .map(|address| proto::Address { address }),
                );
                let balance = ix
                    .get_taddress_balance_stream(addresses)
                    .await
                    .map_err(rpc_err)?;
                renderer.unary(&balance, "Balance")
            })
        }
        Cmd::GetMempoolTx { exclude } => {
            let exclude = exclude
                .iter()
                .map(|suffix| hex::decode(suffix).context("--exclude takes hex"))
                .collect::<Result<Vec<_>>>()?;
            dispatch_identity!(variant, channel, |ix, proto| {
                let txs = ix.get_mempool_tx(exclude).await.map_err(rpc_err)?;
                drain(txs, &renderer, "CompactTx").await
            })
        }
        Cmd::GetMempoolStream => dispatch!(variant, channel, |ix, proto| {
            let txs = ix.get_mempool_stream().await.map_err(rpc_err)?;
            drain(txs, &renderer, "RawTransaction").await
        }),
        Cmd::GetSubtreeRoots {
            protocol,
            start_index,
            max_entries,
        } => {
            dispatch!(variant, channel, |ix, proto| {
                let arg = proto::GetSubtreeRootsArg {
                    start_index,
                    shielded_protocol: match protocol {
                        Protocol::Sapling => proto::ShieldedProtocol::Sapling,
                        Protocol::Orchard => proto::ShieldedProtocol::Orchard,
                        Protocol::Ironwood => proto::ShieldedProtocol::Ironwood,
                    } as i32,
                    max_entries,
                };
                let roots = ix.get_subtree_roots(arg).await.map_err(rpc_err)?;
                drain(roots, &renderer, "SubtreeRoot").await
            })
        }
        Cmd::GetAddressUtxos {
            addresses,
            start_height,
            max_entries,
        } => {
            dispatch_identity!(variant, channel, |ix, proto| {
                let utxos = ix
                    .get_address_utxos(addresses, start_height, max_entries)
                    .await
                    .map_err(rpc_err)?;
                renderer.unary(&utxos, "GetAddressUtxosReplyList")
            })
        }
        Cmd::GetAddressUtxosStream {
            addresses,
            start_height,
            max_entries,
        } => {
            dispatch_identity!(variant, channel, |ix, proto| {
                let utxos = ix
                    .get_address_utxos_stream(addresses, start_height, max_entries)
                    .await
                    .map_err(rpc_err)?;
                drain(utxos, &renderer, "GetAddressUtxosReply").await
            })
        }
        Cmd::GetLightdInfo => dispatch!(variant, channel, |ix, proto| {
            let info = ix.get_lightd_info().await.map_err(rpc_err)?;
            renderer.unary(&info, "LightdInfo")
        }),
        Cmd::Ping { interval_us } => dispatch!(variant, channel, |ix, proto| {
            let pong = ix.ping(interval_us).await.map_err(rpc_err)?;
            renderer.unary(&pong, "PingResponse")
        }),
        Cmd::GetBlockNullifiers { height } => dispatch!(variant, channel, |ix, proto| {
            #[allow(deprecated)]
            let block = ix.get_block_nullifiers(height).await.map_err(rpc_err)?;
            renderer.unary(&block, "CompactBlock")
        }),
        Cmd::GetBlockRangeNullifiers { start, end } => {
            dispatch!(variant, channel, |ix, proto| {
                #[allow(deprecated)]
                let blocks = ix
                    .get_block_range_nullifiers(start, end)
                    .await
                    .map_err(rpc_err)?;
                drain(blocks, &renderer, "CompactBlock").await
            })
        }
        Cmd::GetRoster => {
            let ix = CrosslinkIndexer::new(channel, cli_params());
            let roster = ix.get_roster().await.map_err(rpc_err)?;
            renderer.unary(&roster, "Bytes")
        }
        Cmd::GetBondInfo { bond_key } => {
            let bond_key = hex::decode(&bond_key).context("bond key is not valid hex")?;
            let ix = CrosslinkIndexer::new(channel, cli_params());
            let info = ix.get_bond_info(bond_key).await.map_err(rpc_err)?;
            renderer.unary(&info, "BondInfoResponse")
        }
        Cmd::RequestFaucetDonation { address } => {
            let ix = CrosslinkIdentityClient::new(channel);
            let donation = ix.request_faucet_donation(address).await.map_err(rpc_err)?;
            renderer.unary(&donation, "FaucetResponse")
        }
        Cmd::Completions { .. } => Ok(()), // handled before connecting
    }
}

/// Open the channel over the resolved transport. TLS follows the scheme in
/// every case: tonic runs the handshake end-to-end through the tunnel when
/// the endpoint carries a `tls_config`, and tonic applies `connect_timeout`
/// to custom connectors too, so --timeout means the same thing on all routes.
async fn connect(cli: &Cli, url: &str, transport: Transport) -> Result<Channel> {
    let mut endpoint = Endpoint::from_shared(url.to_string())
        .with_context(|| format!("invalid endpoint url {url}"))?
        .connect_timeout(Duration::from_secs(cli.timeout));
    if url.starts_with("https://") {
        endpoint = endpoint
            .tls_config(ClientTlsConfig::new().with_webpki_roots())
            .context("tls configuration")?;
    }
    match transport {
        Transport::Direct => endpoint
            .connect()
            .await
            .with_context(|| format!("connecting to {url}")),
        #[cfg(feature = "tor")]
        Transport::Tor => {
            // Default arti config so the directory cache persists in the
            // platform state dir: the first run bootstraps in seconds, later
            // runs in a fraction of that.
            eprintln!("bootstrapping tor...");
            let tor = arti_client::TorClient::create_bootstrapped(
                arti_client::TorClientConfig::default(),
            )
            .await
            .context("bootstrapping tor")?;
            lightwallet_transport_tor::channel(&endpoint, &tor)
                .await
                .with_context(|| format!("connecting to {url} over tor"))
        }
        #[cfg(feature = "nym")]
        Transport::Nym => {
            let socks = cli.nym_socks5();
            lightwallet_transport_nym::channel(&endpoint, socks)
                .await
                .with_context(|| {
                    format!("connecting to {url} through nym-socks5-client at {socks}")
                })
        }
    }
}

async fn drain<T: prost::Message + std::fmt::Debug>(
    mut stream: BoxStream<'static, lightwallet_core::Result<T>>,
    renderer: &Renderer,
    type_name: &str,
) -> Result<()> {
    while let Some(item) = stream.next().await {
        renderer.item(&item.map_err(rpc_err)?, type_name)?;
    }
    Ok(())
}

fn rpc_err(e: lightwallet_core::Error) -> anyhow::Error {
    if e.retryable() {
        anyhow!("{e} (retryable)")
    } else {
        anyhow!("{e}")
    }
}

/// Params are per-deployment data the indexers carry for consumers; nothing on
/// the one-shot RPC surface reads them, so an empty set is fine here.
fn cli_params() -> NetworkParams {
    NetworkParams {
        chain_name: String::new(),
        activation_heights: Default::default(),
        consensus_branch_id: 0,
    }
}

/// Parse a txid given in display order (as explorers show it) into the wire
/// byte order the protocol expects.
fn parse_txid(txid: &str) -> Result<Vec<u8>> {
    let mut bytes = hex::decode(txid).context("txid is not valid hex")?;
    if bytes.len() != 32 {
        bail!("txid is {} bytes, expected 32", bytes.len());
    }
    bytes.reverse();
    Ok(bytes)
}

fn parse_tx_hex(arg: &str) -> Result<Vec<u8>> {
    let hex_str = if arg == "-" {
        let mut buffered = String::new();
        std::io::stdin()
            .read_to_string(&mut buffered)
            .context("reading the transaction from stdin")?;
        buffered
    } else {
        arg.to_string()
    };
    let data = hex::decode(hex_str.trim()).context("transaction is not valid hex")?;
    if data.is_empty() {
        bail!("transaction is empty");
    }
    Ok(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_definition_is_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn txids_parse_from_display_order() {
        let display = format!("{}{}", "00".repeat(31), "ff");
        let wire = parse_txid(&display).unwrap();
        assert_eq!(wire[0], 0xff);
        assert_eq!(wire[31], 0x00);
        assert!(parse_txid("abcd").is_err());
        assert!(parse_txid(&"zz".repeat(32)).is_err());
    }

    #[test]
    fn tx_hex_rejects_junk_and_empty_input() {
        assert!(parse_tx_hex("zz").unwrap_err().to_string().contains("hex"));
        assert!(parse_tx_hex("").unwrap_err().to_string().contains("empty"));
        assert_eq!(
            parse_tx_hex("  deadbeef\n").unwrap(),
            vec![0xde, 0xad, 0xbe, 0xef]
        );
    }

    #[test]
    fn crosslink_only_commands_are_marked() {
        let cli = Cli::parse_from(["lwcli", "get-roster"]);
        assert!(cli.command.crosslink_only());
        let cli = Cli::parse_from(["lwcli", "get-block", "100"]);
        assert!(!cli.command.crosslink_only());
    }

    #[test]
    fn the_transport_defaults_to_direct() {
        let cli = Cli::parse_from(["lwcli", "get-latest-height"]);
        assert!(matches!(cli.transport().unwrap(), Transport::Direct));
    }

    #[cfg(feature = "nym")]
    #[test]
    fn nym_socks5_implies_the_transport() {
        let cli = Cli::parse_from([
            "lwcli",
            "--nym-socks5",
            "127.0.0.1:9060",
            "get-latest-height",
        ]);
        assert!(matches!(cli.transport().unwrap(), Transport::Nym));
        assert_eq!(cli.nym_socks5().port(), 9060);
    }

    #[cfg(feature = "nym")]
    #[test]
    fn nym_socks5_contradicts_an_explicit_transport() {
        let cli = Cli::parse_from([
            "lwcli",
            "--transport",
            "direct",
            "--nym-socks5",
            "127.0.0.1:9060",
            "get-latest-height",
        ]);
        assert!(cli.transport().is_err());
    }
}
