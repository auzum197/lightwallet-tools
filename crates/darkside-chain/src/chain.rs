//! The chain aggregate: blocks, derived state, the schedule, and the
//! mempool. Chains are values. A reorg is serving a different chain that
//! shares a prefix.

use std::collections::HashMap;

use rand::RngCore;
use sapling_crypto::note_encryption::{SaplingDomain, Zip212Enforcement};
use zcash_note_encryption::{try_note_decryption, try_output_recovery_with_ovk};
use zcash_primitives::block::BlockHash;
use zcash_primitives::transaction::Transaction;
use zcash_protocol::TxId;
use zcash_protocol::consensus::{BranchId, NetworkUpgrade, OrchardProtocolRevision};
use zcash_protocol::value::Zatoshis;
use zcash_transparent::address::TransparentAddress;
use zcash_transparent::bundle::OutPoint;

use orchard::bundle::BundleVersion;
use orchard::note_encryption::{IronwoodDomain, OrchardDomain};
use orchard::tree::MerkleHashOrchard;
use sapling_crypto::Node as SaplingNode;

use crate::account::Account;
use crate::block::{Block, Corruption, MinedTx, block_hash};
use crate::error::{Error, Result};
use crate::fabricate::{self, OrchardOut, SaplingOut, SpendInput};
use crate::mempool::{Eviction, PendingState, PendingTx, TxSource};
use crate::notes::{NoteRecord, OutgoingPayment, Pool};
use crate::params::ChainParams;
use crate::scan::ScanKeys;
use crate::seed::Seed;
use crate::transparent::{Utxo, UtxoSet};
use crate::tree::{IronwoodTree, OrchardTree, PoolTree, SaplingTree, SubtreeRoot};

/// Recipient of a `fund` or `send`.
#[derive(Clone, Debug)]
pub enum Recipient {
    /// A declared account's shielded receiver.
    Declared(String),
    /// A declared account's transparent address.
    DeclaredTransparent(String),
    /// A literal address string: unified, Sapling, or transparent. The
    /// recipient needs no declaration.
    Literal(String),
    /// A literal address string, paid at its transparent receiver. Needed
    /// because [`Recipient::Literal`] prefers a unified address's shielded
    /// receivers and only falls through to transparent when none is usable.
    LiteralTransparent(String),
}

/// Which receiver of an address a payment targets.
///
/// A different axis from [`Pool`]. Transparent is a receiver and not a
/// shielded pool, and Orchard and Ironwood share one receiver inside a
/// unified address, told apart by whether NU6.3 is active at the paying
/// height.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Receiver {
    /// Pay the Orchard receiver, as an Ironwood note.
    Ironwood,
    /// Pay the Orchard receiver, as an Orchard note.
    Orchard,
    /// Pay the Sapling receiver.
    Sapling,
    /// Pay the transparent receiver.
    Transparent,
}

impl Receiver {
    /// The shielded pool this receiver is paid in, or `None` for
    /// transparent.
    pub fn pool(self) -> Option<Pool> {
        match self {
            Receiver::Ironwood => Some(Pool::Ironwood),
            Receiver::Orchard => Some(Pool::Orchard),
            Receiver::Sapling => Some(Pool::Sapling),
            Receiver::Transparent => None,
        }
    }

    /// The single letter that names this receiver in a receiver set.
    pub fn letter(self) -> char {
        match self {
            Receiver::Ironwood => 'i',
            Receiver::Orchard => 'o',
            Receiver::Sapling => 's',
            Receiver::Transparent => 't',
        }
    }

    /// The receiver a letter names, or `None` for an unknown letter.
    pub fn from_letter(c: char) -> Option<Self> {
        match c {
            'i' => Some(Receiver::Ironwood),
            'o' => Some(Receiver::Orchard),
            's' => Some(Receiver::Sapling),
            't' => Some(Receiver::Transparent),
            _ => None,
        }
    }
}

/// A planned `fund`: value appearing from nowhere.
#[derive(Clone, Debug)]
pub struct FundSpec {
    /// Who gets paid.
    pub recipient: Recipient,
    /// Shielded pool override. Defaults to Orchard for shielded
    /// recipients. Ignored for transparent ones.
    pub pool: Option<Pool>,
    /// Value in zatoshis of each output.
    pub zats: u64,
    /// How many identical outputs the funding transaction carries, each worth
    /// `zats`. One output is the usual case. A larger count mints that many
    /// notes (or UTXOs) in a single transaction.
    pub outputs: u32,
    /// Height the funding transaction is mined at.
    pub at: u32,
    /// Mine at index 0 with a real null-prevout input, so wallets apply
    /// 100-block maturity. Requires a transparent recipient.
    pub via_coinbase: bool,
    /// Corruption applied to the funding output.
    pub corruption: Option<Corruption>,
}

/// A planned `send`: a darkside-authored spend of a declared account's
/// notes.
#[derive(Clone, Debug)]
pub struct SendSpec {
    /// Declared account whose notes are spent.
    pub from: String,
    /// Source pool. Defaults to the recipient's pool, Orchard when the
    /// recipient is transparent or unspecified.
    pub pool: Option<Pool>,
    /// Value in zatoshis. The fee is zero by design.
    pub zats: u64,
    /// Who gets paid.
    pub recipient: Recipient,
    /// Height the transaction is mined at. `None` means never mined,
    /// which requires `expiring_at`.
    pub at: Option<u32>,
    /// Height at which the transaction enters the mempool. Defaults to
    /// one block before `at`.
    pub pending_from: Option<u32>,
    /// Height after which an unmined transaction is evicted.
    pub expiring_at: Option<u32>,
    /// Corruption applied to the transaction.
    pub corruption: Option<Corruption>,
}

#[derive(Clone)]
enum ResolvedRecipient {
    Sapling(Box<sapling_crypto::PaymentAddress>),
    Orchard(orchard::Address, Pool),
    Transparent(TransparentAddress),
}

#[derive(Clone)]
enum EventKind {
    Fund {
        recipient: ResolvedRecipient,
        zats: u64,
        outputs: u32,
        via_coinbase: bool,
        corruption: Option<Corruption>,
    },
    Send {
        from: usize,
        pool: Pool,
        zats: u64,
        recipient: ResolvedRecipient,
        corruption: Option<Corruption>,
    },
}

#[derive(Clone)]
struct ScheduledEvent {
    pending_from: u32,
    mine_at: Option<u32>,
    expiring_at: Option<u32>,
    kind: EventKind,
    done: bool,
}

/// Serialized note-commitment tree states for one height, in the legacy
/// zcashd format lightwalletd serves.
pub struct TreeStates {
    /// Sapling tree bytes.
    pub sapling: Vec<u8>,
    /// Orchard tree bytes.
    pub orchard: Vec<u8>,
    /// Ironwood tree bytes.
    pub ironwood: Vec<u8>,
    /// Sapling tree size (`chainMetadata.saplingCommitmentTreeSize`).
    pub sapling_size: u64,
    /// Orchard tree size.
    pub orchard_size: u64,
    /// Ironwood tree size.
    pub ironwood_size: u64,
}

/// A transaction found by txid: its bytes and where it lives.
pub struct TxLookup<'a> {
    /// Full serialized bytes.
    pub raw: &'a [u8],
    /// Mined height, or `None` while in the mempool.
    pub height: Option<u32>,
}

