//! The transaction fabricator: real notes, real nullifiers, dummy proofs
//! and signatures. Nothing here holds spend authority. Nothing downstream
//! verifies authorization, and the one thing consumers do
//! check, the nullifier, is supplied by the caller from the note records.

use rand::RngCore;
use rand_chacha::ChaCha20Rng;

use sapling_crypto::bundle::{
    Authorized as SaplingAuthorized, Bundle as SaplingBundle, GrothProofBytes, OutputDescription,
    SpendDescription,
};
use sapling_crypto::keys::OutgoingViewingKey as SaplingOvk;
use sapling_crypto::note_encryption::{SaplingDomain, sapling_note_encryption};
use sapling_crypto::value::{
    NoteValue as SaplingValue, ValueCommitTrapdoor as SaplingTrapdoor,
    ValueCommitment as SaplingValueCommitment,
};
use sapling_crypto::{PaymentAddress, Rseed};

use orchard::bundle::{Authorized as OrchardAuthorized, Bundle as OrchardBundle, BundleVersion};
use orchard::keys::OutgoingViewingKey as OrchardOvk;
use orchard::note::{
    ExtractedNoteCommitment, Note as OrchardNote, NoteVersion, Nullifier as OrchardNullifier,
    RandomSeed, Rho, TransmittedNoteCiphertext,
};
use orchard::note_encryption::{
    IronwoodDomain, IronwoodNoteEncryption, OrchardDomain, OrchardNoteEncryption,
};
use orchard::primitives::redpallas;
use orchard::value::{
    NoteValue as OrchardValue, ValueCommitTrapdoor as OrchardTrapdoor,
    ValueCommitment as OrchardValueCommitment,
};
use orchard::{Action, Anchor as OrchardAnchor};

use nonempty::NonEmpty;
use zcash_note_encryption::Domain;
use zcash_primitives::transaction::{Authorized, Transaction, TransactionData, TxVersion};
use zcash_protocol::TxId;
use zcash_protocol::consensus::BlockHeight;
use zcash_protocol::value::ZatBalance;
use zcash_transparent::address::{Script, TransparentAddress};
use zcash_transparent::bundle::{
    Authorized as TransparentAuthorized, Bundle as TransparentBundle, OutPoint, TxIn, TxOut,
};

use crate::error::{Error, Result};
use crate::notes::Pool;
use crate::params::ChainParams;

/// A spend of a previously mined note: its real nullifier and its value.
pub(crate) struct SpendInput {
    pub nullifier: [u8; 32],
    pub value: u64,
}

/// One planned Sapling output.
pub(crate) struct SaplingOut {
    pub addr: PaymentAddress,
    pub value: u64,
    pub ovk: Option<SaplingOvk>,
    /// Serve a decoy commitment instead of the note's own (`corrupt
    /// commitment`). Change outputs stay clean.
    pub corrupt_cmx: bool,
}

/// One planned Orchard or Ironwood output.
pub(crate) struct OrchardOut {
    pub addr: orchard::Address,
    pub value: u64,
    pub ovk: Option<OrchardOvk>,
    /// Serve a decoy commitment instead of the note's own.
    pub corrupt_cmx: bool,
}

/// The empty memo: 0xF6 followed by zeros, what wallets write when the
/// user typed nothing.
fn empty_memo() -> [u8; 512] {
    let mut memo = [0u8; 512];
    memo[0] = 0xf6;
    memo
}

fn dummy_sapling_rk(rng: &mut ChaCha20Rng) -> redjubjub::VerificationKey<redjubjub::SpendAuth> {
    let sk = redjubjub::SigningKey::new(rng);
    redjubjub::VerificationKey::from(&sk)
}

fn dummy_orchard_rk(rng: &mut ChaCha20Rng) -> redpallas::VerificationKey<redpallas::SpendAuth> {
    loop {
        let mut bytes = [0u8; 32];
        rng.fill_bytes(&mut bytes);
        if let Ok(sk) = redpallas::SigningKey::try_from(bytes) {
            return redpallas::VerificationKey::from(&sk);
        }
    }
}

