//! The two drivers. Same chain, different clock source: live ticks a
//! wall clock into `mine`, scenario advances on RPC-observable barriers and
//! reads no clock anywhere.

use core::fmt;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use darkside_chain::Chain;
use darkside_decl::{Barrier, Expectation, Scenario, Step};
use rand::Rng as _;
use tokio::sync::watch;

use crate::Darkside;

/// The live driver's block cadence: a fixed wall-clock interval, or a
/// uniform range drawn afresh before each block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tick {
    /// One interval, every block.
    Fixed(Duration),
    /// A fresh interval drawn uniformly in `[low, high]` per block.
    Range {
        /// Shortest wait.
        low: Duration,
        /// Longest wait.
        high: Duration,
    },
}

impl Tick {
    /// A fixed cadence of `secs` seconds between blocks.
    pub fn fixed(secs: u64) -> Self {
        Tick::Fixed(Duration::from_secs(secs))
    }

    /// A cadence drawn uniformly between `low` and `high` seconds.
    pub fn range(low: u64, high: u64) -> Self {
        Tick::Range {
            low: Duration::from_secs(low),
            high: Duration::from_secs(high),
        }
    }

    /// The wait before the next block. A range draws uniformly in
    /// `[low, high]`. Timestamps stay the real wall clock, so this only
    /// governs how long blocks take to appear.
    fn wait(self) -> Duration {
        match self {
            Tick::Fixed(d) => d,
            Tick::Range { low, high } => {
                let lo = low.as_millis() as u64;
                let hi = high.as_millis() as u64;
                if hi <= lo {
                    low
                } else {
                    Duration::from_millis(rand::thread_rng().gen_range(lo..=hi))
                }
            }
        }
    }
}

impl std::str::FromStr for Tick {
    type Err = String;

    /// A bare `N` for a fixed cadence, or `LOW..HIGH` for a uniform range.
    /// A range must satisfy `1 <= LOW <= HIGH`, since a zero wait would let
    /// two blocks race the same wall-clock second. A trailing `s` is allowed
    /// so a `Tick` parses back from its own `Display`.
    fn from_str(spec: &str) -> Result<Self, Self::Err> {
        let spec = spec.trim();
        let spec = spec.strip_suffix('s').unwrap_or(spec);
        if let Some((lo, hi)) = spec.split_once("..") {
            let low = lo
                .trim()
                .parse::<u64>()
                .map_err(|_| format!("invalid range low bound `{lo}`"))?;
            let high = hi
                .trim()
                .parse::<u64>()
                .map_err(|_| format!("invalid range high bound `{hi}`"))?;
            if low == 0 {
                return Err("range low bound must be at least 1 second".into());
            }
            if high < low {
                return Err(format!("range high bound {high} is below low bound {low}"));
            }
            Ok(Tick::range(low, high))
        } else {
            spec.trim()
                .parse::<u64>()
                .map(Tick::fixed)
                .map_err(|_| format!("invalid tick `{spec}`"))
        }
    }
}

impl fmt::Display for Tick {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Tick::Fixed(d) => write!(f, "{}s", d.as_secs()),
            Tick::Range { low, high } => write!(f, "{}..{}s", low.as_secs(), high.as_secs()),
        }
    }
}

/// Live-mode configuration fixed at boot.
#[derive(Clone, Debug, Default)]
pub struct LiveConfig {
    /// Mine the moment a transaction is accepted instead of waiting for
    /// the tick.
    pub instamine: bool,
    /// Accept wallet submissions but never mine them.
    pub withhold: bool,
}

/// The live miner's editable state. Carried on a watch channel so an edit
/// lands mid-wait rather than after the wait in flight.
#[derive(Clone, Copy, Debug)]
pub struct MinerControl {
    /// Auto-mining is halted. Blocks appear only when something asks for
    /// one.
    pub paused: bool,
    /// Cadence between auto-mined blocks.
    pub tick: Tick,
}

impl Default for MinerControl {
    fn default() -> Self {
        MinerControl {
            paused: false,
            tick: Tick::fixed(10),
        }
    }
}

fn wall_clock() -> u32 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as u32)
        .unwrap_or_default()
}

/// Run the live driver forever: blocks appear every tick, timestamps are
/// wall clock, mining continues past the declared tip indefinitely.
///
/// `control` carries the pause flag and the cadence. Ends when every sender
/// has dropped.
pub async fn run_live(
    darkside: Darkside,
    config: LiveConfig,
    mut control: watch::Receiver<MinerControl>,
) -> darkside_chain::Result<()> {
    darkside.set_withhold(config.withhold);
    loop {
        let ctl = *control.borrow_and_update();
        if ctl.paused {
            if control.changed().await.is_err() {
                return Ok(());
            }
            continue;
        }
        let observations = darkside.observations().clone();
        let submitted = observations.snapshot().tx_submitted;
        let due = tokio::select! {
            () = tokio::time::sleep(ctl.tick.wait()) => true,
            () = observations.wait(|c| c.tx_submitted > submitted), if config.instamine => true,
            changed = control.changed() => {
                if changed.is_err() {
                    return Ok(());
                }
                false
            }
        };
        if due {
            darkside.mine_with_time(wall_clock())?;
        }
    }
}