/// The deterministic chain state machine.
pub struct Chain {
    params: ChainParams,
    seed: Seed,
    /// Height of the first materialized block. Blocks below it are the
    /// empty prehistory, served on demand (ADR 0002).
    start_height: u32,
    /// Fork lineage as ascending `(start_height, fork_id)` segments. The
    /// fork id owning a height decides its empty-block hash, so a fork's
    /// prehistory matches its parent below the fork point and diverges
    /// above it. Always begins with `(0, base)`.
    lineage: Vec<(u32, u64)>,
    accounts: Vec<Account>,
    scan_keys: ScanKeys,
    blocks: Vec<Block>,
    sapling_tree: SaplingTree,
    orchard_tree: OrchardTree,
    ironwood_tree: IronwoodTree,
    notes: Vec<NoteRecord>,
    nf_index: HashMap<(u8, [u8; 32]), usize>,
    utxos: UtxoSet,
    outgoing: Vec<OutgoingPayment>,
    pending: Vec<PendingTx>,
    schedule: Vec<ScheduledEvent>,
    reserved: Vec<bool>,
    withhold: bool,
    fab_counter: u64,
    burn_addr: TransparentAddress,
}

fn pool_tag(pool: Pool) -> u8 {
    match pool {
        Pool::Sapling => 0,
        Pool::Orchard => 1,
        Pool::Ironwood => 2,
    }
}

fn burn_address() -> TransparentAddress {
    let digest = blake2b_simd::Params::new()
        .hash_length(20)
        .hash(b"darkside-burn-address");
    let mut hash = [0u8; 20];
    hash.copy_from_slice(digest.as_bytes());
    TransparentAddress::PublicKeyHash(hash)
}

fn orchard_bundle_version(branch: BranchId, pool: Pool) -> Option<BundleVersion> {
    match (pool, branch.orchard_protocol_revision()?) {
        (Pool::Ironwood, OrchardProtocolRevision::V3) => Some(BundleVersion::ironwood_v3()),
        (Pool::Ironwood, _) => None,
        (_, OrchardProtocolRevision::InsecureV1) => Some(BundleVersion::orchard_insecure_v1()),
        (_, OrchardProtocolRevision::V2) => Some(BundleVersion::orchard_v2()),
        (_, OrchardProtocolRevision::V3) => Some(BundleVersion::orchard_v3()),
    }
}

impl Chain {
    /// A new chain with a mined genesis at the parameters' boot height.
    /// Same seed, byte-identical chain.
    pub fn new(params: ChainParams, seed: Seed) -> Self {
        let start_height = params.start_height;
        let mut chain = Chain::bare(params, seed, Vec::new(), start_height, vec![(0, 0)]);
        chain
            .mine(1)
            .expect("mining genesis over an empty schedule cannot fail");
        chain
    }

    fn bare(
        params: ChainParams,
        seed: Seed,
        accounts: Vec<Account>,
        start_height: u32,
        lineage: Vec<(u32, u64)>,
    ) -> Self {
        let scan_keys = ScanKeys::build(&accounts);
        Chain {
            params,
            seed,
            start_height,
            lineage,
            accounts,
            scan_keys,
            blocks: Vec::new(),
            sapling_tree: SaplingTree(PoolTree::new()),
            orchard_tree: OrchardTree(PoolTree::new()),
            ironwood_tree: IronwoodTree(PoolTree::new()),
            notes: Vec::new(),
            nf_index: HashMap::new(),
            utxos: UtxoSet::default(),
            outgoing: Vec::new(),
            pending: Vec::new(),
            schedule: Vec::new(),
            reserved: Vec::new(),
            withhold: false,
            fab_counter: 0,
            burn_addr: burn_address(),
        }
    }

    /// The chain's parameters.
    pub fn params(&self) -> &ChainParams {
        &self.params
    }

    /// Height of the tip.
    pub fn tip_height(&self) -> u32 {
        match self.blocks.last() {
            Some(block) => block.height,
            None => self.start_height.saturating_sub(1),
        }
    }

    /// Height of the boot block, below which the chain is empty prehistory.
    pub fn start_height(&self) -> u32 {
        self.start_height
    }

    fn next_height(&self) -> u32 {
        match self.blocks.last() {
            Some(block) => block.height + 1,
            None => self.start_height,
        }
    }

    /// Blocks ascend by height but need not be consecutive: a jump leaves a
    /// span of unmined heights behind it, so the index is searched rather
    /// than computed.
    fn block_index(&self, height: u32) -> Option<usize> {
        self.blocks
            .binary_search_by_key(&height, |block| block.height)
            .ok()
    }

    /// A materialized block at `height`. Prehistory heights return `None`.
    /// Serve them with [`Chain::block_at`].
    pub fn block(&self, height: u32) -> Option<&Block> {
        self.block_index(height).map(|i| &self.blocks[i])
    }

    /// The block at `height`: materialized, or an empty block computed on
    /// demand for prehistory and for the spans a jump leaves behind. `None`
    /// only above the tip.
    pub fn block_at(&self, height: u32) -> Option<Block> {
        match self.block(height) {
            Some(block) => Some(block.clone()),
            None => (height <= self.tip_height()).then(|| self.empty_block(height)),
        }
    }

    /// All materialized blocks, boot block first.
    pub fn blocks(&self) -> &[Block] {
        &self.blocks
    }

    fn fork_id_at(&self, height: u32) -> u64 {
        self.lineage
            .iter()
            .rev()
            .find(|(start, _)| *start <= height)
            .map(|(_, fid)| *fid)
            .unwrap_or(0)
    }

    fn hash_at(&self, height: u32) -> BlockHash {
        match self.block(height) {
            Some(block) => block.hash,
            None => crate::block::empty_block_hash(&self.seed, self.fork_id_at(height), height),
        }
    }

    fn empty_block(&self, height: u32) -> Block {
        let hash = crate::block::empty_block_hash(&self.seed, self.fork_id_at(height), height);
        let prev_hash = if height == 0 {
            BlockHash([0u8; 32])
        } else {
            self.hash_at(height - 1)
        };
        Block {
            height,
            hash,
            prev_hash,
            time: self.params.scheduled_time(height),
            txs: Vec::new(),
        }
    }

    /// Declare an account: derive its keys from the seed string and retain
    /// the viewing keys. Declare accounts before mining blocks that pay
    /// them. Scanning is not retroactive.
    pub fn declare_account(
        &mut self,
        name: &str,
        seed_phrase: &str,
        account_index: u32,
    ) -> Result<()> {
        if self.account_index(name).is_some() {
            return Err(Error::Derivation(format!(
                "account {name} already declared"
            )));
        }
        let account = Account::derive(&self.params, name, seed_phrase, account_index)?;
        self.accounts.push(account);
        self.scan_keys = ScanKeys::build(&self.accounts);
        Ok(())
    }

    /// The declared account named `name`.
    pub fn account(&self, name: &str) -> Option<&Account> {
        self.account_index(name).map(|i| &self.accounts[i])
    }

    /// All declared accounts, in declaration order.
    pub fn accounts(&self) -> &[Account] {
        &self.accounts
    }

