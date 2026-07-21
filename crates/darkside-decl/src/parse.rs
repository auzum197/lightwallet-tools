//! The line-oriented declaration parser.

use core::fmt;

use darkside_chain::{
    ChainParams, Corruption, FundSpec, Pool, Recipient, Seed, SendSpec, SyntheticNetwork,
};
use zcash_protocol::consensus::{NetworkType, NetworkUpgrade};

use crate::{AccountDecl, Barrier, ChainDecl, Declaration, Event, Expectation, Scenario, Step};

/// A declaration rejected at parse or build time, with the offending line
/// (0 when the failure surfaced while building the world).
#[derive(Debug)]
pub struct DeclError {
    line: usize,
    msg: String,
}

impl DeclError {
    pub(crate) fn new(line: usize, msg: String) -> Self {
        DeclError { line, msg }
    }

    /// The 1-based source line, or 0 for build-phase failures.
    pub fn line(&self) -> usize {
        self.line
    }
}

impl fmt::Display for DeclError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.line == 0 {
            write!(f, "declaration error: {}", self.msg)
        } else {
            write!(f, "declaration error at line {}: {}", self.line, self.msg)
        }
    }
}

impl std::error::Error for DeclError {}

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Word(String),
    Str(String),
}

impl Tok {
    fn word(&self) -> Option<&str> {
        match self {
            Tok::Word(w) => Some(w),
            Tok::Str(_) => None,
        }
    }
}

fn tokenize(line: &str, lineno: usize) -> Result<Vec<Tok>, DeclError> {
    let mut toks = Vec::new();
    let mut chars = line.chars().peekable();
    while let Some(&c) = chars.peek() {
        if c == '#' {
            break;
        }
        if c.is_whitespace() {
            chars.next();
            continue;
        }
        if c == '"' {
            chars.next();
            let mut s = String::new();
            loop {
                match chars.next() {
                    Some('"') => break,
                    Some(ch) => s.push(ch),
                    None => {
                        return Err(DeclError::new(lineno, "unterminated string".into()));
                    }
                }
            }
            toks.push(Tok::Str(s));
        } else {
            let mut w = String::new();
            while let Some(&ch) = chars.peek() {
                if ch.is_whitespace() || ch == '#' || ch == '"' {
                    break;
                }
                w.push(ch);
                chars.next();
            }
            toks.push(Tok::Word(w));
        }
    }
    Ok(toks)
}

fn parse_u64(word: &str, lineno: usize) -> Result<u64, DeclError> {
    let parsed = if let Some(hex_str) = word.strip_prefix("0x") {
        u64::from_str_radix(hex_str, 16)
    } else {
        word.parse()
    };
    parsed.map_err(|_| DeclError::new(lineno, format!("expected a number, got {word:?}")))
}

fn parse_height(word: &str, lineno: usize) -> Result<u32, DeclError> {
    parse_u64(word, lineno)?
        .try_into()
        .map_err(|_| DeclError::new(lineno, format!("height {word} out of range")))
}

fn parse_amount(amount: &str, unit: &str, lineno: usize) -> Result<u64, DeclError> {
    let err = |msg: String| DeclError::new(lineno, msg);
    match unit {
        "ZEC" | "zec" => {
            let (int_part, frac_part) = amount.split_once('.').unwrap_or((amount, ""));
            if frac_part.len() > 8 {
                return Err(err(format!("{amount} ZEC has more than 8 decimal places")));
            }
            let int: u64 = int_part
                .parse()
                .map_err(|_| err(format!("bad amount {amount:?}")))?;
            let frac: u64 = if frac_part.is_empty() {
                0
            } else {
                let padded = format!("{frac_part:0<8}");
                padded
                    .parse()
                    .map_err(|_| err(format!("bad amount {amount:?}")))?
            };
            Ok(int * 100_000_000 + frac)
        }
        "zat" | "zats" => amount
            .parse()
            .map_err(|_| err(format!("bad zatoshi amount {amount:?}"))),
        other => Err(err(format!("unknown unit {other:?}, expected ZEC or zats"))),
    }
}

fn parse_pool(word: &str) -> Option<Pool> {
    match word {
        "sapling" => Some(Pool::Sapling),
        "orchard" => Some(Pool::Orchard),
        "ironwood" => Some(Pool::Ironwood),
        _ => None,
    }
}

