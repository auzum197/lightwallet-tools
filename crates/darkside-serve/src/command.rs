//! The command vocabulary and the dispatcher that runs it.
//!
//! One enum, one dispatcher, several frontends. A frontend parses its own
//! syntax into a [`Command`], hands it over, and renders whatever comes
//! back. Nothing in here prints or logs: results and failures are values, so
//! a caller reached over a socket sees everything a caller at a terminal
//! does.

use darkside_chain::{Chain, ChainParams, FundSpec, Receiver, Recipient, Seed};
use serde::{Deserialize, Serialize};
use tokio::sync::watch;
use zcash_protocol::consensus::NetworkUpgrade;

use crate::{Darkside, MinerControl, Tick};

/// The upgrades darkside models, in consensus order, with the names its
/// overrides and status output use. `activation` returns `None` for any not
/// on the served chain, so listing a superset is safe.
const UPGRADES: [(NetworkUpgrade, &str); 10] = [
    (NetworkUpgrade::Overwinter, "overwinter"),
    (NetworkUpgrade::Sapling, "sapling"),
    (NetworkUpgrade::Blossom, "blossom"),
    (NetworkUpgrade::Heartwood, "heartwood"),
    (NetworkUpgrade::Canopy, "canopy"),
    (NetworkUpgrade::Nu5, "nu5"),
    (NetworkUpgrade::Nu6, "nu6"),
    (NetworkUpgrade::Nu6_1, "nu6.1"),
    (NetworkUpgrade::Nu6_2, "nu6.2"),
    (NetworkUpgrade::Nu6_3, "ironwood"),
];

/// One operator instruction.
#[derive(Clone, Debug)]
pub enum Command {
    /// Mine `blocks` blocks immediately.
    Mine {
        /// How many blocks.
        blocks: u32,
    },
    /// Mine forward until the tip reaches `height`.
    MineTo {
        /// Target tip.
        height: u32,
    },
    /// Mine forward to the next scheduled upgrade above the tip.
    NextUpgrade,
    /// Mine forward to a named upgrade's activation height.
    Advance {
        /// Canonical upgrade name, as [`upgrade_named`] resolves it.
        upgrade: String,
    },
    /// Fabricate value to an address and mine the block carrying it.
    Fund {
        /// Any address valid for the served network, declared or not.
        address: String,
        /// Total value in zatoshis, split across `receivers`.
        zats: u64,
        /// Which receivers of the address to pay, in the order given.
        /// `None` picks the most recent one the address carries and the
        /// chain has active.
        receivers: Option<Vec<Receiver>>,
    },
    /// Halt auto-mining.
    Pause,
    /// Restart auto-mining.
    Resume,
    /// Retime auto-mining.
    SetTick {
        /// New cadence.
        tick: Tick,
    },
    /// Fork `depth` blocks back and serve the fork.
    Reorg {
        /// How far back the fork point sits from the tip.
        depth: u32,
    },
    /// Rebuild the boot chain and serve it.
    Reset,
    /// Accept wallet submissions but hold them out of blocks.
    Withhold {
        /// Whether withholding is on.
        on: bool,
    },
    /// Report tip, pools, upgrades, mempool, and miner state.
    Status,
}

/// What a command produced.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Outcome {
    /// One-line human rendering, what a terminal frontend prints.
    pub summary: String,
    /// Non-fatal problems. A skipped receiver lands here rather than
    /// failing the command, so a caller who never reads the log still
    /// learns that part of the request was dropped.
    pub warnings: Vec<String>,
    /// The structured result.
    pub detail: Detail,
}