    fn account_index(&self, name: &str) -> Option<usize> {
        self.accounts.iter().position(|a| a.name() == name)
    }

    fn require_account(&self, name: &str) -> Result<usize> {
        self.account_index(name)
            .ok_or_else(|| Error::Derivation(format!("account {name} is not declared")))
    }

    fn resolve(&self, recipient: &Recipient, pool: Option<Pool>) -> Result<ResolvedRecipient> {
        match recipient {
            Recipient::Declared(name) => {
                let idx = self.require_account(name)?;
                let ua = self.accounts[idx].ua();
                match pool.unwrap_or(Pool::Orchard) {
                    Pool::Sapling => ua
                        .sapling()
                        .map(|pa| ResolvedRecipient::Sapling(Box::new(*pa)))
                        .ok_or_else(|| Error::Address(format!("{name} has no sapling receiver"))),
                    p => ua
                        .orchard()
                        .map(|oa| ResolvedRecipient::Orchard(*oa, p))
                        .ok_or_else(|| Error::Address(format!("{name} has no orchard receiver"))),
                }
            }
            Recipient::DeclaredTransparent(name) => {
                let idx = self.require_account(name)?;
                Ok(ResolvedRecipient::Transparent(*self.accounts[idx].taddr()))
            }
            Recipient::Literal(addr) => self.resolve_literal(addr, pool),
            Recipient::LiteralTransparent(addr) => self.resolve_literal_transparent(addr),
        }
    }

    /// The receivers `addr` carries, most recent first: Ironwood and Orchard
    /// (one receiver, two pools), then Sapling, then transparent. Says
    /// nothing about whether the chain has those pools active, which is a
    /// separate question the caller asks of [`Chain::params`].
    pub fn receivers(&self, addr: &str) -> Result<Vec<Receiver>> {
        use zcash_keys::address::Address;
        let parsed = Address::decode(&self.params.network, addr)
            .ok_or_else(|| Error::Address(format!("cannot parse address {addr:?}")))?;
        let mut found = Vec::new();
        match parsed {
            Address::Sapling(_) => found.push(Receiver::Sapling),
            Address::Transparent(_) | Address::Tex(_) => found.push(Receiver::Transparent),
            Address::Unified(ua) => {
                if ua.orchard().is_some() {
                    found.push(Receiver::Ironwood);
                    found.push(Receiver::Orchard);
                }
                if ua.sapling().is_some() {
                    found.push(Receiver::Sapling);
                }
                if ua.transparent().is_some() {
                    found.push(Receiver::Transparent);
                }
            }
        }
        Ok(found)
    }

    fn resolve_literal_transparent(&self, addr: &str) -> Result<ResolvedRecipient> {
        use zcash_keys::address::Address;
        let parsed = Address::decode(&self.params.network, addr)
            .ok_or_else(|| Error::Address(format!("cannot parse address {addr:?}")))?;
        let no_receiver = || Error::Address(format!("no transparent receiver in {addr:?}"));
        match parsed {
            Address::Transparent(t) => Ok(ResolvedRecipient::Transparent(t)),
            Address::Tex(hash) => Ok(ResolvedRecipient::Transparent(
                TransparentAddress::PublicKeyHash(hash),
            )),
            Address::Unified(ua) => ua
                .transparent()
                .map(|t| ResolvedRecipient::Transparent(*t))
                .ok_or_else(no_receiver),
            Address::Sapling(_) => Err(no_receiver()),
        }
    }

    fn resolve_literal(&self, addr: &str, pool: Option<Pool>) -> Result<ResolvedRecipient> {
        use zcash_keys::address::Address;
        let parsed = Address::decode(&self.params.network, addr)
            .ok_or_else(|| Error::Address(format!("cannot parse address {addr:?}")))?;
        match parsed {
            Address::Sapling(pa) => Ok(ResolvedRecipient::Sapling(Box::new(pa))),
            Address::Transparent(t) => Ok(ResolvedRecipient::Transparent(t)),
            Address::Tex(hash) => Ok(ResolvedRecipient::Transparent(
                TransparentAddress::PublicKeyHash(hash),
            )),
            Address::Unified(ua) => {
                if let Some(oa) = ua.orchard()
                    && !matches!(pool, Some(Pool::Sapling))
                {
                    return Ok(ResolvedRecipient::Orchard(
                        *oa,
                        pool.unwrap_or(Pool::Orchard),
                    ));
                }
                if let Some(pa) = ua.sapling()
                    && !matches!(pool, Some(Pool::Orchard | Pool::Ironwood))
                {
                    return Ok(ResolvedRecipient::Sapling(Box::new(*pa)));
                }
                ua.transparent()
                    .map(|t| ResolvedRecipient::Transparent(*t))
                    .ok_or_else(|| Error::Address(format!("no usable receiver in {addr:?}")))
            }
        }
    }

    fn check_pool_active(&self, pool: Pool, height: u32) -> Result<()> {
        let (nu, name) = match pool {
            Pool::Sapling => (NetworkUpgrade::Sapling, "sapling"),
            Pool::Orchard => (NetworkUpgrade::Nu5, "orchard"),
            Pool::Ironwood => (NetworkUpgrade::Nu6_3, "ironwood"),
        };
        if self.params.is_active(nu, height) {
            Ok(())
        } else {
            Err(Error::InvalidHeight(format!(
                "pool {name} is not active at height {height}"
            )))
        }
    }

    fn recipient_pool(recipient: &ResolvedRecipient) -> Option<Pool> {
        match recipient {
            ResolvedRecipient::Sapling(_) => Some(Pool::Sapling),
            ResolvedRecipient::Orchard(_, pool) => Some(*pool),
            ResolvedRecipient::Transparent(_) => None,
        }
    }

    /// Schedule a `fund`. Validated now, fabricated when mining
    /// reaches its height.
    pub fn fund(&mut self, spec: FundSpec) -> Result<()> {
        if spec.at <= self.tip_height() {
            return Err(Error::InvalidHeight(format!(
                "fund at {} but the tip is already {}",
                spec.at,
                self.tip_height()
            )));
        }
        if spec.outputs == 0 {
            return Err(Error::Amount("a fund needs at least one output".into()));
        }
        Zatoshis::from_u64(spec.zats)
            .map_err(|_| Error::Amount(format!("{} zatoshis out of range", spec.zats)))?;
        let recipient = self.resolve(&spec.recipient, spec.pool)?;
        if let Some(pool) = Self::recipient_pool(&recipient) {
            self.check_pool_active(pool, spec.at)?;
        }
        if spec.via_coinbase && !matches!(recipient, ResolvedRecipient::Transparent(_)) {
            return Err(Error::Address(
                "via coinbase requires a transparent recipient".into(),
            ));
        }
        self.schedule.push(ScheduledEvent {
            pending_from: spec.at,
            mine_at: Some(spec.at),
            expiring_at: None,
            kind: EventKind::Fund {
                recipient,
                zats: spec.zats,
                outputs: spec.outputs,
                via_coinbase: spec.via_coinbase,
                corruption: spec.corruption,
            },
            done: false,
        });
        Ok(())
    }