fn parse_corruption(word: &str, lineno: usize) -> Result<Corruption, DeclError> {
    match word {
        "commitment" => Ok(Corruption::Commitment),
        "spentness" => Ok(Corruption::Spentness),
        "divergence" => Ok(Corruption::Divergence),
        other => Err(DeclError::new(
            lineno,
            format!(
                "unknown corruption {other:?}: the vocabulary is commitment, spentness, divergence"
            ),
        )),
    }
}

fn parse_recipient(tok: &Tok) -> Recipient {
    match tok {
        Tok::Str(addr) => Recipient::Literal(addr.clone()),
        Tok::Word(w) => match w.strip_suffix(".taddr") {
            Some(name) => Recipient::DeclaredTransparent(name.to_owned()),
            None => Recipient::Declared(w.clone()),
        },
    }
}

const UPGRADES: [(&str, NetworkUpgrade); 10] = [
    ("overwinter", NetworkUpgrade::Overwinter),
    ("sapling", NetworkUpgrade::Sapling),
    ("blossom", NetworkUpgrade::Blossom),
    ("heartwood", NetworkUpgrade::Heartwood),
    ("canopy", NetworkUpgrade::Canopy),
    ("nu5", NetworkUpgrade::Nu5),
    ("nu6", NetworkUpgrade::Nu6),
    ("nu6.1", NetworkUpgrade::Nu6_1),
    ("nu6.2", NetworkUpgrade::Nu6_2),
    ("nu6.3", NetworkUpgrade::Nu6_3),
];

fn parse_activation(toks: &[Tok], lineno: usize) -> Result<[Option<u32>; 10], DeclError> {
    let err = |msg: String| DeclError::new(lineno, msg);
    let mut listed: [Option<u32>; 10] = [None; 10];
    for tok in toks {
        let word = tok
            .word()
            .ok_or_else(|| err("activation entries are name@height".into()))?;
        let (name, height) = word
            .split_once('@')
            .ok_or_else(|| err(format!("activation entry {word:?} is not name@height")))?;
        let canonical = if name == "ironwood" { "nu6.3" } else { name };
        let idx = UPGRADES
            .iter()
            .position(|(n, _)| *n == canonical)
            .ok_or_else(|| err(format!("unknown network upgrade {name:?}")))?;
        listed[idx] = Some(parse_height(height, lineno)?);
    }
    // An unlisted upgrade activates with the next listed one: consensus
    // requires the sequence to be non-decreasing, and this is the minimal
    // consistent completion. Upgrades after the last listed stay off.
    let mut filled = listed;
    let mut next = None;
    for slot in filled.iter_mut().rev() {
        match slot {
            Some(h) => next = Some(*h),
            None => *slot = next,
        }
    }
    let heights: Vec<u32> = filled.iter().flatten().copied().collect();
    if heights.windows(2).any(|w| w[0] > w[1]) {
        return Err(err(
            "activation heights are not non-decreasing in upgrade order".into(),
        ));
    }
    Ok(filled)
}

fn parse_encoding(name: &str, lineno: usize) -> Result<NetworkType, DeclError> {
    match name {
        "main" => Ok(NetworkType::Main),
        "test" => Ok(NetworkType::Test),
        "regtest" => Ok(NetworkType::Regtest),
        other => Err(DeclError::new(
            lineno,
            format!("network {other:?} is not main, test, or regtest"),
        )),
    }
}

enum Ctx {
    Top,
    Chain(ChainDecl),
    Scenario(Scenario),
}