fn random_orchard_nullifier(rng: &mut ChaCha20Rng) -> OrchardNullifier {
    loop {
        let mut bytes = [0u8; 32];
        rng.fill_bytes(&mut bytes);
        if let Some(nf) = Option::from(OrchardNullifier::from_bytes(&bytes)) {
            return nf;
        }
    }
}

fn orchard_note(
    rng: &mut ChaCha20Rng,
    recipient: orchard::Address,
    value: u64,
    rho: Rho,
    version: NoteVersion,
) -> OrchardNote {
    loop {
        let mut rseed_bytes = [0u8; 32];
        rng.fill_bytes(&mut rseed_bytes);
        let Some(rseed) = Option::from(RandomSeed::from_bytes(rseed_bytes, &rho)) else {
            continue;
        };
        let note = OrchardNote::from_parts(
            recipient,
            OrchardValue::from_raw(value),
            rho,
            rseed,
            version,
        );
        if let Some(note) = Option::from(note) {
            return note;
        }
    }
}

fn orchard_trapdoor(rng: &mut ChaCha20Rng) -> OrchardTrapdoor {
    loop {
        let mut bytes = [0u8; 32];
        rng.fill_bytes(&mut bytes);
        if let Some(t) = Option::from(OrchardTrapdoor::from_bytes(bytes)) {
            return t;
        }
    }
}

/// Fabricate a Sapling bundle: spends carry caller-supplied real
/// nullifiers, outputs are real encrypted notes, proofs and signatures are
/// dummies.
pub(crate) fn sapling_bundle(
    rng: &mut ChaCha20Rng,
    anchor: bls12_381::Scalar,
    spends: &[SpendInput],
    outputs: &[SaplingOut],
    corrupt_spends: bool,
) -> Option<SaplingBundle<SaplingAuthorized, ZatBalance>> {
    let shielded_spends = spends
        .iter()
        .map(|spend| {
            let nf = if corrupt_spends {
                let mut bytes = [0u8; 32];
                rng.fill_bytes(&mut bytes);
                sapling_crypto::Nullifier(bytes)
            } else {
                sapling_crypto::Nullifier(spend.nullifier)
            };
            let cv = SaplingValueCommitment::derive(
                SaplingValue::from_raw(spend.value),
                SaplingTrapdoor::random(&mut *rng),
            );
            SpendDescription::from_parts(
                cv,
                anchor,
                nf,
                dummy_sapling_rk(rng),
                [0u8; 192] as GrothProofBytes,
                redjubjub::Signature::from([0u8; 64]),
            )
        })
        .collect::<Vec<_>>();

    let shielded_outputs = outputs
        .iter()
        .map(|out| {
            let mut rseed_bytes = [0u8; 32];
            rng.fill_bytes(&mut rseed_bytes);
            let note = out.addr.create_note(
                SaplingValue::from_raw(out.value),
                Rseed::AfterZip212(rseed_bytes),
            );
            let cv =
                SaplingValueCommitment::derive(note.value(), SaplingTrapdoor::random(&mut *rng));
            let cmu = if out.corrupt_cmx {
                // A decoy note's commitment: canonical, plausible, and not
                // the commitment of the note the ciphertext decrypts to.
                let mut decoy_rseed = [0u8; 32];
                rng.fill_bytes(&mut decoy_rseed);
                out.addr
                    .create_note(
                        SaplingValue::from_raw(out.value + 1),
                        Rseed::AfterZip212(decoy_rseed),
                    )
                    .cmu()
            } else {
                note.cmu()
            };
            let encryptor = sapling_note_encryption(out.ovk, note, empty_memo(), &mut *rng);
            let ephemeral_key = SaplingDomain::epk_bytes(encryptor.epk());
            let enc_ciphertext = encryptor.encrypt_note_plaintext();
            let out_ciphertext = encryptor.encrypt_outgoing_plaintext(&cv, &cmu, &mut *rng);
            OutputDescription::from_parts(
                cv,
                cmu,
                ephemeral_key,
                enc_ciphertext,
                out_ciphertext,
                [0u8; 192] as GrothProofBytes,
            )
        })
        .collect::<Vec<_>>();

    let balance = spends.iter().map(|s| s.value as i64).sum::<i64>()
        - outputs.iter().map(|o| o.value as i64).sum::<i64>();
    SaplingBundle::from_parts(
        shielded_spends,
        shielded_outputs,
        ZatBalance::from_i64(balance).expect("fabricated values stay in range"),
        SaplingAuthorized {
            binding_sig: redjubjub::Signature::from([0u8; 64]),
        },
    )
}