/// The structured half of an [`Outcome`], tagged by command kind.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Detail {
    /// Blocks were mined.
    Mined {
        /// Blocks added.
        blocks: u32,
        /// Tip after mining.
        tip: u32,
    },
    /// Value was fabricated.
    Funded {
        /// Height the funding transactions were mined at.
        height: u32,
        /// Tip after mining.
        tip: u32,
        /// What each paid receiver got.
        outputs: Vec<FundedOutput>,
    },
    /// The auto-miner's state changed, or was reported.
    Miner {
        /// Whether auto-mining is halted.
        paused: bool,
        /// Cadence, rendered.
        tick: String,
    },
    /// A block was mined at a height above the tip, leaving the span below
    /// it unmined and computed on demand.
    Jumped {
        /// Tip before the jump.
        from: u32,
        /// Tip after.
        tip: u32,
    },
    /// The served chain was replaced by a fork.
    Reorged {
        /// Blocks rolled back.
        depth: u32,
        /// Tip after the swap.
        tip: u32,
    },
    /// The served chain was rebuilt from the boot recipe.
    Reset {
        /// Tip after the rebuild.
        tip: u32,
    },
    /// The withhold knob changed.
    Withhold {
        /// Whether withholding is on.
        on: bool,
    },
    /// A status report.
    Status(Status),
}

/// One receiver paid by a `fund`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FundedOutput {
    /// Receiver letter: `t`, `s`, `o`, or `i`.
    pub receiver: char,
    /// Value in zatoshis.
    pub zats: u64,
}

/// The served chain at a glance.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Status {
    /// Tip height.
    pub tip: u32,
    /// Tip block time.
    pub time: u32,
    /// Tip block hash.
    pub hash: String,
    /// Chain name reported by `GetLightdInfo`.
    pub chain: String,
    /// Pools active at the tip, by receiver letter.
    pub pools: Vec<char>,
    /// Next upgrade above the tip, if any.
    pub next_upgrade: Option<NextUpgrade>,
    /// Transactions waiting in the mempool.
    pub mempool: usize,
    /// Whether auto-mining is halted.
    pub paused: bool,
    /// Cadence, rendered.
    pub tick: String,
}

/// An upgrade scheduled above the tip.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NextUpgrade {
    /// Upgrade name as the override syntax spells it.
    pub name: String,
    /// Activation height.
    pub height: u32,
}

