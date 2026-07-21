//! Prepared trial-decryption keys for every declared account.

use sapling_crypto::keys::{
    OutgoingViewingKey as SaplingOvk, PreparedIncomingViewingKey as SaplingPreparedIvk,
};
use sapling_crypto::zip32::DiversifiableFullViewingKey;
use zip32::Scope;

use orchard::keys::{
    FullViewingKey as OrchardFvk, OutgoingViewingKey as OrchardOvk,
    PreparedIncomingViewingKey as OrchardPreparedIvk,
};

use crate::account::Account;

const SCOPES: [Scope; 2] = [Scope::External, Scope::Internal];

/// One account's Sapling scan keys under one scope.
pub(crate) struct SaplingScanKey {
    pub account: usize,
    pub scope: Scope,
    pub ivk: SaplingPreparedIvk,
    pub dfvk: DiversifiableFullViewingKey,
}

/// One account's Orchard scan keys under one scope. Ironwood shares key
/// material with Orchard. Only the note-encryption domain differs.
pub(crate) struct OrchardScanKey {
    pub account: usize,
    pub ivk: OrchardPreparedIvk,
    pub fvk: OrchardFvk,
}

/// Everything trial decryption and OVK recovery need, precomputed from the
/// declared accounts' UFVKs.
#[derive(Default)]
pub(crate) struct ScanKeys {
    pub sapling_ivks: Vec<SaplingScanKey>,
    pub sapling_ovks: Vec<(usize, SaplingOvk)>,
    pub orchard_ivks: Vec<OrchardScanKey>,
    pub orchard_ovks: Vec<(usize, OrchardOvk)>,
}

impl ScanKeys {
    pub(crate) fn build(accounts: &[Account]) -> Self {
        let mut keys = ScanKeys::default();
        for (idx, account) in accounts.iter().enumerate() {
            if let Some(dfvk) = account.ufvk().sapling() {
                for scope in SCOPES {
                    keys.sapling_ivks.push(SaplingScanKey {
                        account: idx,
                        scope,
                        ivk: SaplingPreparedIvk::new(&dfvk.to_ivk(scope)),
                        dfvk: dfvk.clone(),
                    });
                    keys.sapling_ovks.push((idx, dfvk.to_ovk(scope)));
                }
            }
            if let Some(fvk) = account.ufvk().orchard() {
                for scope in SCOPES {
                    keys.orchard_ivks.push(OrchardScanKey {
                        account: idx,
                        ivk: OrchardPreparedIvk::new(&fvk.to_ivk(scope)),
                        fvk: fvk.clone(),
                    });
                    keys.orchard_ovks.push((idx, fvk.to_ovk(scope)));
                }
            }
        }
        keys
    }
}