    /// Schedule a `send`. Note selection and fabrication happen when
    /// the tip reaches its `pending_from` height. Overdrawing the source
    /// pool surfaces as an error from [`Chain::mine`] while the world is
    /// being built.
    pub fn send(&mut self, spec: SendSpec) -> Result<()> {
        let from = self.require_account(&spec.from)?;
        let recipient = self.resolve(&spec.recipient, spec.pool)?;
        let pool = spec
            .pool
            .or_else(|| Self::recipient_pool(&recipient))
            .unwrap_or(Pool::Orchard);
        let mine_at = spec.at;
        if let Some(at) = mine_at {
            if at <= self.tip_height() {
                return Err(Error::InvalidHeight(format!(
                    "send at {at} but the tip is already {}",
                    self.tip_height()
                )));
            }
            self.check_pool_active(pool, at)?;
        } else if spec.expiring_at.is_none() {
            return Err(Error::InvalidHeight(
                "a send needs a mine height or an expiry".into(),
            ));
        }
        let pending_from = match (spec.pending_from, mine_at) {
            (Some(p), Some(at)) if p >= at => {
                return Err(Error::InvalidHeight(format!(
                    "pending from {p} is not before the mine height {at}"
                )));
            }
            (Some(p), _) => p,
            (None, Some(at)) => at.saturating_sub(1),
            (None, None) => self.tip_height(),
        };
        if let Some(exp) = spec.expiring_at
            && exp <= pending_from
        {
            return Err(Error::InvalidHeight(format!(
                "expiring at {exp} is not after pending from {pending_from}"
            )));
        }
        self.schedule.push(ScheduledEvent {
            pending_from,
            mine_at,
            expiring_at: spec.expiring_at,
            kind: EventKind::Send {
                from,
                pool,
                zats: spec.zats,
                recipient,
                corruption: spec.corruption,
            },
            done: false,
        });
        // A send whose mempool-entry height has already been reached shows
        // up immediately.
        self.fabricate_due_sends()
    }

    fn fabricate_due_sends(&mut self) -> Result<()> {
        let tip = self.tip_height();
        let due: Vec<usize> = self
            .schedule
            .iter()
            .enumerate()
            .filter(|(_, ev)| {
                !ev.done && matches!(ev.kind, EventKind::Send { .. }) && ev.pending_from <= tip
            })
            .map(|(i, _)| i)
            .collect();
        for idx in due {
            self.fabricate_send(idx)?;
        }
        Ok(())
    }

    /// Accept raw transaction bytes from a wallet. Rejects on parse
    /// failure only.
    pub fn submit(&mut self, bytes: &[u8]) -> Result<TxId> {
        let branch = self.params.branch_id(self.tip_height() + 1);
        let tx = Transaction::read(bytes, branch).map_err(Error::TxParse)?;
        let txid = tx.txid();
        let expiry = u32::from(tx.expiry_height());
        self.pending.push(PendingTx {
            txid,
            raw: bytes.to_vec(),
            tx,
            visible_from: self.tip_height(),
            mine_at: None,
            expires_after: (expiry != 0).then_some(expiry),
            corruption: None,
            source: TxSource::Submitted,
            state: PendingState::Pending,
        });
        Ok(txid)
    }

    /// Accept wallet submissions but never mine them.
    pub fn set_withhold(&mut self, on: bool) {
        self.withhold = on;
    }

    /// Mine `count` blocks with scenario-driver times, executing any
    /// scheduled content whose height is reached.
    pub fn mine(&mut self, count: u32) -> Result<()> {
        for _ in 0..count {
            let next = self.next_height();
            self.mine_next(self.params.scheduled_time(next))?;
        }
        Ok(())
    }

    /// Mine one block at `height`, leaving the span between it and the tip
    /// as empty blocks computed on demand (ADR 0002). Returns the new tip.
    ///
    /// This is how darkside crosses a million heights without storing a
    /// block per height. Nothing below the span is touched: existing blocks
    /// keep their heights and hashes, and the trees carry through unchanged
    /// because an empty block appends no commitments.
    ///
    /// Refused when the span it would skip has scheduled work in it, since
    /// those events would never fire.
    pub fn jump_to(&mut self, height: u32, time: u32) -> Result<u32> {
        let next = self.next_height();
        if height < next {
            return Err(Error::InvalidHeight(format!(
                "cannot jump to {height}, the next block is already {next}"
            )));
        }
        if let Some(scheduled) = self
            .schedule
            .iter()
            .filter(|ev| !ev.done)
            .filter_map(|ev| ev.mine_at)
            .filter(|at| *at < height)
            .min()
        {
            return Err(Error::InvalidHeight(format!(
                "cannot jump to {height}, work is scheduled at {scheduled} inside the span"
            )));
        }
        self.mine_block_at(height, time)?;
        Ok(self.tip_height())
    }

    /// Mine one block with an explicit (live-driver) time. Returns the new
    /// tip height.
    pub fn mine_with_time(&mut self, time: u32) -> Result<u32> {
        if let Some(last) = self.blocks.last()
            && time < last.time
        {
            return Err(Error::InvalidHeight(format!(
                "block time {time} is before the tip's time {}",
                last.time
            )));
        }
        self.mine_next(time)?;
        Ok(self.tip_height())
    }

    fn next_fab_rng(&mut self) -> rand_chacha::ChaCha20Rng {
        let rng = self.seed.rng_for("fabricate", self.fab_counter);
        self.fab_counter += 1;
        rng
    }

    fn sapling_anchor(&self) -> bls12_381::Scalar {
        use incrementalmerkletree::{Hashable, Level};
        let node = self
            .sapling_tree
            .0
            .root_at(self.tip_height())
            .unwrap_or_else(|| SaplingNode::empty_root(Level::from(crate::tree::TREE_DEPTH)));
        bls12_381::Scalar::from(node)
    }

    fn orchard_anchor(&self, pool: Pool) -> orchard::Anchor {
        let tree = match pool {
            Pool::Ironwood => &self.ironwood_tree.0,
            _ => &self.orchard_tree.0,
        };
        tree.root_at(self.tip_height())
            .map(orchard::Anchor::from)
            .unwrap_or_else(orchard::Anchor::empty_tree)
    }