pub(crate) fn parse(src: &str) -> Result<Declaration, DeclError> {
    let mut params = ChainParams::regtest();
    let mut encoding = NetworkType::Regtest;
    let mut activation: Option<[Option<u32>; 10]> = None;
    let mut network_seen = false;
    let mut seed: Option<Seed> = None;
    let mut accounts: Vec<AccountDecl> = Vec::new();
    let mut chains: Vec<ChainDecl> = Vec::new();
    let mut scenarios: Vec<Scenario> = Vec::new();
    let mut ctx = Ctx::Top;

    for (idx, line) in src.lines().enumerate() {
        let lineno = idx + 1;
        let toks = tokenize(line, lineno)?;
        if toks.is_empty() {
            continue;
        }
        let err = |msg: String| DeclError::new(lineno, msg);
        let head = toks[0]
            .word()
            .ok_or_else(|| err("a line cannot start with a string literal".into()))?;

        match &mut ctx {
            Ctx::Top => match head {
                "network" => {
                    let name = toks
                        .get(1)
                        .and_then(Tok::word)
                        .ok_or_else(|| err("network needs a name".into()))?;
                    encoding = parse_encoding(name, lineno)?;
                    network_seen = true;
                }
                "seed" => {
                    let word = toks
                        .get(1)
                        .and_then(Tok::word)
                        .ok_or_else(|| err("seed needs a number".into()))?;
                    seed = Some(Seed::from(parse_u64(word, lineno)?));
                }
                "activation" => {
                    activation = Some(parse_activation(&toks[1..], lineno)?);
                }
                "account" => {
                    let name = toks
                        .get(1)
                        .and_then(Tok::word)
                        .ok_or_else(|| err("account needs a name".into()))?
                        .to_owned();
                    if toks.get(2).and_then(Tok::word) != Some("seed") {
                        return Err(err("account syntax: account <name> seed \"...\"".into()));
                    }
                    let seed_phrase = match toks.get(3) {
                        Some(Tok::Str(s)) => s.clone(),
                        _ => return Err(err("account seed must be a quoted string".into())),
                    };
                    let account_index = match toks.get(4).and_then(Tok::word) {
                        Some("account_index") => {
                            let n = toks
                                .get(5)
                                .and_then(Tok::word)
                                .ok_or_else(|| err("account_index needs a number".into()))?;
                            parse_height(n, lineno)?
                        }
                        Some(other) => {
                            return Err(err(format!("unexpected {other:?} after account seed")));
                        }
                        None => 0,
                    };
                    if accounts.iter().any(|a| a.name == name) {
                        return Err(err(format!("account {name} is declared twice")));
                    }
                    accounts.push(AccountDecl {
                        name,
                        seed_phrase,
                        account_index,
                    });
                }
                "chain" => {
                    let name = toks
                        .get(1)
                        .and_then(Tok::word)
                        .ok_or_else(|| err("chain needs a name".into()))?
                        .to_owned();
                    if chains.iter().any(|c| c.name == name) {
                        return Err(err(format!("chain {name} is declared twice")));
                    }
                    let mut fork = None;
                    let mut rest = 2;
                    if toks.get(2).and_then(Tok::word) == Some("from") {
                        let spec = toks.get(3).and_then(Tok::word).ok_or_else(|| {
                            err("fork syntax: chain <name> from <parent>@<h>".into())
                        })?;
                        let (parent, height) = spec.split_once('@').ok_or_else(|| {
                            err(format!("fork target {spec:?} is not parent@height"))
                        })?;
                        let height = parse_height(height, lineno)?;
                        let parent_decl = chains
                            .iter()
                            .find(|c| c.name == parent)
                            .ok_or_else(|| err(format!("fork parent {parent} is not declared")))?;
                        if height > parent_decl.tip {
                            return Err(err(format!(
                                "fork at {height} is above {parent}'s declared tip {}",
                                parent_decl.tip
                            )));
                        }
                        fork = Some((parent.to_owned(), height));
                        rest = 4;
                    }
                    if toks.get(rest).and_then(Tok::word) != Some("{") {
                        return Err(err("chain block must open with {".into()));
                    }
                    if toks.len() > rest + 1 {
                        return Err(err(
                            "chain content starts on the line after { (one construct per line)"
                                .into(),
                        ));
                    }
                    let tip = fork.as_ref().map(|(_, h)| *h).unwrap_or(0);
                    ctx = Ctx::Chain(ChainDecl {
                        name,
                        fork,
                        tip,
                        events: Vec::new(),
                    });
                }
                "scenario" => {
                    let name = toks
                        .get(1)
                        .and_then(Tok::word)
                        .ok_or_else(|| err("scenario needs a name".into()))?
                        .to_owned();
                    if toks.get(2).and_then(Tok::word) != Some("{") {
                        return Err(err("scenario block must open with {".into()));
                    }
                    if toks.len() > 3 {
                        return Err(err(
                            "scenario content starts on the line after { (one step per line)"
                                .into(),
                        ));
                    }
                    ctx = Ctx::Scenario(Scenario {
                        name,
                        steps: Vec::new(),
                    });
                }
                other => return Err(err(format!("unknown top-level construct {other:?}"))),
            },
            Ctx::Chain(decl) => match head {
                "}" => {
                    let done = std::mem::replace(
                        decl,
                        ChainDecl {
                            name: String::new(),
                            fork: None,
                            tip: 0,
                            events: Vec::new(),
                        },
                    );
                    chains.push(done);
                    ctx = Ctx::Top;
                }
                "blocks" => {
                    let range = toks
                        .get(1)
                        .and_then(Tok::word)
                        .ok_or_else(|| err("blocks needs a range A..B".into()))?;
                    let (a, b) = range
                        .split_once("..")
                        .ok_or_else(|| err(format!("blocks range {range:?} is not A..B")))?;
                    let (a, b) = (parse_height(a, lineno)?, parse_height(b, lineno)?);
                    if a > b {
                        return Err(err(format!("blocks range {a}..{b} is backwards")));
                    }
                    decl.tip = decl.tip.max(b);
                }
                "fund" => {
                    let event = parse_fund(&toks[1..], lineno, &accounts)?;
                    decl.tip = decl.tip.max(event.at);
                    decl.events.push(Event::Fund(event));
                }
                "send" => {
                    let event = parse_send(&toks[1..], lineno, &accounts)?;
                    if let Some(at) = event.at {
                        decl.tip = decl.tip.max(at);
                    }
                    decl.events.push(Event::Send(event));
                }
                other => return Err(err(format!("unknown chain construct {other:?}"))),
            },
            Ctx::Scenario(scenario) => match head {
                "}" => {
                    let done = std::mem::replace(
                        scenario,
                        Scenario {
                            name: String::new(),
                            steps: Vec::new(),
                        },
                    );
                    scenarios.push(done);
                    ctx = Ctx::Top;
                }
                "serve" => {
                    let name = toks
                        .get(1)
                        .and_then(Tok::word)
                        .ok_or_else(|| err("serve needs a chain name".into()))?;
                    scenario.steps.push(Step::Serve(name.to_owned()));
                }
                "wait" => {
                    scenario
                        .steps
                        .push(Step::Wait(parse_barrier(&toks[1..], lineno, &accounts)?));
                }
                "expect" => {
                    let is_tip = toks.get(1).and_then(Tok::word) == Some("tip")
                        && toks.get(2).and_then(Tok::word) == Some("==");
                    if !is_tip {
                        return Err(err("the only expectation is: expect tip == <height>".into()));
                    }
                    let h = toks
                        .get(3)
                        .and_then(Tok::word)
                        .ok_or_else(|| err("expect tip == needs a height".into()))?;
                    scenario
                        .steps
                        .push(Step::Expect(Expectation::Tip(parse_height(h, lineno)?)));
                }
                other => return Err(err(format!("unknown scenario construct {other:?}"))),
            },
        }
    }

    if !matches!(ctx, Ctx::Top) {
        return Err(DeclError::new(src.lines().count(), "unclosed block".into()));
    }
    if !network_seen {
        return Err(DeclError::new(0, "declaration needs a network line".into()));
    }
    // The encoding sets the address prefixes; the activation line, when
    // present, fully specifies the schedule under it, otherwise the encoding's
    // default schedule applies. Built here so order of the network and
    // activation lines does not matter.
    params.network = match activation {
        Some(heights) => SyntheticNetwork::with_encoding(encoding, heights),
        None => SyntheticNetwork::default_for(encoding),
    };
    let seed = seed.ok_or_else(|| DeclError::new(0, "declaration needs a seed line".into()))?;

    let decl = Declaration {
        params,
        seed,
        accounts,
        chains,
        scenarios,
    };
    validate(&decl)?;
    Ok(decl)
}

