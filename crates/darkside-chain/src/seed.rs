//! The chain seed: the single source of randomness in the state machine.

use rand_chacha::ChaCha20Rng;
use rand_chacha::rand_core::SeedableRng;

/// Seed for a [`crate::Chain`]. Same seed, byte-identical chain: every
/// fabricated `rseed`, ephemeral key, and value-commitment trapdoor is drawn
/// from an RNG rooted here. A test failure reproduces from this one value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Seed([u8; 32]);

impl Seed {
    /// An RNG for one labeled purpose. Each (label, index) pair gets an
    /// independent stream, so adding a draw in one code path cannot shift
    /// the bytes another path sees.
    pub(crate) fn rng_for(&self, label: &str, index: u64) -> ChaCha20Rng {
        let digest = blake2b_simd::Params::new()
            .hash_length(32)
            .key(&self.0)
            .personal(b"darkside_subseed")
            .to_state()
            .update(label.as_bytes())
            .update(&index.to_le_bytes())
            .finalize();
        let mut key = [0u8; 32];
        key.copy_from_slice(digest.as_bytes());
        ChaCha20Rng::from_seed(key)
    }
}

impl From<u64> for Seed {
    fn from(v: u64) -> Self {
        let mut bytes = [0u8; 32];
        bytes[..8].copy_from_slice(&v.to_le_bytes());
        Seed(bytes)
    }
}

impl From<[u8; 32]> for Seed {
    fn from(bytes: [u8; 32]) -> Self {
        Seed(bytes)
    }
}