    fn fabricate_send(&mut self, event_idx: usize) -> Result<()> {
        let (from, pool, zats, recipient, corruption) = match &self.schedule[event_idx].kind {
            EventKind::Send {
                from,
                pool,
                zats,
                recipient,
                corruption,
            } => (*from, *pool, *zats, recipient.clone(), *corruption),
            EventKind::Fund { .. } => unreachable!("funds are fabricated at mine time"),
        };
        let (mine_at, expiring_at) = {
            let ev = &self.schedule[event_idx];
            (ev.mine_at, ev.expiring_at)
        };

        // Oldest-first selection within the source pool, skipping
        // notes already reserved by an earlier pending send.
        let mut selected = Vec::new();
        let mut selected_value = 0u64;
        for (idx, record) in self.notes.iter().enumerate() {
            if record.account == from
                && record.pool == pool
                && record.spent.is_none()
                && !self.reserved[idx]
            {
                selected.push(idx);
                selected_value += record.value;
                if selected_value >= zats {
                    break;
                }
            }
        }
        if selected_value < zats {
            return Err(Error::InsufficientFunds {
                account: self.accounts[from].name().to_owned(),
                pool,
                available: self
                    .notes
                    .iter()
                    .enumerate()
                    .filter(|(i, r)| {
                        r.account == from
                            && r.pool == pool
                            && r.spent.is_none()
                            && !self.reserved[*i]
                    })
                    .map(|(_, r)| r.value)
                    .sum(),
                requested: zats,
            });
        }
        let change = selected_value - zats;
        let spends = selected
            .iter()
            .map(|&i| SpendInput {
                nullifier: self.notes[i].nullifier,
                value: self.notes[i].value,
            })
            .collect::<Vec<_>>();

        let corrupt_spends = corruption == Some(Corruption::Spentness);
        let corrupt_cmx = corruption == Some(Corruption::Commitment);
        let mine_height = mine_at.unwrap_or(self.tip_height() + 1);
        let branch = self.params.branch_id(mine_height);
        let mut rng = self.next_fab_rng();
        let ufvk = self.accounts[from].ufvk();

        // Route the recipient output, the change output (source pool,
        // internal address), and any transparent output, then assemble.
        let mut sapling_outs: Vec<SaplingOut> = Vec::new();
        let mut orchard_outs: Vec<OrchardOut> = Vec::new();
        let mut ironwood_outs: Vec<OrchardOut> = Vec::new();
        let mut transparent_outs: Vec<(TransparentAddress, u64)> = Vec::new();

        match &recipient {
            ResolvedRecipient::Sapling(pa) => sapling_outs.push(SaplingOut {
                addr: *pa.as_ref(),
                value: zats,
                ovk: ufvk.sapling().map(|d| d.to_ovk(zip32::Scope::External)),
                corrupt_cmx,
            }),
            ResolvedRecipient::Orchard(oa, out_pool) => {
                let out = OrchardOut {
                    addr: *oa,
                    value: zats,
                    ovk: ufvk.orchard().map(|f| f.to_ovk(zip32::Scope::External)),
                    corrupt_cmx,
                };
                match out_pool {
                    Pool::Ironwood => ironwood_outs.push(out),
                    _ => orchard_outs.push(out),
                }
            }
            ResolvedRecipient::Transparent(t) => transparent_outs.push((*t, zats)),
        }
        if change > 0 {
            match pool {
                Pool::Sapling => {
                    let dfvk = ufvk
                        .sapling()
                        .ok_or_else(|| Error::Derivation("sender lacks sapling keys".into()))?;
                    sapling_outs.push(SaplingOut {
                        addr: dfvk.change_address().1,
                        value: change,
                        ovk: Some(dfvk.to_ovk(zip32::Scope::Internal)),
                        corrupt_cmx: false,
                    });
                }
                p => {
                    let fvk = ufvk
                        .orchard()
                        .ok_or_else(|| Error::Derivation("sender lacks orchard keys".into()))?;
                    let out = OrchardOut {
                        addr: fvk.address_at(0u32, zip32::Scope::Internal),
                        value: change,
                        ovk: Some(fvk.to_ovk(zip32::Scope::Internal)),
                        corrupt_cmx: false,
                    };
                    match p {
                        Pool::Ironwood => ironwood_outs.push(out),
                        _ => orchard_outs.push(out),
                    }
                }
            }
        }

        let sapling_spends = if pool == Pool::Sapling {
            &spends[..]
        } else {
            &[]
        };
        let orchard_spends = if pool == Pool::Orchard {
            &spends[..]
        } else {
            &[]
        };
        let ironwood_spends = if pool == Pool::Ironwood {
            &spends[..]
        } else {
            &[]
        };

        let sapling_bundle = if sapling_spends.is_empty() && sapling_outs.is_empty() {
            None
        } else {
            fabricate::sapling_bundle(
                &mut rng,
                self.sapling_anchor(),
                sapling_spends,
                &sapling_outs,
                corrupt_spends,
            )
        };
        let dummy_recipient = self.accounts[from]
            .ua()
            .orchard()
            .copied()
            .ok_or_else(|| Error::Derivation("sender lacks an orchard receiver".into()))?;
        let orchard_bundle = if orchard_spends.is_empty() && orchard_outs.is_empty() {
            None
        } else {
            let version = orchard_bundle_version(branch, Pool::Orchard).ok_or_else(|| {
                Error::InvalidHeight(format!("orchard is not available at height {mine_height}"))
            })?;
            fabricate::orchard_bundle(
                &mut rng,
                Pool::Orchard,
                version,
                self.orchard_anchor(Pool::Orchard),
                orchard_spends,
                &orchard_outs,
                dummy_recipient,
                corrupt_spends,
            )
        };
        let ironwood_bundle = if ironwood_spends.is_empty() && ironwood_outs.is_empty() {
            None
        } else {
            let version = orchard_bundle_version(branch, Pool::Ironwood).ok_or_else(|| {
                Error::InvalidHeight(format!("ironwood is not available at height {mine_height}"))
            })?;
            fabricate::orchard_bundle(
                &mut rng,
                Pool::Ironwood,
                version,
                self.orchard_anchor(Pool::Ironwood),
                ironwood_spends,
                &ironwood_outs,
                dummy_recipient,
                corrupt_spends,
            )
        };
        let transparent_bundle = if transparent_outs.is_empty() {
            None
        } else {
            Some(fabricate::transparent_fund_bundle(&transparent_outs)?)
        };

        let (tx, raw, txid) = fabricate::assemble(
            &self.params,
            mine_height,
            expiring_at.unwrap_or(0),
            transparent_bundle,
            sapling_bundle,
            orchard_bundle,
            ironwood_bundle,
        )?;

        for idx in selected {
            self.reserved[idx] = true;
        }
        let ev = &self.schedule[event_idx];
        self.pending.push(PendingTx {
            txid,
            raw,
            tx,
            visible_from: ev.pending_from,
            mine_at: ev.mine_at,
            expires_after: ev.expiring_at,
            corruption,
            source: TxSource::Fabricated,
            state: PendingState::Pending,
        });
        self.schedule[event_idx].done = true;
        Ok(())
    }