fn require_declared(
    recipient: &Recipient,
    accounts: &[AccountDecl],
    lineno: usize,
) -> Result<(), DeclError> {
    let name = match recipient {
        Recipient::Declared(n) | Recipient::DeclaredTransparent(n) => n,
        Recipient::Literal(_) | Recipient::LiteralTransparent(_) => return Ok(()),
    };
    if accounts.iter().any(|a| a.name == *name) {
        Ok(())
    } else {
        Err(DeclError::new(
            lineno,
            format!("account {name} is not declared"),
        ))
    }
}

fn parse_fund(
    toks: &[Tok],
    lineno: usize,
    accounts: &[AccountDecl],
) -> Result<FundSpec, DeclError> {
    let err = |msg: String| DeclError::new(lineno, msg);
    let mut it = toks.iter().peekable();
    let recipient = parse_recipient(
        it.next()
            .ok_or_else(|| err("fund needs a recipient".into()))?,
    );
    require_declared(&recipient, accounts, lineno)?;
    let pool = it
        .peek()
        .and_then(|t| t.word())
        .and_then(parse_pool)
        .inspect(|_| {
            it.next();
        });
    let amount = it
        .next()
        .and_then(Tok::word)
        .ok_or_else(|| err("fund needs an amount".into()))?;
    let unit = it
        .next()
        .and_then(Tok::word)
        .ok_or_else(|| err("fund needs a unit (ZEC or zats)".into()))?;
    let zats = parse_amount(amount, unit, lineno)?;
    if it.next().and_then(Tok::word) != Some("at") {
        return Err(err("fund needs: at <height>".into()));
    }
    let at = parse_height(
        it.next()
            .and_then(Tok::word)
            .ok_or_else(|| err("at needs a height".into()))?,
        lineno,
    )?;
    let mut via_coinbase = false;
    let mut corruption = None;
    while let Some(tok) = it.next() {
        match tok.word() {
            Some("via") => {
                if it.next().and_then(Tok::word) != Some("coinbase") {
                    return Err(err("expected: via coinbase".into()));
                }
                via_coinbase = true;
            }
            Some("corrupt") => {
                let word = it
                    .next()
                    .and_then(Tok::word)
                    .ok_or_else(|| err("corrupt needs a word".into()))?;
                corruption = Some(parse_corruption(word, lineno)?);
            }
            _ => return Err(err(format!("unexpected token {tok:?} in fund"))),
        }
    }
    Ok(FundSpec {
        recipient,
        pool,
        zats,
        outputs: 1,
        at,
        via_coinbase,
        corruption,
    })
}