/// Why a command failed.
#[derive(Debug)]
pub enum Error {
    /// The request is malformed, or asks for something this chain cannot
    /// do.
    Request(String),
    /// The chain rejected it.
    Chain(darkside_chain::Error),
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Error::Request(msg) => write!(f, "{msg}"),
            Error::Chain(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<darkside_chain::Error> for Error {
    fn from(e: darkside_chain::Error) -> Self {
        Error::Chain(e)
    }
}

/// Result alias over [`Error`].
pub type Result<T> = core::result::Result<T, Error>;

/// The seed and schedule a `reset` rebuilds from. Rebuilding from the same
/// pair yields a byte-identical chain, so a reset returns the world to its
/// exact starting point.
#[derive(Clone, Debug)]
pub struct BootChain {
    params: ChainParams,
    seed: Seed,
}

impl BootChain {
    /// The recipe for a chain booted from `params` with `seed`.
    pub fn new(params: ChainParams, seed: Seed) -> Self {
        BootChain { params, seed }
    }

    /// Build a fresh chain from the recipe.
    pub fn build(&self) -> Chain {
        Chain::new(self.params.clone(), self.seed)
    }
}

/// Runs [`Command`]s against a served chain.
///
/// Clones drive the same chain, so every frontend holds its own handle.
#[derive(Clone)]
pub struct Dispatcher {
    darkside: Darkside,
    miner: watch::Sender<MinerControl>,
    boot: BootChain,
    max_blocks: u32,
}

impl Dispatcher {
    /// A dispatcher over `darkside`, editing the live miner through
    /// `miner`, rebuilding from `boot` on reset, and refusing any single
    /// command that would mine more than `max_blocks`.
    pub fn new(
        darkside: Darkside,
        miner: watch::Sender<MinerControl>,
        boot: BootChain,
        max_blocks: u32,
    ) -> Self {
        Dispatcher {
            darkside,
            miner,
            boot,
            max_blocks,
        }
    }

    /// The served chain, for frontends that need it directly.
    pub fn darkside(&self) -> &Darkside {
        &self.darkside
    }

    /// Run one command.
    pub fn run(&self, command: Command) -> Result<Outcome> {
        match command {
            Command::Mine { blocks } => self.mine(blocks),
            Command::MineTo { height } => {
                let tip = self.darkside.tip();
                if height <= tip {
                    return Err(Error::Request(format!(
                        "height {height} is not above the tip {tip}"
                    )));
                }
                self.mine(height - tip)
            }
            Command::NextUpgrade => {
                let tip = self.darkside.tip();
                let next = self
                    .darkside
                    .with_chain(|c| next_upgrade_above(c.params(), tip));
                match next {
                    Some((_, height)) => self.mine(height - tip),
                    None => Err(Error::Request(format!(
                        "no upgrade is scheduled above the tip {tip}"
                    ))),
                }
            }
            Command::Advance { upgrade } => self.advance(&upgrade),
            Command::Fund {
                address,
                zats,
                receivers,
            } => self.fund(&address, zats, receivers.as_deref()),
            Command::Pause => Ok(self.set_miner(|m| m.paused = true)),
            Command::Resume => Ok(self.set_miner(|m| m.paused = false)),
            Command::SetTick { tick } => Ok(self.set_miner(|m| m.tick = tick)),
            Command::Reorg { depth } => self.reorg(depth),
            Command::Reset => {
                let fresh = self.boot.build();
                let tip = fresh.tip_height();
                self.darkside.serve(fresh);
                Ok(Outcome {
                    summary: format!("reset to the boot chain, tip now {tip}"),
                    warnings: Vec::new(),
                    detail: Detail::Reset { tip },
                })
            }
            Command::Withhold { on } => {
                self.darkside.set_withhold(on);
                let summary = if on {
                    "withhold on: submissions accepted, held out of blocks".to_owned()
                } else {
                    "withhold off: submissions mined into the next block".to_owned()
                };
                Ok(Outcome {
                    summary,
                    warnings: Vec::new(),
                    detail: Detail::Withhold { on },
                })
            }
            Command::Status => Ok(self.status()),
        }
    }

    fn mine(&self, blocks: u32) -> Result<Outcome> {
        if blocks > self.max_blocks {
            return Err(Error::Request(format!(
                "{blocks} blocks exceeds the {} allowed in one command",
                self.max_blocks
            )));
        }
        let mut tip = self.darkside.tip();
        for _ in 0..blocks {
            tip = self.darkside.mine_with_time(wall_clock())?;
        }
        Ok(Outcome {
            summary: format!("mined {blocks} block(s), tip now {tip}"),
            warnings: Vec::new(),
            detail: Detail::Mined { blocks, tip },
        })
    }

    /// Mine to `upgrade`'s activation. Refuses an upgrade the chain never
    /// schedules and one that already activated, since neither can be mined
    /// toward.
    fn advance(&self, upgrade: &str) -> Result<Outcome> {
        let (nu, name) = upgrade_named(upgrade)
            .ok_or_else(|| Error::Request(format!("unknown upgrade '{upgrade}'")))?;
        let tip = self.darkside.tip();
        let activation = self
            .darkside
            .with_chain(|c| c.params().activation(nu))
            .ok_or_else(|| Error::Request(format!("{name} is not on this chain's schedule")))?;
        if activation <= tip {
            return Err(Error::Request(format!(
                "{name} activated at {activation} and the tip is already {tip}"
            )));
        }

        // A span too long to mine is unmined height, not work. Jumping mines
        // the target block directly and leaves the span below it empty and
        // computed, so everything already on the chain keeps its height.
        let span = activation - tip;
        if span > self.max_blocks {
            let tip_now = self.darkside.jump_to(activation, wall_clock())?;
            return Ok(Outcome {
                summary: format!("jumped to {name} activation at {tip_now}"),
                warnings: Vec::new(),
                detail: Detail::Jumped {
                    from: tip,
                    tip: tip_now,
                },
            });
        }

        let mut outcome = self.mine(span)?;
        outcome.summary = format!("mined to {name} activation at {activation}");
        Ok(outcome)
    }

    fn reorg(&self, depth: u32) -> Result<Outcome> {
        let fork = self
            .darkside
            .with_chain(|c| c.fork_at(c.tip_height().saturating_sub(depth)))?;
        let tip = fork.tip_height();
        self.darkside.serve(fork);
        Ok(Outcome {
            summary: format!("reorged {depth} block(s) back, tip now {tip}"),
            warnings: Vec::new(),
            detail: Detail::Reorged { depth, tip },
        })
    }

    fn fund(&self, address: &str, zats: u64, requested: Option<&[Receiver]>) -> Result<Outcome> {
        let mut warnings = Vec::new();

        // Scheduling runs under one write lock so the auto-miner cannot
        // advance the tip between choosing the height and using it.
        let (height, outputs) = self.darkside.with_chain_mut(|chain| {
            let at = chain.tip_height() + 1;
            let carried = chain.receivers(address)?;
            let targets = match requested {
                // A bare fund pays the newest receiver the address carries
                // and the chain has active. `receivers` returns them newest
                // first, so the first live one wins.
                None => carried
                    .iter()
                    .copied()
                    .find(|r| pool_live(chain, *r, at))
                    .into_iter()
                    .collect(),
                Some(list) => list
                    .iter()
                    .copied()
                    .filter(|r| {
                        if !carried.contains(r) {
                            warnings.push(format!(
                                "{address} carries no {} receiver, skipped",
                                r.letter()
                            ));
                            return false;
                        }
                        if !pool_live(chain, *r, at) {
                            warnings.push(format!(
                                "{} is not active at height {at}, skipped",
                                r.letter()
                            ));
                            return false;
                        }
                        true
                    })
                    .collect::<Vec<_>>(),
            };
            if targets.is_empty() {
                return Err(Error::Request(format!(
                    "nothing to fund: {address} has no requested receiver active at height {at}"
                )));
            }

            // Equal split by integer division. The remainder goes to the
            // first receiver as typed, so ordering the letters differently
            // moves at most n-1 zatoshis.
            let share = zats / targets.len() as u64;
            let remainder = zats % targets.len() as u64;
            let mut outputs = Vec::with_capacity(targets.len());
            for (index, receiver) in targets.iter().enumerate() {
                let amount = if index == 0 { share + remainder } else { share };
                let recipient = match receiver {
                    Receiver::Transparent => Recipient::LiteralTransparent(address.to_owned()),
                    _ => Recipient::Literal(address.to_owned()),
                };
                chain.fund(FundSpec {
                    recipient,
                    pool: receiver.pool(),
                    zats: amount,
                    outputs: 1,
                    at,
                    via_coinbase: false,
                    corruption: None,
                })?;
                outputs.push(FundedOutput {
                    receiver: receiver.letter(),
                    zats: amount,
                });
            }
            Ok((at, outputs))
        })?;

        let tip = self.darkside.mine_with_time(wall_clock())?;
        let rendered = outputs
            .iter()
            .map(|o| format!("{} {}", zec(o.zats), o.receiver))
            .collect::<Vec<_>>()
            .join(", ");
        Ok(Outcome {
            summary: format!("funded {address} at height {height}: {rendered}, tip now {tip}"),
            warnings,
            detail: Detail::Funded {
                height,
                tip,
                outputs,
            },
        })
    }

    fn set_miner(&self, edit: impl FnOnce(&mut MinerControl)) -> Outcome {
        self.miner.send_modify(edit);
        let state = *self.miner.borrow();
        let summary = if state.paused {
            "auto-mining paused".to_owned()
        } else {
            format!("auto-mining on, block time {}", state.tick)
        };
        Outcome {
            summary,
            warnings: Vec::new(),
            detail: Detail::Miner {
                paused: state.paused,
                tick: state.tick.to_string(),
            },
        }
    }

    fn status(&self) -> Outcome {
        let miner = *self.miner.borrow();
        let status = self.darkside.with_chain(|c| {
            let tip = c.tip_height();
            let params = c.params();
            let (time, hash) = c
                .block(tip)
                .map(|b| (b.time, b.hash.to_string()))
                .unwrap_or((0, "-".to_owned()));
            let mut pools = vec!['t'];
            if params.is_active(NetworkUpgrade::Sapling, tip) {
                pools.push('s');
            }
            if params.is_active(NetworkUpgrade::Nu5, tip) {
                pools.push('o');
            }
            if params.ironwood_active(tip) {
                pools.push('i');
            }
            Status {
                tip,
                time,
                hash,
                chain: params.chain_name.clone(),
                pools,
                next_upgrade: next_upgrade_above(params, tip).map(|(name, height)| NextUpgrade {
                    name: name.to_owned(),
                    height,
                }),
                mempool: c.mempool().len(),
                paused: miner.paused,
                tick: miner.tick.to_string(),
            }
        });
        Outcome {
            summary: format!(
                "tip {} @ {}  pools {}  mempool {}  auto-mining {}",
                status.tip,
                status.time,
                status.pools.iter().collect::<String>(),
                status.mempool,
                if status.paused {
                    "paused"
                } else {
                    status.tick.as_str()
                },
            ),
            warnings: Vec::new(),
            detail: Detail::Status(status),
        }
    }
}

/// Whether the pool `receiver` pays into is active at `height`. Transparent
/// has no activation.
fn pool_live(chain: &Chain, receiver: Receiver, height: u32) -> bool {
    let params = chain.params();
    match receiver {
        Receiver::Transparent => true,
        Receiver::Sapling => params.is_active(NetworkUpgrade::Sapling, height),
        Receiver::Orchard => params.is_active(NetworkUpgrade::Nu5, height),
        Receiver::Ironwood => params.ironwood_active(height),
    }
}

/// Every upgrade name `advance` accepts, comma separated, for error text
/// and help output.
pub fn upgrade_names() -> String {
    UPGRADES
        .iter()
        .map(|(_, name)| *name)
        .collect::<Vec<_>>()
        .join(", ")
}

/// The upgrade a name refers to, with its canonical spelling.
///
/// Case-insensitive, and `nu6.3` resolves to ironwood so the name used by
/// `--activation-heights` works here too.
pub fn upgrade_named(name: &str) -> Option<(NetworkUpgrade, &'static str)> {
    let wanted = name.trim().to_ascii_lowercase();
    let wanted = match wanted.as_str() {
        "nu6.3" => "ironwood",
        other => other,
    };
    UPGRADES
        .iter()
        .find(|(_, known)| *known == wanted)
        .map(|(nu, known)| (*nu, *known))
}

/// The nearest upgrade whose activation is strictly above `height`.
fn next_upgrade_above(params: &ChainParams, height: u32) -> Option<(&'static str, u32)> {
    UPGRADES
        .iter()
        .filter_map(|(nu, name)| params.activation(*nu).map(|h| (*name, h)))
        .filter(|(_, h)| *h > height)
        .min_by_key(|(_, h)| *h)
}

fn wall_clock() -> u32 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as u32)
        .unwrap_or_default()
}

