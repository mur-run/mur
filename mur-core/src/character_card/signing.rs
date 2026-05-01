//! Ed25519 sign / verify for `MurCard`.
//!
//! Signs `canonical_data_bytes(&card)` — the canonical JSON of the
//! card with the signature block stripped. Public key + signature are
//! multibase-encoded (z-prefix, base58btc) so they round-trip cleanly
//! through YAML.
//!
//! `verify_card` is non-fatal for unsigned cards: it returns
//! `VerifyOutcome::Unsigned` so the importer can tag `import_trust:
//! "unsigned"` and surface a yellow banner instead of refusing to load.

use anyhow::{Context, Result, bail};
use chrono::Utc;
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};

use super::canonical::canonical_data_bytes;
use super::extensions::{CardSignature, MurExt, ProvenanceExt};
use super::schema::{Extensions, MurCard};

const MULTIBASE_Z: char = 'z';

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerifyOutcome {
    Signed,
    Unsigned,
}

pub fn sign_card(card: &mut MurCard, sk: &SigningKey) -> Result<()> {
    // Set up the extensions / mur / provenance scaffolding up-front and
    // clear any pre-existing signature. Doing it BEFORE
    // `canonical_data_bytes` ensures that sign-time and verify-time see
    // the same enclosing structure (verify only ever strips
    // `provenance.signature`, never the surrounding objects).
    let ext = card.extensions.get_or_insert_with(Extensions::default);
    let mur = ext.mur.get_or_insert(MurExt {
        schema_version: 1,
        voice: None,
        avatar: None,
        relationship: None,
        first_memory: None,
        companion: None,
        provenance: None,
    });
    let prov = mur.provenance.get_or_insert(ProvenanceExt {
        signature: None,
        content_rating: None,
        import_trust: None,
    });
    prov.signature = None;

    let pubkey = encode_multibase_z(&sk.verifying_key().to_bytes());
    let signed_at = Utc::now();

    // Compute canonical bytes with the scaffolding present and signature
    // absent — exactly what `verify_card` will see post-strip.
    let bytes = canonical_data_bytes(card)?;
    let sig: Signature = sk.sign(&bytes);
    let value = encode_multibase_z(&sig.to_bytes());

    let cs = CardSignature {
        algorithm: "ed25519".into(),
        public_key: pubkey,
        value,
        signed_at,
    };
    // Re-borrow to attach the freshly-computed signature.
    card.extensions
        .as_mut()
        .and_then(|e| e.mur.as_mut())
        .and_then(|m| m.provenance.as_mut())
        .expect("scaffolding installed above")
        .signature = Some(cs);
    Ok(())
}

pub fn verify_card(card: &MurCard) -> Result<VerifyOutcome> {
    let Some(sig) = card
        .extensions
        .as_ref()
        .and_then(|e| e.mur.as_ref())
        .and_then(|m| m.provenance.as_ref())
        .and_then(|p| p.signature.as_ref())
    else {
        return Ok(VerifyOutcome::Unsigned);
    };
    if sig.algorithm != "ed25519" {
        bail!("unsupported signature algorithm: {}", sig.algorithm);
    }
    let pub_bytes = decode_multibase_z(&sig.public_key).context("decode public_key")?;
    let sig_bytes = decode_multibase_z(&sig.value).context("decode signature value")?;

    let pub_arr: [u8; 32] = pub_bytes
        .as_slice()
        .try_into()
        .context("public_key length must be 32 bytes")?;
    let sig_arr: [u8; 64] = sig_bytes
        .as_slice()
        .try_into()
        .context("signature length must be 64 bytes")?;

    let vk = VerifyingKey::from_bytes(&pub_arr).context("VerifyingKey::from_bytes")?;
    let sig_obj = Signature::from_bytes(&sig_arr);
    let bytes = canonical_data_bytes(card)?;
    vk.verify(&bytes, &sig_obj)
        .map_err(|e| anyhow::anyhow!("signature verification failed: {e}"))?;
    Ok(VerifyOutcome::Signed)
}

fn encode_multibase_z(bytes: &[u8]) -> String {
    let body = bs58::encode(bytes).into_string();
    let mut out = String::with_capacity(body.len() + 1);
    out.push(MULTIBASE_Z);
    out.push_str(&body);
    out
}

fn decode_multibase_z(s: &str) -> Result<Vec<u8>> {
    let mut chars = s.chars();
    let prefix = chars.next().context("empty multibase string")?;
    if prefix != MULTIBASE_Z {
        bail!("only multibase z-prefix supported, got '{prefix}'");
    }
    let body: String = chars.collect();
    bs58::decode(body).into_vec().context("base58 decode")
}