fn parse_send(
    toks: &[Tok],
    lineno: usize,
    accounts: &[AccountDecl],
) -> Result<SendSpec, DeclError> {
    let err = |msg: String| DeclError::new(lineno, msg);
    let mut it = toks.iter().peekable();
    let from = it
        .next()
        .and_then(Tok::word)
        .ok_or_else(|| err("a send sender must be a declared account".into()))?
        .to_owned();
    if !accounts.iter().any(|a| a.name == from) {
        return Err(err(format!("account {from} is not declared")));
    }
    let pool = it
        .peek()
        .and_then(|t| t.word())
        .and_then(parse_pool)
        .inspect(|_| {
            it.next();
        });
    let amount = it
        .next()
        .and_then(Tok::word)
        .ok_or_else(|| err("send needs an amount".into()))?;
    let unit = it
        .next()
        .and_then(Tok::word)
        .ok_or_else(|| err("send needs a unit (ZEC or zats)".into()))?;
    let zats = parse_amount(amount, unit, lineno)?;
    if it.next().and_then(Tok::word) != Some("to") {
        return Err(err("send needs: to <recipient>".into()));
    }
    let recipient = parse_recipient(
        it.next()
            .ok_or_else(|| err("to needs a recipient".into()))?,
    );
    require_declared(&recipient, accounts, lineno)?;

    let mut at = None;
    let mut pending_from = None;
    let mut expiring_at = None;
    let mut corruption = None;
    while let Some(tok) = it.next() {
        match tok.word() {
            Some("at") => at = Some(next_height(&mut it, "at", lineno)?),
            Some("pending") => {
                if it.next().and_then(Tok::word) != Some("from") {
                    return Err(err("expected: pending from <height>".into()));
                }
                pending_from = Some(next_height(&mut it, "pending from", lineno)?);
            }
            Some("expiring") => {
                if it.next().and_then(Tok::word) != Some("at") {
                    return Err(err("expected: expiring at <height>".into()));
                }
                expiring_at = Some(next_height(&mut it, "expiring at", lineno)?);
            }
            Some("corrupt") => {
                let word = it
                    .next()
                    .and_then(Tok::word)
                    .ok_or_else(|| err("corrupt needs a word".into()))?;
                corruption = Some(parse_corruption(word, lineno)?);
            }
            _ => return Err(err(format!("unexpected token {tok:?} in send"))),
        }
    }
    if at.is_none() && expiring_at.is_none() {
        return Err(err(
            "a send needs a mine height (at) or an expiry (expiring at)".into(),
        ));
    }
    if let (Some(p), Some(a)) = (pending_from, at)
        && p >= a
    {
        return Err(err(format!(
            "pending from {p} is not before the mine height {a}"
        )));
    }
    if let (Some(e), Some(p)) = (expiring_at, pending_from)
        && e <= p
    {
        return Err(err(format!(
            "expiring at {e} is not after pending from {p}"
        )));
    }
    Ok(SendSpec {
        from,
        pool,
        zats,
        recipient,
        at,
        pending_from,
        expiring_at,
        corruption,
    })
}