    fn fabricate_fund(&mut self, event_idx: usize, mine_height: u32) -> Result<MinedTx> {
        let (recipient, zats, outputs, via_coinbase, corruption) =
            match &self.schedule[event_idx].kind {
                EventKind::Fund {
                    recipient,
                    zats,
                    outputs,
                    via_coinbase,
                    corruption,
                } => (
                    recipient.clone(),
                    *zats,
                    *outputs,
                    *via_coinbase,
                    *corruption,
                ),
                EventKind::Send { .. } => unreachable!("sends are fabricated from the schedule"),
            };
        let branch = self.params.branch_id(mine_height);
        let corrupt_cmx = corruption == Some(Corruption::Commitment);
        let n = outputs as usize;
        let mut rng = self.next_fab_rng();

        let (transparent, sapling, orchard_b, ironwood) = match &recipient {
            ResolvedRecipient::Transparent(t) => {
                let outs = vec![(*t, zats); n];
                let bundle = if via_coinbase {
                    fabricate::coinbase_bundle(mine_height, &outs)?
                } else {
                    fabricate::transparent_fund_bundle(&outs)?
                };
                (Some(bundle), None, None, None)
            }
            ResolvedRecipient::Sapling(pa) => {
                let outs: Vec<SaplingOut> = (0..n)
                    .map(|_| SaplingOut {
                        addr: *pa.as_ref(),
                        value: zats,
                        ovk: None,
                        corrupt_cmx,
                    })
                    .collect();
                let bundle =
                    fabricate::sapling_bundle(&mut rng, self.sapling_anchor(), &[], &outs, false);
                (None, bundle, None, None)
            }
            ResolvedRecipient::Orchard(oa, pool) => {
                let version = orchard_bundle_version(branch, *pool).ok_or_else(|| {
                    Error::InvalidHeight(format!(
                        "pool {pool:?} is not available at height {mine_height}"
                    ))
                })?;
                let outs: Vec<OrchardOut> = (0..n)
                    .map(|_| OrchardOut {
                        addr: *oa,
                        value: zats,
                        ovk: None,
                        corrupt_cmx,
                    })
                    .collect();
                let bundle = fabricate::orchard_bundle(
                    &mut rng,
                    *pool,
                    version,
                    self.orchard_anchor(*pool),
                    &[],
                    &outs,
                    *oa,
                    false,
                );
                match pool {
                    Pool::Ironwood => (None, None, None, bundle),
                    _ => (None, None, bundle, None),
                }
            }
        };
        let (tx, raw, txid) = fabricate::assemble(
            &self.params,
            mine_height,
            0,
            transparent,
            sapling,
            orchard_b,
            ironwood,
        )?;
        self.schedule[event_idx].done = true;
        Ok(MinedTx {
            txid,
            raw,
            tx,
            corruption,
        })
    }

    fn synthetic_coinbase(&mut self, height: u32) -> Result<MinedTx> {
        let bundle = fabricate::coinbase_bundle(height, &[(self.burn_addr, 0)])?;
        let (tx, raw, txid) =
            fabricate::assemble(&self.params, height, 0, Some(bundle), None, None, None)?;
        Ok(MinedTx {
            txid,
            raw,
            tx,
            corruption: None,
        })
    }

    fn mine_next(&mut self, time: u32) -> Result<()> {
        self.mine_block_at(self.next_height(), time)
    }

    /// Mine one block at `next`, which is the height directly above the tip
    /// except when a jump puts an unmined span below it.
    fn mine_block_at(&mut self, next: u32, time: u32) -> Result<()> {
        // Evict expired entries before draining.
        for entry in &mut self.pending {
            if entry.state == PendingState::Pending && entry.expired_at(next) {
                entry.state = PendingState::Evicted(next, Eviction::Expired);
            }
        }

        // Coinbase: a scheduled `via coinbase` fund, or the synthetic burn.
        let coinbase_event = self.schedule.iter().position(|ev| {
            !ev.done
                && ev.mine_at == Some(next)
                && matches!(
                    ev.kind,
                    EventKind::Fund {
                        via_coinbase: true,
                        ..
                    }
                )
        });
        let coinbase = match coinbase_event {
            Some(idx) => self.fabricate_fund(idx, next)?,
            None => self.synthetic_coinbase(next)?,
        };
        let mut txs = vec![coinbase];

        // Scheduled funds at this height, in declaration order.
        let fund_events: Vec<usize> = self
            .schedule
            .iter()
            .enumerate()
            .filter(|(_, ev)| {
                !ev.done
                    && ev.mine_at == Some(next)
                    && matches!(
                        ev.kind,
                        EventKind::Fund {
                            via_coinbase: false,
                            ..
                        }
                    )
            })
            .map(|(i, _)| i)
            .collect();
        for idx in fund_events {
            let mined = self.fabricate_fund(idx, next)?;
            txs.push(mined);
        }

        // Drain the mempool: scheduled sends for this height, then wallet
        // submissions. First conflicting spend wins; the loser is evicted.
        let mut spent_nfs: Vec<(u8, [u8; 32])> = Vec::new();
        let mut spent_outpoints: Vec<OutPoint> = Vec::new();
        for entry in &mut self.pending {
            if entry.state != PendingState::Pending {
                continue;
            }
            let eligible = match entry.source {
                TxSource::Fabricated => entry.mine_at == Some(next),
                TxSource::Submitted => !self.withhold,
            };
            if !eligible {
                continue;
            }
            let mut conflict = false;
            let mut tx_nfs = Vec::new();
            let mut tx_outpoints = Vec::new();
            if let Some(bundle) = entry.tx.sapling_bundle() {
                for spend in bundle.shielded_spends() {
                    tx_nfs.push((pool_tag(Pool::Sapling), spend.nullifier().0));
                }
            }
            if let Some(bundle) = entry.tx.orchard_bundle() {
                for action in bundle.actions() {
                    tx_nfs.push((pool_tag(Pool::Orchard), action.nullifier().to_bytes()));
                }
            }
            if let Some(bundle) = entry.tx.ironwood_bundle() {
                for action in bundle.actions() {
                    tx_nfs.push((pool_tag(Pool::Ironwood), action.nullifier().to_bytes()));
                }
            }
            if let Some(bundle) = entry.tx.transparent_bundle() {
                for txin in &bundle.vin {
                    tx_outpoints.push(txin.prevout().clone());
                }
            }
            if tx_nfs.iter().any(|nf| spent_nfs.contains(nf))
                || tx_outpoints.iter().any(|op| spent_outpoints.contains(op))
            {
                conflict = true;
            }
            if conflict {
                entry.state = PendingState::Evicted(next, Eviction::Conflict);
                continue;
            }
            spent_nfs.extend(tx_nfs);
            spent_outpoints.extend(tx_outpoints);
            entry.state = PendingState::Mined(next);
            txs.push(MinedTx {
                txid: entry.txid,
                raw: entry.raw.clone(),
                tx: entry.tx.clone(),
                corruption: entry.corruption,
            });
        }

        let prev_hash = if next == 0 {
            BlockHash([0u8; 32])
        } else {
            self.hash_at(next - 1)
        };
        let hash = block_hash(next, self.fork_id_at(next), prev_hash, time, &self.seed);
        let block = Block {
            height: next,
            hash,
            prev_hash,
            time,
            txs,
        };
        self.apply_block(&block);
        self.blocks.push(block);
        // Sends anchored to the new tip enter the mempool now, before the
        // next tick, so pending state is observable between blocks.
        self.fabricate_due_sends()
    }