/// Fabricate an Orchard or Ironwood bundle. The pool decides the note
/// version and note-encryption domain. The bundle version comes from the
/// caller, derived from the consensus branch in force.
#[allow(clippy::too_many_arguments)]
pub(crate) fn orchard_bundle(
    rng: &mut ChaCha20Rng,
    pool: Pool,
    bundle_version: BundleVersion,
    anchor: OrchardAnchor,
    spends: &[SpendInput],
    outputs: &[OrchardOut],
    dummy_recipient: orchard::Address,
    corrupt_spends: bool,
) -> Option<OrchardBundle<OrchardAuthorized, ZatBalance>> {
    if spends.is_empty() && outputs.is_empty() {
        return None;
    }
    let note_version = match pool {
        Pool::Ironwood => NoteVersion::V3,
        _ => NoteVersion::V2,
    };
    let n_actions = spends.len().max(outputs.len()).max(2);

    let actions = (0..n_actions)
        .map(|i| {
            let (nf, spend_value) = match spends.get(i) {
                Some(spend) if !corrupt_spends => {
                    let nf = OrchardNullifier::from_bytes(&spend.nullifier);
                    (
                        Option::<OrchardNullifier>::from(nf)
                            .expect("recorded nullifiers are canonical"),
                        spend.value,
                    )
                }
                Some(spend) => (random_orchard_nullifier(rng), spend.value),
                None => (random_orchard_nullifier(rng), 0),
            };
            let rho = Option::<Rho>::from(Rho::from_bytes(&nf.to_bytes()))
                .expect("a nullifier is a canonical rho");
            let (addr, out_value, ovk, corrupt_cmx) = match outputs.get(i) {
                Some(out) => (out.addr, out.value, out.ovk.clone(), out.corrupt_cmx),
                None => (dummy_recipient, 0, None, false),
            };
            let note = orchard_note(rng, addr, out_value, rho, note_version);
            let cmx = if corrupt_cmx {
                let decoy = orchard_note(rng, addr, out_value + 1, rho, note_version);
                ExtractedNoteCommitment::from(decoy.commitment())
            } else {
                ExtractedNoteCommitment::from(note.commitment())
            };
            let cv_net = OrchardValueCommitment::derive(
                OrchardValue::from_raw(spend_value) - OrchardValue::from_raw(out_value),
                orchard_trapdoor(rng),
            );
            let encrypted_note = match pool {
                Pool::Ironwood => {
                    let enc = IronwoodNoteEncryption::new(ovk, note, empty_memo());
                    TransmittedNoteCiphertext {
                        epk_bytes: IronwoodDomain::epk_bytes(enc.epk()).0,
                        enc_ciphertext: enc.encrypt_note_plaintext(),
                        out_ciphertext: enc.encrypt_outgoing_plaintext(&cv_net, &cmx, &mut *rng),
                    }
                }
                _ => {
                    let enc = OrchardNoteEncryption::new(ovk, note, empty_memo());
                    TransmittedNoteCiphertext {
                        epk_bytes: OrchardDomain::epk_bytes(enc.epk()).0,
                        enc_ciphertext: enc.encrypt_note_plaintext(),
                        out_ciphertext: enc.encrypt_outgoing_plaintext(&cv_net, &cmx, &mut *rng),
                    }
                }
            };
            Action::from_parts(
                nf,
                dummy_orchard_rk(rng),
                cmx,
                encrypted_note,
                cv_net,
                redpallas::Signature::from([0u8; 64]),
            )
            .expect("real encryption gives a valid epk and the rk is non-identity")
        })
        .collect::<Vec<_>>();

    let balance = spends.iter().map(|s| s.value as i64).sum::<i64>()
        - outputs.iter().map(|o| o.value as i64).sum::<i64>();
    let proof = orchard::Proof::new(vec![0u8; orchard::Proof::expected_proof_size(n_actions)]);
    let bundle = OrchardBundle::try_from_parts(
        NonEmpty::from_vec(actions).expect("n_actions is at least 2"),
        bundle_version.default_flags(),
        ZatBalance::from_i64(balance).expect("fabricated values stay in range"),
        anchor,
        OrchardAuthorized::from_parts(proof, redpallas::Signature::from([0u8; 64])),
        bundle_version,
    )
    .expect("canonical proof size and representable flags");
    Some(bundle)
}

