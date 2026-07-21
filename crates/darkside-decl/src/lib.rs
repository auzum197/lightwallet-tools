//! Declaration parser for the darkside: text to chains and
//! scenarios.
//!
//! The file is authoritative. The parser drives the `darkside-chain` API.
//! There is no serialize path, and no randomization construct: an external
//! harness derives ground truth by reading the declaration.

mod parse;

use darkside_chain::{Chain, ChainParams, FundSpec, Seed, SendSpec};

pub use parse::DeclError;

/// A declared account line.
#[derive(Clone, Debug)]
pub struct AccountDecl {
    /// Account name, referenced by funds, sends, and barriers.
    pub name: String,
    /// The declared seed string (see `darkside_chain::seed_bytes`).
    pub seed_phrase: String,
    /// ZIP-32 account index, default 0.
    pub account_index: u32,
}

/// One `chain` block.
#[derive(Clone, Debug)]
pub struct ChainDecl {
    /// Chain name, referenced by scenarios and forks.
    pub name: String,
    /// `from <parent>@<height>` for forks.
    pub fork: Option<(String, u32)>,
    /// Declared tip: the highest height any construct mentions.
    pub tip: u32,
    /// Funds and sends, in declaration order.
    pub events: Vec<Event>,
}

/// A chain-block event.
#[derive(Clone, Debug)]
pub enum Event {
    /// A `fund` line.
    Fund(FundSpec),
    /// A `send` line.
    Send(SendSpec),
}

/// An RPC-observable barrier. Darkside can only see what the
/// wallet asks for. Anything else is rejected at parse time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Barrier {
    /// `wait block_requested >= <height>`
    BlockRequested(u32),
    /// `wait tx_submitted`
    TxSubmitted,
    /// `wait mempool_polled`
    MempoolPolled,
    /// `wait utxos_requested for <account>.taddr`
    UtxosRequested(String),
}

/// A scenario assertion.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Expectation {
    /// `expect tip == <height>`
    Tip(u32),
}

/// One ordered step of a scenario.
#[derive(Clone, Debug)]
pub enum Step {
    /// Serve the named chain from here on.
    Serve(String),
    /// Block until the barrier is observed.
    Wait(Barrier),
    /// Assert against the served chain.
    Expect(Expectation),
}

/// One `scenario` block: temporal interleaving, ordered by construction.
#[derive(Clone, Debug)]
pub struct Scenario {
    /// Scenario name.
    pub name: String,
    /// Steps in order.
    pub steps: Vec<Step>,
}

/// A parsed declaration: everything needed to build the world.
#[derive(Clone, Debug)]
pub struct Declaration {
    /// Consensus parameters from the `network` and `activation` lines.
    pub params: ChainParams,
    /// The chain seed.
    pub seed: Seed,
    /// Declared accounts.
    pub accounts: Vec<AccountDecl>,
    /// Declared chains, in order (forks after their parents).
    pub chains: Vec<ChainDecl>,
    /// Scenario scripts.
    pub scenarios: Vec<Scenario>,
}

/// The built world: chains by name, plus the scenarios that direct them.
pub struct World {
    /// Built chains, in declaration order.
    pub chains: Vec<(String, Chain)>,
    /// Scenario scripts, verbatim from the declaration.
    pub scenarios: Vec<Scenario>,
}

impl World {
    /// The built chain named `name`.
    pub fn chain(&self, name: &str) -> Option<&Chain> {
        self.chains.iter().find(|(n, _)| n == name).map(|(_, c)| c)
    }
}

impl Declaration {
    /// Parse a declaration source text. Everything the spec lists as a parser
    /// obligation fails here, except overdraws, which fail in
    /// [`Declaration::build`] while the chain is constructed.
    pub fn parse(src: &str) -> Result<Declaration, DeclError> {
        parse::parse(src)
    }

    /// Build every declared chain to its declared tip.
    pub fn build(&self) -> Result<World, DeclError> {
        let mut chains = Vec::new();
        for decl in &self.chains {
            let chain = self.build_into(decl, None, &chains)?;
            chains.push((decl.name.clone(), chain));
        }
        Ok(World {
            chains,
            scenarios: self.scenarios.clone(),
        })
    }

    /// Build one chain, optionally stopping below its declared tip (the
    /// `--replay-from` boundary). Scheduled content above the stop height
    /// stays registered, so a live driver mining onward includes it.
    pub fn build_chain(&self, name: &str, up_to: Option<u32>) -> Result<Chain, DeclError> {
        let mut built: Vec<(String, Chain)> = Vec::new();
        for decl in &self.chains {
            let target = if decl.name == name { up_to } else { None };
            let chain = self.build_into(decl, target, &built)?;
            if decl.name == name {
                return Ok(chain);
            }
            built.push((decl.name.clone(), chain));
        }
        Err(DeclError::new(0, format!("chain {name} is not declared")))
    }

    fn build_into(
        &self,
        decl: &ChainDecl,
        up_to: Option<u32>,
        built: &[(String, Chain)],
    ) -> Result<Chain, DeclError> {
        let err = |msg: String| DeclError::new(0, msg);
        let mut chain = match &decl.fork {
            Some((parent, height)) => {
                let parent_chain = built
                    .iter()
                    .find(|(n, _)| n == parent)
                    .map(|(_, c)| c)
                    .ok_or_else(|| {
                        err(format!(
                            "chain {parent} must be declared before forks of it"
                        ))
                    })?;
                parent_chain
                    .fork_at(*height)
                    .map_err(|e| err(format!("fork {}: {e}", decl.name)))?
            }
            None => {
                let mut chain = Chain::new(self.params.clone(), self.seed);
                for account in &self.accounts {
                    chain
                        .declare_account(&account.name, &account.seed_phrase, account.account_index)
                        .map_err(|e| err(format!("account {}: {e}", account.name)))?;
                }
                chain
            }
        };
        for event in &decl.events {
            match event {
                Event::Fund(spec) => chain.fund(spec.clone()),
                Event::Send(spec) => chain.send(spec.clone()),
            }
            .map_err(|e| err(format!("chain {}: {e}", decl.name)))?;
        }
        let target = up_to.unwrap_or(decl.tip).max(chain.tip_height());
        chain
            .mine(target - chain.tip_height())
            .map_err(|e| err(format!("chain {}: {e}", decl.name)))?;
        Ok(chain)
    }
}