    /// Apply a block to derived state: trees, nullifiers, the UTXO set,
    /// and the note records. Used by mining and by fork replay. State
    /// drift is unrepresentable because this is the only writer.
    fn apply_block(&mut self, block: &Block) {
        let height = block.height;
        for (tx_index, mined) in block.txs.iter().enumerate() {
            let divergent = mined.corruption == Some(Corruption::Divergence);
            let txid = mined.txid;
            if let Some(bundle) = mined.tx.transparent_bundle() {
                self.utxos.apply_tx(txid, height, tx_index, bundle);
            }
            if let Some(bundle) = mined.tx.sapling_bundle() {
                for spend in bundle.shielded_spends() {
                    self.mark_spent(Pool::Sapling, spend.nullifier().0, height);
                }
                let domain = SaplingDomain::new(Zip212Enforcement::On);
                for output in bundle.shielded_outputs() {
                    let position = self
                        .sapling_tree
                        .0
                        .append(SaplingNode::from_cmu(output.cmu()));
                    if divergent {
                        continue;
                    }
                    let matched = self.scan_keys.sapling_ivks.iter().find_map(|key| {
                        try_note_decryption(&domain, &key.ivk, output)
                            .map(|(note, _, _)| (key, note))
                    });
                    if let Some((key, note)) = matched {
                        let nf = note.nf(&key.dfvk.to_nk(key.scope), position);
                        self.push_note(NoteRecord {
                            account: key.account,
                            pool: Pool::Sapling,
                            value: note.value().inner(),
                            height,
                            txid,
                            position,
                            nullifier: nf.0,
                            spent: None,
                        });
                    } else {
                        for (account, ovk) in &self.scan_keys.sapling_ovks {
                            if let Some((note, _, _)) = try_output_recovery_with_ovk(
                                &domain,
                                ovk,
                                output,
                                output.cv(),
                                output.out_ciphertext(),
                            ) {
                                self.outgoing.push(OutgoingPayment {
                                    from: *account,
                                    pool: Pool::Sapling,
                                    value: note.value().inner(),
                                    height,
                                    txid,
                                    to_declared: None,
                                });
                                break;
                            }
                        }
                    }
                }
            }
            if let Some(bundle) = mined.tx.orchard_bundle() {
                self.scan_orchard_actions(bundle, Pool::Orchard, height, txid, divergent);
            }
            if let Some(bundle) = mined.tx.ironwood_bundle() {
                self.scan_orchard_actions(bundle, Pool::Ironwood, height, txid, divergent);
            }
        }
        let hash = block.hash.0;
        self.sapling_tree.0.end_block(height, hash);
        self.orchard_tree.0.end_block(height, hash);
        self.ironwood_tree.0.end_block(height, hash);
    }

    fn scan_orchard_actions(
        &mut self,
        bundle: &orchard::Bundle<orchard::bundle::Authorized, zcash_protocol::value::ZatBalance>,
        pool: Pool,
        height: u32,
        txid: TxId,
        divergent: bool,
    ) {
        for action in bundle.actions() {
            self.mark_spent(pool, action.nullifier().to_bytes(), height);
            let tree = match pool {
                Pool::Ironwood => &mut self.ironwood_tree.0,
                _ => &mut self.orchard_tree.0,
            };
            let position = tree.append(MerkleHashOrchard::from_cmx(action.cmx()));
            if divergent {
                continue;
            }
            let matched = self.scan_keys.orchard_ivks.iter().find_map(|key| {
                let decrypted = match pool {
                    Pool::Ironwood => {
                        try_note_decryption(&IronwoodDomain::for_action(action), &key.ivk, action)
                    }
                    _ => try_note_decryption(&OrchardDomain::for_action(action), &key.ivk, action),
                };
                decrypted.map(|(note, _, _)| (key, note))
            });
            if let Some((key, note)) = matched {
                let nf = note.nullifier(&key.fvk);
                self.push_note(NoteRecord {
                    account: key.account,
                    pool,
                    value: note.value().inner(),
                    height,
                    txid,
                    position,
                    nullifier: nf.to_bytes(),
                    spent: None,
                });
            } else {
                for (account, ovk) in &self.scan_keys.orchard_ovks {
                    let recovered = match pool {
                        Pool::Ironwood => try_output_recovery_with_ovk(
                            &IronwoodDomain::for_action(action),
                            ovk,
                            action,
                            action.cv_net(),
                            &action.encrypted_note().out_ciphertext,
                        ),
                        _ => try_output_recovery_with_ovk(
                            &OrchardDomain::for_action(action),
                            ovk,
                            action,
                            action.cv_net(),
                            &action.encrypted_note().out_ciphertext,
                        ),
                    };
                    if let Some((note, _, _)) = recovered {
                        if note.value().inner() > 0 {
                            self.outgoing.push(OutgoingPayment {
                                from: *account,
                                pool,
                                value: note.value().inner(),
                                height,
                                txid,
                                to_declared: None,
                            });
                        }
                        break;
                    }
                }
            }
        }
    }

    fn push_note(&mut self, record: NoteRecord) {
        let key = (pool_tag(record.pool), record.nullifier);
        self.nf_index.insert(key, self.notes.len());
        self.notes.push(record);
        self.reserved.push(false);
    }

    fn mark_spent(&mut self, pool: Pool, nullifier: [u8; 32], height: u32) {
        if let Some(&idx) = self.nf_index.get(&(pool_tag(pool), nullifier)) {
            self.notes[idx].spent = Some(height);
        }
    }

    /// Fork this chain at `height`: a new value sharing blocks `0..=height`
    /// (identical hashes) with its own derived state, empty schedule, and
    /// empty mempool.
    pub fn fork_at(&self, height: u32) -> Result<Chain> {
        if height > self.tip_height() {
            return Err(Error::InvalidHeight(format!(
                "cannot fork at {height}, the tip is {}",
                self.tip_height()
            )));
        }
        if height < self.start_height {
            return Err(Error::InvalidHeight(format!(
                "cannot fork at {height}, below the boot height {}",
                self.start_height
            )));
        }
        let new_fork_id = self
            .fork_id_at(height)
            .wrapping_mul(0x0000_0100_0000_01b3)
            .wrapping_add(u64::from(height) + 1);
        // Inherit the parent's lineage up to the fork point, then diverge:
        // heights above `height` carry the new id, so their empty and mined
        // blocks differ from the parent's.
        let mut lineage: Vec<(u32, u64)> = self
            .lineage
            .iter()
            .filter(|(start, _)| *start <= height)
            .cloned()
            .collect();
        lineage.push((height + 1, new_fork_id));
        let mut fork = Chain::bare(
            self.params.clone(),
            self.seed,
            self.accounts.clone(),
            self.start_height,
            lineage,
        );
        fork.fab_counter = self.fab_counter;
        // Blocks may skip heights, so take every one at or below the fork
        // point rather than indexing by offset.
        let last = self.blocks.partition_point(|b| b.height <= height);
        for block in &self.blocks[..last] {
            fork.apply_block(block);
            fork.blocks.push(block.clone());
        }
        Ok(fork)
    }

    // --- Escape hatches: inconsistency must require asking for it ---

    /// Overwrite the stored `prev_hash` of the block at `height` with
    /// garbage, breaking continuity deliberately.
    pub fn corrupt_prev_hash(&mut self, height: u32) -> Result<()> {
        let seed = self.seed;
        let idx = self
            .block_index(height)
            .ok_or_else(|| Error::InvalidHeight(format!("no block at {height}")))?;
        let block = &mut self.blocks[idx];
        let mut garbage = [0u8; 32];
        seed.rng_for("corrupt-prev-hash", height as u64)
            .fill_bytes(&mut garbage);
        block.prev_hash = BlockHash(garbage);
        Ok(())
    }

