//! The darkside server: one chain, both variants' surfaces.
//!
//! [`Darkside`] is the shared handle: the served [`Chain`] behind a lock,
//! plus the request observations that scenario barriers wait on. Swapping
//! the chain (`serve`) is how a reorg happens. The wallet sees a `prevHash`
//! mismatch and reorgs, exactly as against a real indexer.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};

use darkside_chain::Chain;
use zcash_protocol::TxId;

mod wire;
#[macro_use]
mod service;
pub mod canonical;
pub mod command;
pub mod control;
pub mod crosslink;
mod driver;
mod indexer;

pub use driver::{LiveConfig, MinerControl, ScenarioError, Tick, run_live, run_scenario};
pub use indexer::DarksideIndexerClient;

/// Counts of what wallets have asked for: the only state scenario barriers
/// may reference. Wallet-internal state is structurally absent.
#[derive(Clone, Debug, Default)]
pub struct Counters {
    /// Highest block height any block RPC has requested.
    pub max_block_requested: u64,
    /// Number of `SendTransaction` calls.
    pub tx_submitted: u64,
    /// Number of mempool RPC calls.
    pub mempool_polled: u64,
    /// Number of UTXO queries per transparent address string.
    pub utxos_requested: HashMap<String, u64>,
    /// Bumped whenever the served chain changes: a swap or a mined block.
    pub epoch: u64,
}

/// Request observations plus a change signal for waiters.
#[derive(Default)]
pub struct Observations {
    counters: Mutex<Counters>,
    changed: tokio::sync::Notify,
}

impl Observations {
    fn record(&self, f: impl FnOnce(&mut Counters)) {
        {
            let mut counters = self.counters.lock().expect("observation lock");
            f(&mut counters);
        }
        self.changed.notify_waiters();
    }

    /// A copy of the current counters.
    pub fn snapshot(&self) -> Counters {
        self.counters.lock().expect("observation lock").clone()
    }

    /// Wait until `pred` holds over the counters.
    pub async fn wait(&self, pred: impl Fn(&Counters) -> bool) {
        loop {
            let notified = self.changed.notified();
            if pred(&self.counters.lock().expect("observation lock")) {
                return;
            }
            notified.await;
        }
    }
}

/// Darkside: a served chain plus its observations. Inexpensive to clone,
/// and clones share state.
#[derive(Clone)]
pub struct Darkside {
    chain: Arc<RwLock<Chain>>,
    observations: Arc<Observations>,
}

impl Darkside {
    /// Start serving `chain`.
    pub fn new(chain: Chain) -> Self {
        Darkside {
            chain: Arc::new(RwLock::new(chain)),
            observations: Arc::new(Observations::default()),
        }
    }

    /// Swap the served chain, returning the previous one. The wallet sees
    /// the swap as a reorg (or nothing, if the chains agree to the tip).
    pub fn serve(&self, chain: Chain) -> Chain {
        let previous = {
            let mut served = self.chain.write().expect("chain lock");
            std::mem::replace(&mut *served, chain)
        };
        self.observations.record(|c| c.epoch += 1);
        previous
    }

    /// Read the served chain.
    pub fn with_chain<R>(&self, f: impl FnOnce(&Chain) -> R) -> R {
        f(&self.chain.read().expect("chain lock"))
    }

    /// Mutate the served chain (the Rust-API escape hatch: corruption
    /// hooks, schedule additions).
    pub fn with_chain_mut<R>(&self, f: impl FnOnce(&mut Chain) -> R) -> R {
        let out = f(&mut self.chain.write().expect("chain lock"));
        self.observations.record(|c| c.epoch += 1);
        out
    }

    /// Mine one block at `time` through the mine path.
    pub fn mine_with_time(&self, time: u32) -> darkside_chain::Result<u32> {
        let tip = {
            let mut chain = self.chain.write().expect("chain lock");
            chain.mine_with_time(time)?
        };
        self.observations.record(|c| c.epoch += 1);
        Ok(tip)
    }

    /// Mine one block at `height`, leaving the span below it as empty
    /// blocks computed on demand.
    pub fn jump_to(&self, height: u32, time: u32) -> darkside_chain::Result<u32> {
        let tip = {
            let mut chain = self.chain.write().expect("chain lock");
            chain.jump_to(height, time)?
        };
        self.observations.record(|c| c.epoch += 1);
        Ok(tip)
    }

    /// Height of the served chain's tip.
    pub fn tip(&self) -> u32 {
        self.with_chain(|c| c.tip_height())
    }

    /// Accept raw transaction bytes, recording the submission for
    /// barriers.
    pub fn submit(&self, bytes: &[u8]) -> darkside_chain::Result<TxId> {
        let txid = {
            let mut chain = self.chain.write().expect("chain lock");
            chain.submit(bytes)?
        };
        self.observations.record(|c| c.tx_submitted += 1);
        Ok(txid)
    }

    /// Toggle the withhold knob: submissions accepted, never mined.
    pub fn set_withhold(&self, on: bool) {
        self.chain.write().expect("chain lock").set_withhold(on);
    }

    /// The request observations, for drivers and barriers.
    pub fn observations(&self) -> &Arc<Observations> {
        &self.observations
    }

    fn record_block_request(&self, height: u64) {
        self.observations
            .record(|c| c.max_block_requested = c.max_block_requested.max(height));
    }

    fn record_mempool_poll(&self) {
        self.observations.record(|c| c.mempool_polled += 1);
    }

    fn record_utxo_request(&self, address: &str) {
        self.observations
            .record(|c| *c.utxos_requested.entry(address.to_owned()).or_default() += 1);
    }

    async fn wait_epoch_change(&self, seen: u64) {
        self.observations.wait(|c| c.epoch != seen).await;
    }
}