/// Render zatoshis as a ZEC amount, trailing zeros trimmed.
fn zec(zats: u64) -> String {
    let whole = zats / 100_000_000;
    let frac = zats % 100_000_000;
    if frac == 0 {
        format!("{whole} ZEC")
    } else {
        let frac = format!("{frac:08}");
        format!("{whole}.{} ZEC", frac.trim_end_matches('0'))
    }
}

/// Parse one line of the operator syntax into a command.
///
/// Lives beside the vocabulary rather than in a frontend so every terminal
/// that speaks darkside speaks the same words. Blank lines yield `None`.
/// `help` and `quit` are absent on purpose: they act on the frontend, not on
/// the chain.
pub fn parse_line(line: &str) -> Result<Option<Command>> {
    let mut parts = line.split_whitespace();
    let Some(verb) = parts.next() else {
        return Ok(None);
    };
    let missing =
        |what: &str, usage: &str| Error::Request(format!("{verb}: {what}, usage {usage}"));
    let number = |arg: Option<&str>, what: &str, usage: &str| -> Result<u32> {
        arg.ok_or_else(|| missing(what, usage))?
            .parse()
            .map_err(|_| Error::Request(format!("{verb}: {what} must be a whole number")))
    };

    let command = match verb {
        "mine" => Command::Mine {
            blocks: match parts.next() {
                None => 1,
                Some(n) => n
                    .parse()
                    .map_err(|_| Error::Request("mine: N must be a whole number".into()))?,
            },
        },
        "to" => Command::MineTo {
            height: number(parts.next(), "a target height", "'to <height>'")?,
        },
        "next" => Command::NextUpgrade,
        "advance" => {
            let label = parts
                .next()
                .ok_or_else(|| missing("an upgrade name", "'advance <upgrade>'"))?;
            // Resolved here so a typo fails at the prompt instead of after a
            // round trip.
            let (_, canonical) = upgrade_named(label).ok_or_else(|| {
                Error::Request(format!(
                    "advance: unknown upgrade '{label}', try one of {}",
                    upgrade_names()
                ))
            })?;
            Command::Advance {
                upgrade: canonical.to_owned(),
            }
        }
        "fund" => {
            let usage = "'fund <address> <zec> [tsoi]'";
            let address = parts.next().ok_or_else(|| missing("an address", usage))?;
            let zec = parts.next().ok_or_else(|| missing("an amount", usage))?;
            Command::Fund {
                address: address.to_owned(),
                zats: parse_zec(zec)?,
                receivers: parts.next().map(parse_receivers).transpose()?,
            }
        }
        "pause" => Command::Pause,
        "resume" => Command::Resume,
        "tick" => {
            let spec = parts
                .next()
                .ok_or_else(|| missing("a cadence", "'tick <seconds|low..high>'"))?;
            Command::SetTick {
                tick: spec.parse().map_err(Error::Request)?,
            }
        }
        "reorg" => Command::Reorg {
            depth: number(parts.next(), "a depth", "'reorg <depth>'")?,
        },
        "reset" => Command::Reset,
        "withhold" => match parts.next() {
            Some("on") => Command::Withhold { on: true },
            Some("off") => Command::Withhold { on: false },
            _ => return Err(missing("on or off", "'withhold on|off'")),
        },
        "status" | "st" => Command::Status,
        other => {
            return Err(Error::Request(format!(
                "unknown command '{other}', try 'help'"
            )));
        }
    };
    Ok(Some(command))
}

