//! Note-commitment trees, one per shielded pool.
//!
//! Three structurally identical trees (`DEPTH` 32, `SHARD_HEIGHT` 16) with
//! different contents. Sapling's node type differs from Orchard's, so the
//! compiler separates those two. Orchard and Ironwood share a node type and
//! are kept apart as distinct named types, because an Ironwood commitment
//! appended to the Orchard tree is a silent, unrecoverable corruption.

use std::collections::BTreeMap;

use incrementalmerkletree::frontier::{CommitmentTree, Frontier};
use incrementalmerkletree::{Hashable, Retention};
use orchard::tree::MerkleHashOrchard;
use sapling_crypto::Node as SaplingNode;
use shardtree::ShardTree;
use shardtree::store::ShardStore;
use shardtree::store::memory::MemoryShardStore;
use zcash_primitives::merkle_tree::{HashSer, write_commitment_tree};

pub(crate) const TREE_DEPTH: u8 = 32;
pub(crate) const SHARD_HEIGHT: u8 = 16;
const SUBTREE_SIZE: u64 = 1 << (SHARD_HEIGHT as u64);

/// A completed subtree root plus its completion metadata, recorded at mine
/// time because `shardtree` has no notion of blocks.
#[derive(Clone, Debug)]
pub struct SubtreeRoot {
    /// Subtree index at level 16.
    pub index: u64,
    /// Root hash of the completed subtree.
    pub root_hash: [u8; 32],
    /// Height of the block whose commitments completed the subtree.
    pub completing_height: u32,
    /// Hash of that block.
    pub completing_hash: [u8; 32],
}

/// One pool's tree state: the shard tree, a per-block frontier snapshot for
/// `GetTreeState`, and the completed-subtree record for `GetSubtreeRoots`.
pub(crate) struct PoolTree<H: Hashable + Clone + PartialEq> {
    tree: ShardTree<MemoryShardStore<H, u32>, TREE_DEPTH, SHARD_HEIGHT>,
    size: u64,
    frontiers: BTreeMap<u32, Frontier<H, TREE_DEPTH>>,
    subtree_roots: Vec<SubtreeRoot>,
}

pub(crate) trait NodeBytes {
    fn node_bytes(&self) -> [u8; 32];
}

impl NodeBytes for SaplingNode {
    fn node_bytes(&self) -> [u8; 32] {
        self.to_bytes()
    }
}

impl NodeBytes for MerkleHashOrchard {
    fn node_bytes(&self) -> [u8; 32] {
        self.to_bytes()
    }
}

impl<H: Hashable + HashSer + NodeBytes + Clone + PartialEq> PoolTree<H> {
    pub(crate) fn new() -> Self {
        PoolTree {
            // Checkpoints back every GetTreeState height, so never prune.
            tree: ShardTree::new(MemoryShardStore::empty(), usize::MAX),
            size: 0,
            frontiers: BTreeMap::new(),
            subtree_roots: Vec::new(),
        }
    }

    /// Append one commitment, returning its leaf position. Every leaf is
    /// marked: ephemeral leaves get pruned, and darkside must keep the
    /// whole tree to serve frontiers and subtree roots for any height.
    pub(crate) fn append(&mut self, node: H) -> u64 {
        self.tree
            .append(node, Retention::Marked)
            .expect("in-memory append on a never-pruned tree cannot fail");
        let position = self.size;
        self.size += 1;
        position
    }

    /// Close out a block: checkpoint at `height`, snapshot the frontier,
    /// and record any subtree this block's commitments completed.
    pub(crate) fn end_block(&mut self, height: u32, block_hash: [u8; 32]) {
        self.tree
            .checkpoint(height)
            .expect("in-memory checkpoint cannot fail");
        let frontier = self
            .tree
            .frontier()
            .expect("in-memory frontier query cannot fail");
        self.frontiers.insert(height, frontier);

        while (self.subtree_roots.len() as u64) < self.size / SUBTREE_SIZE {
            let index = self.subtree_roots.len() as u64;
            let addr = incrementalmerkletree::Address::from_parts(
                incrementalmerkletree::Level::from(SHARD_HEIGHT),
                index,
            );
            let shard = self
                .tree
                .store()
                .get_shard(addr)
                .expect("in-memory store is infallible")
                .expect("completed shard exists");
            let root = shard
                .root_hash(addr.position_range_end())
                .expect("completed shard is dense");
            self.subtree_roots.push(SubtreeRoot {
                index,
                root_hash: root.node_bytes(),
                completing_height: height,
                completing_hash: block_hash,
            });
        }
    }

    /// The tree root as of the checkpoint at `height`.
    pub(crate) fn root_at(&self, height: u32) -> Option<H> {
        self.tree.root_at_checkpoint_id(&height).ok().flatten()
    }

    /// Number of leaves as of the checkpoint at `height`.
    pub(crate) fn size_at(&self, height: u32) -> Option<u64> {
        self.frontier_at(height).map(Frontier::tree_size)
    }

    /// The frontier as of `height`, which is the one recorded at the nearest
    /// mined height at or below it. A height inside an unmined span between
    /// mined blocks resolves to the one below it, since the empty blocks
    /// spanning it append no commitments and so leave the tree alone.
    fn frontier_at(&self, height: u32) -> Option<&Frontier<H, TREE_DEPTH>> {
        self.frontiers
            .range(..=height)
            .next_back()
            .map(|(_, frontier)| frontier)
    }

    /// The legacy zcashd `CommitmentTree` serialization as of `height`,
    /// the format lightwalletd serves in `TreeState`.
    pub(crate) fn tree_state_bytes(&self, height: u32) -> Option<Vec<u8>> {
        let frontier = self.frontier_at(height)?;
        let legacy = CommitmentTree::from_frontier(frontier);
        let mut bytes = Vec::new();
        write_commitment_tree(&legacy, &mut bytes).expect("writing to a Vec cannot fail");
        Some(bytes)
    }

    /// The legacy serialization of an empty tree, for prehistory heights
    /// below the boot height, where no commitment has ever been appended.
    pub(crate) fn empty_state_bytes() -> Vec<u8> {
        let legacy = CommitmentTree::from_frontier(&Frontier::<H, TREE_DEPTH>::empty());
        let mut bytes = Vec::new();
        write_commitment_tree(&legacy, &mut bytes).expect("writing to a Vec cannot fail");
        bytes
    }

    /// Completed subtree roots, oldest first.
    pub(crate) fn subtree_roots(&self) -> &[SubtreeRoot] {
        &self.subtree_roots
    }

    /// Damage the stored frontier for `height`: the `corrupt_tree_state`
    /// escape hatch. The served tree state will disagree with the
    /// commitments in the block stream.
    pub(crate) fn corrupt_frontier(&mut self, height: u32, garbage: H) -> bool {
        match self.frontiers.get_mut(&height) {
            Some(frontier) => {
                frontier.append(garbage);
                true
            }
            None => false,
        }
    }
}

/// The Sapling note-commitment tree.
pub(crate) struct SaplingTree(pub(crate) PoolTree<SaplingNode>);

/// The Orchard note-commitment tree.
pub(crate) struct OrchardTree(pub(crate) PoolTree<MerkleHashOrchard>);

/// The Ironwood note-commitment tree. Same shape as Orchard's, different
/// tree. A distinct type so the two cannot be swapped by accident.
pub(crate) struct IronwoodTree(pub(crate) PoolTree<MerkleHashOrchard>);