fn next_height<'a, I: Iterator<Item = &'a Tok>>(
    it: &mut I,
    what: &str,
    lineno: usize,
) -> Result<u32, DeclError> {
    it.next()
        .and_then(Tok::word)
        .ok_or_else(|| DeclError::new(lineno, format!("{what} needs a height")))
        .and_then(|w| parse_height(w, lineno))
}

fn parse_barrier(
    toks: &[Tok],
    lineno: usize,
    accounts: &[AccountDecl],
) -> Result<Barrier, DeclError> {
    let err = |msg: String| DeclError::new(lineno, msg);
    let head = toks
        .first()
        .and_then(Tok::word)
        .ok_or_else(|| err("wait needs a barrier".into()))?;
    match head {
        "tx_submitted" => Ok(Barrier::TxSubmitted),
        "mempool_polled" => Ok(Barrier::MempoolPolled),
        "block_requested" => {
            if toks.get(1).and_then(Tok::word) != Some(">=") {
                return Err(err("expected: block_requested >= <height>".into()));
            }
            let h = toks
                .get(2)
                .and_then(Tok::word)
                .ok_or_else(|| err("block_requested >= needs a height".into()))?;
            Ok(Barrier::BlockRequested(parse_height(h, lineno)?))
        }
        "utxos_requested" => {
            if toks.get(1).and_then(Tok::word) != Some("for") {
                return Err(err("expected: utxos_requested for <account>.taddr".into()));
            }
            let target = toks
                .get(2)
                .and_then(Tok::word)
                .and_then(|w| w.strip_suffix(".taddr"))
                .ok_or_else(|| err("expected: utxos_requested for <account>.taddr".into()))?;
            if !accounts.iter().any(|a| a.name == target) {
                return Err(err(format!("account {target} is not declared")));
            }
            Ok(Barrier::UtxosRequested(target.to_owned()))
        }
        other => {
            let hint = if other.contains('.') {
                " (wallet-internal state is unrepresentable: darkside can only see requests)"
            } else {
                ""
            };
            Err(err(format!(
                "unknown barrier {other:?}{hint}: legal barriers are block_requested >= <h>, \
                 tx_submitted, mempool_polled, utxos_requested for <account>.taddr"
            )))
        }
    }
}

fn validate(decl: &Declaration) -> Result<(), DeclError> {
    let err = |msg: String| DeclError::new(0, msg);
    for scenario in &decl.scenarios {
        for step in &scenario.steps {
            if let Step::Serve(name) = step
                && !decl.chains.iter().any(|c| c.name == *name)
            {
                return Err(err(format!(
                    "scenario {} serves undeclared chain {name}",
                    scenario.name
                )));
            }
        }
    }
    // Funding a pool, or sending from one, before its activation height is
    // an authoring error. Literal recipients resolve at build time and are
    // checked there.
    for chain in &decl.chains {
        for event in &chain.events {
            let (pool, height, what) = match event {
                Event::Fund(f) => {
                    let pool = match (&f.recipient, f.pool) {
                        (Recipient::DeclaredTransparent(_), _) => None,
                        (Recipient::LiteralTransparent(_), _) => None,
                        (Recipient::Literal(_), p) => p,
                        (Recipient::Declared(_), p) => Some(p.unwrap_or(Pool::Orchard)),
                    };
                    (pool, Some(f.at), "fund")
                }
                Event::Send(s) => {
                    // Literal recipients resolve to a pool at build time;
                    // named ones default to Orchard, mirroring the chain.
                    let pool = s.pool.or(match &s.recipient {
                        Recipient::Literal(_) | Recipient::LiteralTransparent(_) => None,
                        _ => Some(Pool::Orchard),
                    });
                    (pool, s.at, "send")
                }
            };
            if let (Some(pool), Some(height)) = (pool, height) {
                let nu = match pool {
                    Pool::Sapling => NetworkUpgrade::Sapling,
                    Pool::Orchard => NetworkUpgrade::Nu5,
                    Pool::Ironwood => NetworkUpgrade::Nu6_3,
                };
                if !decl.params.is_active(nu, height) {
                    return Err(err(format!(
                        "chain {}: {what} uses pool {pool:?} at {height}, before its activation",
                        chain.name
                    )));
                }
            }
        }
    }
    Ok(())
}