/// Parse a receiver set: a string of the letters `t`, `s`, `o`, `i` in the
/// order the caller wants them paid. Rejects unknown and repeated letters,
/// since both mean the caller expected something the split will not do.
pub fn parse_receivers(spec: &str) -> Result<Vec<Receiver>> {
    let mut out: Vec<Receiver> = Vec::new();
    for c in spec.chars() {
        let receiver = Receiver::from_letter(c).ok_or_else(|| {
            Error::Request(format!("unknown receiver '{c}', expected t, s, o, or i"))
        })?;
        if out.contains(&receiver) {
            return Err(Error::Request(format!("receiver '{c}' listed twice")));
        }
        out.push(receiver);
    }
    if out.is_empty() {
        return Err(Error::Request(
            "empty receiver set, expected some of t, s, o, i".into(),
        ));
    }
    Ok(out)
}

/// Parse a ZEC amount (decimal, up to 8 fractional places) into zatoshis,
/// without floating point.
pub fn parse_zec(s: &str) -> Result<u64> {
    let bad = || Error::Request(format!("invalid ZEC amount {s:?}"));
    let (whole, frac) = s.split_once('.').unwrap_or((s, ""));
    if frac.len() > 8 || !frac.chars().all(|c| c.is_ascii_digit()) {
        return Err(bad());
    }
    let whole: u64 = if whole.is_empty() {
        0
    } else {
        whole.parse().map_err(|_| bad())?
    };
    let frac: u64 = format!("{frac:0<8}").parse().map_err(|_| bad())?;
    whole
        .checked_mul(100_000_000)
        .and_then(|z| z.checked_add(frac))
        .ok_or_else(bad)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_blank_line_is_not_a_command() {
        assert!(parse_line("").unwrap().is_none());
        assert!(parse_line("   ").unwrap().is_none());
    }

    #[test]
    fn fund_takes_an_address_an_amount_and_an_optional_receiver_set() {
        let Some(Command::Fund {
            address,
            zats,
            receivers,
        }) = parse_line("fund u1abc 1.5 os").unwrap()
        else {
            panic!("fund did not parse as a fund");
        };
        assert_eq!(address, "u1abc");
        assert_eq!(zats, 150_000_000);
        assert_eq!(receivers, Some(vec![Receiver::Orchard, Receiver::Sapling]));

        let Some(Command::Fund { receivers, .. }) = parse_line("fund u1abc 1.5").unwrap() else {
            panic!("a bare fund did not parse as a fund");
        };
        assert_eq!(receivers, None);
    }

    #[test]
    fn mine_defaults_to_one_block() {
        assert!(matches!(
            parse_line("mine").unwrap(),
            Some(Command::Mine { blocks: 1 })
        ));
        assert!(matches!(
            parse_line("mine 7").unwrap(),
            Some(Command::Mine { blocks: 7 })
        ));
    }

    #[test]
    fn advance_resolves_upgrade_names_at_the_prompt() {
        let Some(Command::Advance { upgrade }) = parse_line("advance ironwood").unwrap() else {
            panic!("advance did not parse as an advance");
        };
        assert_eq!(upgrade, "ironwood");

        // `nu6.3` is what --activation-heights calls it, so it works here
        // too, and case does not matter.
        for spelling in ["nu6.3", "NU6.3", "Ironwood"] {
            let Some(Command::Advance { upgrade }) =
                parse_line(&format!("advance {spelling}")).unwrap()
            else {
                panic!("{spelling} did not parse as an advance");
            };
            assert_eq!(upgrade, "ironwood", "{spelling} resolved wrong");
        }
    }

    #[test]
    fn advance_rejects_a_name_no_upgrade_answers_to() {
        assert!(parse_line("advance").is_err());
        assert!(parse_line("advance banana").is_err());
    }

    #[test]
    fn incomplete_and_unknown_commands_are_rejected() {
        assert!(parse_line("fund u1abc").is_err());
        assert!(parse_line("to").is_err());
        assert!(parse_line("withhold maybe").is_err());
        assert!(parse_line("mine banana").is_err());
        assert!(parse_line("wat").is_err());
    }

    #[test]
    fn a_cadence_parses_back_from_how_it_is_printed() {
        // The client sends `Tick::to_string()` over the wire, so the parser
        // has to accept its own output.
        for tick in [Tick::fixed(30), Tick::range(5, 90)] {
            assert_eq!(
                tick.to_string().parse::<Tick>().expect("round trip"),
                tick,
                "{tick} did not survive a round trip"
            );
        }
    }

    #[test]
    fn receiver_set_keeps_the_order_it_was_typed() {
        assert_eq!(
            parse_receivers("so").unwrap(),
            vec![Receiver::Sapling, Receiver::Orchard]
        );
        assert_eq!(
            parse_receivers("os").unwrap(),
            vec![Receiver::Orchard, Receiver::Sapling]
        );
    }

    #[test]
    fn receiver_set_rejects_unknown_and_repeated_letters() {
        assert!(parse_receivers("x").is_err());
        assert!(parse_receivers("oo").is_err());
        assert!(parse_receivers("").is_err());
    }

    #[test]
    fn zec_parses_without_floating_point() {
        assert_eq!(parse_zec("12").unwrap(), 1_200_000_000);
        assert_eq!(parse_zec("0.00000001").unwrap(), 1);
        assert_eq!(parse_zec("1.5").unwrap(), 150_000_000);
        assert!(parse_zec("1.123456789").is_err());
        assert!(parse_zec("banana").is_err());
    }
}