/// A transparent bundle paying the given addresses, with no inputs. By both
/// coinbase tests this is not coinbase, so the outputs are immediately
/// spendable.
pub(crate) fn transparent_fund_bundle(
    outputs: &[(TransparentAddress, u64)],
) -> Result<TransparentBundle<TransparentAuthorized>> {
    Ok(TransparentBundle {
        vin: Vec::new(),
        vout: transparent_outputs(outputs)?,
        authorization: TransparentAuthorized,
    })
}

/// A coinbase bundle: exactly one null-prevout input plus the outputs.
/// Placed at index 0, so wallets apply the 100-block maturity rule.
pub(crate) fn coinbase_bundle(
    height: u32,
    outputs: &[(TransparentAddress, u64)],
) -> Result<TransparentBundle<TransparentAuthorized>> {
    Ok(TransparentBundle {
        vin: vec![TxIn::from_parts(
            OutPoint::NULL,
            bip34_script_sig(height),
            u32::MAX,
        )],
        vout: transparent_outputs(outputs)?,
        authorization: TransparentAuthorized,
    })
}

fn transparent_outputs(outputs: &[(TransparentAddress, u64)]) -> Result<Vec<TxOut>> {
    outputs
        .iter()
        .map(|(addr, value)| {
            let zats = zcash_protocol::value::Zatoshis::from_u64(*value)
                .map_err(|_| Error::Amount(format!("{value} zatoshis out of range")))?;
            Ok(TxOut::new(zats, Script::from(addr.script())))
        })
        .collect()
}

/// BIP-34 style script sig: a minimal push of the block height.
fn bip34_script_sig(height: u32) -> Script {
    let mut le = height.to_le_bytes().to_vec();
    while le.len() > 1 && le[le.len() - 1] == 0 && le[le.len() - 2] & 0x80 == 0 {
        le.pop();
    }
    if le[le.len() - 1] & 0x80 != 0 {
        le.push(0);
    }
    let mut raw = vec![le.len() as u8];
    raw.extend_from_slice(&le);
    script_from_raw(&raw)
}

/// Build a `Script` from raw opcode bytes via its serialized form.
fn script_from_raw(bytes: &[u8]) -> Script {
    let mut serialized = vec![bytes.len() as u8];
    serialized.extend_from_slice(bytes);
    Script::read(&serialized[..]).expect("length-prefixed bytes always parse")
}

/// Assemble bundles into a frozen transaction with the version the branch
/// in force suggests. Ironwood bundles require the V6 format. Activation
/// checks upstream guarantee the branch supports it.
pub(crate) fn assemble(
    params: &ChainParams,
    mine_height: u32,
    expiry_height: u32,
    transparent: Option<TransparentBundle<TransparentAuthorized>>,
    sapling: Option<SaplingBundle<SaplingAuthorized, ZatBalance>>,
    orchard: Option<OrchardBundle<OrchardAuthorized, ZatBalance>>,
    ironwood: Option<OrchardBundle<OrchardAuthorized, ZatBalance>>,
) -> Result<(Transaction, Vec<u8>, TxId)> {
    let branch = params.branch_id(mine_height);
    let version = TxVersion::suggested_for_branch(branch);
    let data: TransactionData<Authorized> = if version == TxVersion::V6 {
        TransactionData::from_parts_v6(
            branch,
            0,
            BlockHeight::from_u32(expiry_height),
            transparent,
            sapling,
            orchard,
            ironwood,
        )
    } else {
        debug_assert!(ironwood.is_none(), "ironwood bundles require V6");
        TransactionData::from_parts(
            version,
            branch,
            0,
            BlockHeight::from_u32(expiry_height),
            transparent,
            None,
            sapling,
            orchard,
        )
    };
    let tx = data.freeze().map_err(Error::TxParse)?;
    let mut raw = Vec::new();
    tx.write(&mut raw).map_err(Error::TxParse)?;
    let txid = tx.txid();
    Ok((tx, raw, txid))
}