/// A scenario failed: which step, and why.
#[derive(Debug)]
pub struct ScenarioError {
    /// Zero-based index of the failing step, or `None` for start-time
    /// errors.
    pub step: Option<usize>,
    /// What went wrong.
    pub msg: String,
}

impl fmt::Display for ScenarioError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.step {
            Some(step) => write!(f, "scenario failed at step {}: {}", step + 1, self.msg),
            None => write!(f, "scenario failed to start: {}", self.msg),
        }
    }
}

impl std::error::Error for ScenarioError {}

fn start_error(msg: String) -> ScenarioError {
    ScenarioError { step: None, msg }
}

/// Run one scenario to completion: serve swaps, barrier waits, and
/// assertions, fully deterministic. Chains are consumed as they are served.
/// a replaced chain returns to its slot so it can be served again.
pub async fn run_scenario(
    darkside: &Darkside,
    chains: Vec<(String, Chain)>,
    scenario: &Scenario,
) -> Result<(), ScenarioError> {
    // Declared content beyond a chain's tip is a start-time error here: no
    // tick exists to ever reach it.
    for (name, chain) in &chains {
        if chain.has_unrealized_schedule() {
            return Err(start_error(format!(
                "chain {name} declares content beyond its tip; only live mode can reach it"
            )));
        }
    }
    let mut slots: Vec<(String, Option<Chain>)> =
        chains.into_iter().map(|(n, c)| (n, Some(c))).collect();
    let mut serving: Option<String> = None;

    for (index, step) in scenario.steps.iter().enumerate() {
        let fail = |msg: String| ScenarioError {
            step: Some(index),
            msg,
        };
        match step {
            Step::Serve(name) => {
                let slot = slots
                    .iter_mut()
                    .find(|(n, _)| n == name)
                    .ok_or_else(|| fail(format!("chain {name} is not in the world")))?;
                let chain = slot
                    .1
                    .take()
                    .ok_or_else(|| fail(format!("chain {name} is already being served")))?;
                let previous = darkside.serve(chain);
                if let Some(prev_name) = serving.take()
                    && let Some(slot) = slots.iter_mut().find(|(n, _)| *n == prev_name)
                {
                    slot.1 = Some(previous);
                }
                serving = Some(name.clone());
            }
            Step::Wait(barrier) => {
                let observations = darkside.observations().clone();
                match barrier {
                    Barrier::BlockRequested(height) => {
                        let target = *height as u64;
                        observations.wait(|c| c.max_block_requested >= target).await;
                    }
                    Barrier::TxSubmitted => {
                        let baseline = observations.snapshot().tx_submitted;
                        observations.wait(|c| c.tx_submitted > baseline).await;
                    }
                    Barrier::MempoolPolled => {
                        let baseline = observations.snapshot().mempool_polled;
                        observations.wait(|c| c.mempool_polled > baseline).await;
                    }
                    Barrier::UtxosRequested(account) => {
                        let taddr = darkside
                            .with_chain(|chain| {
                                chain
                                    .account(account)
                                    .map(|a| chain.params().encode_taddr(a.taddr()))
                            })
                            .ok_or_else(|| {
                                fail(format!("account {account} is not on the served chain"))
                            })?;
                        let baseline = observations
                            .snapshot()
                            .utxos_requested
                            .get(&taddr)
                            .copied()
                            .unwrap_or(0);
                        observations
                            .wait(|c| {
                                c.utxos_requested.get(&taddr).copied().unwrap_or(0) > baseline
                            })
                            .await;
                    }
                }
            }
            Step::Expect(Expectation::Tip(height)) => {
                let tip = darkside.tip();
                if tip != *height {
                    return Err(fail(format!("expected tip {height}, the tip is {tip}")));
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_cadence_waits_exactly() {
        assert_eq!(Tick::fixed(30).wait(), Duration::from_secs(30));
    }

    #[test]
    fn range_cadence_stays_within_bounds() {
        let tick = Tick::range(15, 90);
        for _ in 0..1000 {
            let wait = tick.wait();
            assert!(wait >= Duration::from_secs(15) && wait <= Duration::from_secs(90));
        }
    }

    #[test]
    fn degenerate_range_collapses_to_the_low_bound() {
        assert_eq!(Tick::range(30, 30).wait(), Duration::from_secs(30));
    }
}
