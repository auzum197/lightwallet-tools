//! The per-chain UTXO set and transparent address index.
//!
//! Two consumer paths must stay consistent: `vin`/`vout` in the compact
//! block stream, and the address queries served from here. Both derive
//! from the same applied transactions, so they cannot disagree.

use std::collections::{BTreeMap, BTreeSet};

use zcash_protocol::TxId;
use zcash_protocol::value::Zatoshis;
use zcash_transparent::address::{Script, TransparentAddress};
use zcash_transparent::bundle::{Authorization, Bundle as TransparentBundle, OutPoint};

/// One unspent transparent output.
#[derive(Clone, Debug)]
pub struct Utxo {
    /// The output's location.
    pub outpoint: OutPoint,
    /// Locking script, verbatim.
    pub script: Script,
    /// Output value.
    pub value: Zatoshis,
    /// Height of the block that mined it.
    pub height: u32,
    /// Address derived from the script. `None` for script kinds other than
    /// P2PKH/P2SH, which are held but invisible to address queries.
    pub address: Option<TransparentAddress>,
}

/// The UTXO set plus the two address indexes `GetTaddress*` queries read.
#[derive(Clone, Default)]
pub struct UtxoSet {
    utxos: BTreeMap<OutPoint, Utxo>,
    by_address: BTreeMap<TransparentAddress, BTreeSet<OutPoint>>,
    // Spends AND receives: GetTaddressTransactions returns full history.
    txids_by_address: BTreeMap<TransparentAddress, Vec<(TxId, u32)>>,
    coinbase_outputs: BTreeSet<OutPoint>,
}

impl UtxoSet {
    /// Apply one mined transaction's transparent bundle, inputs first.
    ///
    /// Inputs referencing unknown outpoints are skipped, not rejected:
    /// funding transactions and wallet transactions spending outside the
    /// darkside's view are both legitimate here.
    pub(crate) fn apply_tx<A: Authorization>(
        &mut self,
        txid: TxId,
        height: u32,
        tx_index: usize,
        bundle: &TransparentBundle<A>,
    ) {
        for txin in &bundle.vin {
            if let Some(spent) = self.utxos.remove(txin.prevout())
                && let Some(addr) = spent.address
            {
                if let Some(set) = self.by_address.get_mut(&addr) {
                    set.remove(&spent.outpoint);
                }
                self.record_txid(addr, txid, height);
            }
        }
        for (n, txout) in bundle.vout.iter().enumerate() {
            let outpoint = OutPoint::new(*txid.as_ref(), n as u32);
            let address = txout.recipient_address();
            if let Some(addr) = address {
                self.by_address
                    .entry(addr)
                    .or_default()
                    .insert(outpoint.clone());
                self.record_txid(addr, txid, height);
            }
            if tx_index == 0 {
                self.coinbase_outputs.insert(outpoint.clone());
            }
            self.utxos.insert(
                outpoint.clone(),
                Utxo {
                    outpoint,
                    script: txout.script_pubkey().clone(),
                    value: txout.value(),
                    height,
                    address,
                },
            );
        }
    }

    fn record_txid(&mut self, addr: TransparentAddress, txid: TxId, height: u32) {
        let history = self.txids_by_address.entry(addr).or_default();
        if history.last() != Some(&(txid, height)) {
            history.push((txid, height));
        }
    }

    /// Unspent outputs paying `addr`, in outpoint order.
    pub fn utxos_for(&self, addr: &TransparentAddress) -> Vec<&Utxo> {
        self.by_address
            .get(addr)
            .into_iter()
            .flatten()
            .filter_map(|op| self.utxos.get(op))
            .collect()
    }

    /// Sum of unspent value paying `addr`, in zatoshis.
    pub fn balance(&self, addr: &TransparentAddress) -> u64 {
        self.utxos_for(addr)
            .iter()
            .map(|u| u.value.into_u64())
            .sum()
    }

    /// Full transaction history touching `addr`: receives and spends.
    pub fn txids_for(&self, addr: &TransparentAddress) -> &[(TxId, u32)] {
        self.txids_by_address
            .get(addr)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// Whether an outpoint was created by a coinbase transaction
    /// (`tx_index == 0`), for maturity-aware queries.
    pub fn is_coinbase(&self, outpoint: &OutPoint) -> bool {
        self.coinbase_outputs.contains(outpoint)
    }

    /// Insert a UTXO no transaction created: the `corrupt_utxo` escape
    /// hatch. Address queries will report funds the block stream
    /// never showed.
    pub(crate) fn insert_phantom(&mut self, utxo: Utxo) {
        if let Some(addr) = utxo.address {
            self.by_address
                .entry(addr)
                .or_default()
                .insert(utxo.outpoint.clone());
        }
        self.utxos.insert(utxo.outpoint.clone(), utxo);
    }
}