    /// Damage the served tree state for one pool at `height` so it
    /// disagrees with the commitments in the block stream.
    pub fn corrupt_tree_state_at(&mut self, height: u32, pool: Pool) -> Result<()> {
        let mut rng = self.seed.rng_for("corrupt-tree-state", height as u64);
        let corrupted = match pool {
            Pool::Sapling => {
                let node = loop {
                    let mut bytes = [0u8; 32];
                    rng.fill_bytes(&mut bytes);
                    if let Some(node) = Option::from(SaplingNode::from_bytes(bytes)) {
                        break node;
                    }
                };
                self.sapling_tree.0.corrupt_frontier(height, node)
            }
            p => {
                let node = loop {
                    let mut bytes = [0u8; 32];
                    rng.fill_bytes(&mut bytes);
                    if let Some(node) = Option::from(MerkleHashOrchard::from_bytes(&bytes)) {
                        break node;
                    }
                };
                match p {
                    Pool::Ironwood => self.ironwood_tree.0.corrupt_frontier(height, node),
                    _ => self.orchard_tree.0.corrupt_frontier(height, node),
                }
            }
        };
        if corrupted {
            Ok(())
        } else {
            Err(Error::InvalidHeight(format!("no tree state at {height}")))
        }
    }

    /// Insert a phantom UTXO no transaction created, so address queries
    /// disagree with the block stream.
    pub fn corrupt_utxo(&mut self, addr: TransparentAddress, zats: u64, height: u32) -> Result<()> {
        let value = Zatoshis::from_u64(zats)
            .map_err(|_| Error::Amount(format!("{zats} zatoshis out of range")))?;
        let mut fake_txid = [0u8; 32];
        self.seed
            .rng_for("corrupt-utxo", self.utxos.txids_for(&addr).len() as u64)
            .fill_bytes(&mut fake_txid);
        let outpoint = OutPoint::new(fake_txid, 0);
        self.utxos.insert_phantom(Utxo {
            outpoint,
            script: zcash_transparent::address::Script::from(addr.script()),
            value,
            height,
            address: Some(addr),
        });
        Ok(())
    }

    // --- Serve-side accessors ---

    /// Serialized tree states and sizes as of `height`. Prehistory heights
    /// below the boot height return empty trees, and a height inside an
    /// unmined span
    /// returns the state as of the last mined block below it. Above the tip
    /// there is nothing to serve.
    pub fn tree_states(&self, height: u32) -> Option<TreeStates> {
        if height > self.tip_height() {
            return None;
        }
        if height < self.start_height {
            return Some(TreeStates {
                sapling: PoolTree::<SaplingNode>::empty_state_bytes(),
                orchard: PoolTree::<MerkleHashOrchard>::empty_state_bytes(),
                ironwood: PoolTree::<MerkleHashOrchard>::empty_state_bytes(),
                sapling_size: 0,
                orchard_size: 0,
                ironwood_size: 0,
            });
        }
        Some(TreeStates {
            sapling: self.sapling_tree.0.tree_state_bytes(height)?,
            orchard: self.orchard_tree.0.tree_state_bytes(height)?,
            ironwood: self.ironwood_tree.0.tree_state_bytes(height)?,
            sapling_size: self.sapling_tree.0.size_at(height)?,
            orchard_size: self.orchard_tree.0.size_at(height)?,
            ironwood_size: self.ironwood_tree.0.size_at(height)?,
        })
    }

    /// Completed subtree roots for one pool, oldest first. Legitimately
    /// empty below 65,536 notes in the pool.
    pub fn subtree_roots(&self, pool: Pool) -> &[SubtreeRoot] {
        match pool {
            Pool::Sapling => self.sapling_tree.0.subtree_roots(),
            Pool::Orchard => self.orchard_tree.0.subtree_roots(),
            Pool::Ironwood => self.ironwood_tree.0.subtree_roots(),
        }
    }

    /// Entries currently visible in the mempool.
    pub fn mempool(&self) -> Vec<&PendingTx> {
        let tip = self.tip_height();
        self.pending.iter().filter(|p| p.in_mempool(tip)).collect()
    }

    /// All pending entries regardless of state, for drivers and tests.
    pub fn pending_txs(&self) -> &[PendingTx] {
        &self.pending
    }

    /// Whether declared content sits beyond the current tip: unfabricated
    /// schedule entries, or fabricated sends waiting for a mining height.
    /// Live mode reaches them per tick. In scenario mode this is a
    /// start-time error, because no tick exists to ever reach them.
    pub fn has_unrealized_schedule(&self) -> bool {
        self.schedule.iter().any(|ev| !ev.done)
            || self.pending.iter().any(|p| {
                p.state == PendingState::Pending
                    && p.source == TxSource::Fabricated
                    && p.mine_at.is_some()
            })
    }

    /// Look up a transaction by txid, mined blocks first, then mempool.
    pub fn transaction(&self, txid: &TxId) -> Option<TxLookup<'_>> {
        for block in self.blocks.iter().rev() {
            for mined in &block.txs {
                if mined.txid == *txid {
                    return Some(TxLookup {
                        raw: &mined.raw,
                        height: Some(block.height),
                    });
                }
            }
        }
        let tip = self.tip_height();
        self.pending
            .iter()
            .find(|p| p.txid == *txid && p.in_mempool(tip))
            .map(|p| TxLookup {
                raw: &p.raw,
                height: None,
            })
    }

    /// The transparent state: UTXOs and address indexes.
    pub fn utxo_set(&self) -> &UtxoSet {
        &self.utxos
    }

    // --- Ground truth ---

    /// Total expected balance: every shielded pool plus the default
    /// transparent address.
    pub fn expected_balance(&self, account: &str) -> Result<u64> {
        let idx = self.require_account(account)?;
        let shielded: u64 = self
            .notes
            .iter()
            .filter(|r| r.account == idx && r.spent.is_none())
            .map(|r| r.value)
            .sum();
        Ok(shielded + self.utxos.balance(self.accounts[idx].taddr()))
    }

    /// Expected balance in one shielded pool.
    pub fn expected_balance_in(&self, account: &str, pool: Pool) -> Result<u64> {
        let idx = self.require_account(account)?;
        Ok(self
            .notes
            .iter()
            .filter(|r| r.account == idx && r.pool == pool && r.spent.is_none())
            .map(|r| r.value)
            .sum())
    }

    /// Expected balance on the account's default transparent address.
    pub fn expected_transparent_balance(&self, account: &str) -> Result<u64> {
        let idx = self.require_account(account)?;
        Ok(self.utxos.balance(self.accounts[idx].taddr()))
    }

    /// Every note record for the account, spent and unspent.
    pub fn notes_for(&self, account: &str) -> Result<Vec<&NoteRecord>> {
        let idx = self.require_account(account)?;
        Ok(self.notes.iter().filter(|r| r.account == idx).collect())
    }

    /// Unspent notes mined at or below `at_height`.
    pub fn spendable_notes(&self, account: &str, at_height: u32) -> Result<Vec<&NoteRecord>> {
        let idx = self.require_account(account)?;
        Ok(self
            .notes
            .iter()
            .filter(|r| r.account == idx && r.spent.is_none() && r.height <= at_height)
            .collect())
    }

    /// UTXOs on the account's default transparent address.
    pub fn utxos_for(&self, account: &str) -> Result<Vec<&Utxo>> {
        let idx = self.require_account(account)?;
        Ok(self.utxos.utxos_for(self.accounts[idx].taddr()))
    }

    /// Outgoing payments recovered through the account's OVK, including
    /// payments to undeclared recipients.
    pub fn outgoing_payments(&self, account: &str) -> Result<Vec<&OutgoingPayment>> {
        let idx = self.require_account(account)?;
        Ok(self.outgoing.iter().filter(|p| p.from == idx).collect())
    }
}
