//! Chain health as a deterministic function of block cadence and the gap since
//! the last block. Evaluated every render tick off cached inputs, so `stalled`
//! fires as wall-clock time passes even when no block (and so no update) arrives.

use std::collections::VecDeque;

/// One-word chain health, in precedence order: `syncing` outranks `stalled` so a
/// sync burst's pauses don't read as a stall.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Health {
    Healthy,
    Syncing,
    Stalled,
}

/// Default target block spacing, seconds. Canonical main/test are 75s
/// post-Blossom; an operator on a featurenet with different spacing passes
/// `--target-spacing`. The explorer always tails the tip, so pre-Blossom 150s
/// never applies.
pub const DEFAULT_TARGET_SPACING: u64 = 75;

/// Interblock intervals kept for the cadence median.
const CADENCE_WINDOW: usize = 10;

/// The inputs `evaluate` reads, all cached on the `App` and refreshed by
/// updates: block times feed cadence and the gap, `GetLightdInfo` feeds the
/// height prover.
pub struct Inputs<'a> {
    /// Unix time of the most recent block, or `None` before any block is seen.
    pub last_block_time: Option<u32>,
    /// Indexer-reported synced height.
    pub block_height: u64,
    /// Indexer-reported estimated chain height (0 when unknown).
    pub estimated_height: u64,
    /// Recent block times, newest last; intervals are their successive diffs.
    pub block_times: &'a VecDeque<u32>,
    /// Target block spacing in seconds.
    pub target: u64,
}

/// The median of the last `CADENCE_WINDOW` interblock intervals, or `None` with
/// fewer than two blocks. Median over mean: intervals are exponential (Poisson
/// block production), so one long gap drags the mean.
fn cadence(block_times: &VecDeque<u32>) -> Option<u64> {
    let mut intervals: Vec<u64> = block_times
        .iter()
        .zip(block_times.iter().skip(1))
        .map(|(a, b)| b.saturating_sub(*a) as u64)
        .collect();
    if intervals.is_empty() {
        return None;
    }
    let start = intervals.len().saturating_sub(CADENCE_WINDOW);
    let mut window = intervals.split_off(start);
    window.sort_unstable();
    Some(window[window.len() / 2])
}

/// Classify chain health. `now` is Unix seconds.
pub fn evaluate(i: &Inputs, now: u64) -> Health {
    let height_syncing = i.estimated_height > 0 && i.estimated_height > i.block_height + 2;
    let cadence_syncing = cadence(i.block_times).is_some_and(|c| c < i.target / 3);
    if height_syncing || cadence_syncing {
        return Health::Syncing;
    }
    if let Some(t) = i.last_block_time
        && now.saturating_sub(t as u64) > 4 * i.target
    {
        return Health::Stalled;
    }
    Health::Healthy
}
