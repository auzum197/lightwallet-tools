//! Declared accounts: seed-derived key lineages, not wallets.

use zcash_keys::address::UnifiedAddress;
use zcash_keys::keys::{UnifiedAddressRequest, UnifiedFullViewingKey, UnifiedSpendingKey};
use zcash_transparent::address::TransparentAddress;
use zcash_transparent::keys::IncomingViewingKey as _;
use zip32::AccountId;

use crate::error::{Error, Result};
use crate::params::ChainParams;

/// A declared account: name plus the retained viewing keys.
///
/// The spending key is derived, used to produce the UFVK, and dropped. The
/// UFVK's `nk` and `ovk` are all the fabricator and the scanner need.
/// Spend authority never exists inside darkside.
#[derive(Clone)]
pub struct Account {
    name: String,
    ufvk: UnifiedFullViewingKey,
    ua: UnifiedAddress,
    taddr: TransparentAddress,
}

impl Account {
    /// Derive an account from a declared seed string via standard ZIP-32,
    /// retaining viewing keys only.
    ///
    /// The seed string maps to seed bytes by the rule in [`seed_bytes`], so
    /// an external harness (or a wallet importing the seed) derives
    /// identical keys with no channel to darkside.
    pub fn derive(
        params: &ChainParams,
        name: &str,
        seed_phrase: &str,
        account_index: u32,
    ) -> Result<Self> {
        let seed = seed_bytes(seed_phrase);
        let account = AccountId::try_from(account_index).map_err(|_| {
            Error::Derivation(format!("account index {account_index} out of range"))
        })?;
        let usk = UnifiedSpendingKey::from_seed(&params.network, &seed, account)
            .map_err(|e| Error::Derivation(e.to_string()))?;
        let ufvk = usk.to_unified_full_viewing_key();
        let (ua, _) = ufvk
            .default_address(UnifiedAddressRequest::AllAvailableKeys)
            .map_err(|e| Error::Derivation(e.to_string()))?;
        let taddr = ufvk
            .transparent()
            .ok_or_else(|| Error::Derivation("UFVK lacks a transparent component".into()))?
            .derive_external_ivk()
            .map_err(|e| Error::Derivation(e.to_string()))?
            .default_address()
            .0;
        Ok(Account {
            name: name.to_owned(),
            ufvk,
            ua,
            taddr,
        })
    }

    /// The declared name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The retained unified full viewing key.
    pub fn ufvk(&self) -> &UnifiedFullViewingKey {
        &self.ufvk
    }

    /// The account's default unified address.
    pub fn ua(&self) -> &UnifiedAddress {
        &self.ua
    }

    /// The account's default external transparent address.
    pub fn taddr(&self) -> &TransparentAddress {
        &self.taddr
    }
}

impl core::fmt::Debug for Account {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Account").field("name", &self.name).finish()
    }
}

/// Map a declared seed string to ZIP-32 seed bytes.
///
/// A `0x`-prefixed hex string of 32 to 252 bytes is used verbatim, so a
/// wallet that imports raw seeds can become the account exactly. Any other
/// string is stretched to 64 bytes with BLAKE2b (personalization
/// `darkside_zip32sd`), a rule an external harness can replicate.
pub fn seed_bytes(seed_phrase: &str) -> Vec<u8> {
    if let Some(hex_str) = seed_phrase.strip_prefix("0x")
        && let Ok(bytes) = hex::decode(hex_str)
        && (32..=252).contains(&bytes.len())
    {
        return bytes;
    }
    blake2b_simd::Params::new()
        .hash_length(64)
        .personal(b"darkside_zip32sd")
        .hash(seed_phrase.as_bytes())
        .as_bytes()
        .to_vec()
}
