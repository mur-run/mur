//! Test keys and helpers. Ed25519 and ECDSA keys are generated per run with
//! `ring`; RSA uses the fixed vector in `rsa_vector.rs`.

use ring::rand::SystemRandom;
use ring::signature::{
    ECDSA_P256_SHA256_FIXED_SIGNING, ECDSA_P384_SHA384_FIXED_SIGNING, EcdsaKeyPair, Ed25519KeyPair,
    KeyPair,
};

use super::rsa_vector;
use crate::ssh_proxy::wire::{put_string, put_u32};

pub fn s(parts: &[&[u8]]) -> Vec<u8> {
    let mut out = Vec::new();
    for p in parts {
        put_string(&mut out, p);
    }
    out
}

/// Positive big-endian magnitude → SSH mpint.
pub fn mpint(mag: &[u8]) -> Vec<u8> {
    let i = mag.iter().position(|&b| b != 0).unwrap_or(mag.len());
    let mut v = mag[i..].to_vec();
    if v.first().is_some_and(|b| b & 0x80 != 0) {
        v.insert(0, 0);
    }
    v
}

pub fn hex(parts: &[&str]) -> Vec<u8> {
    let h: String = parts.concat();
    (0..h.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&h[i..i + 2], 16).unwrap())
        .collect()
}

/// A host key that can sign a session id the way an ssh server does.
pub trait TestHost {
    fn key_blob(&self) -> Vec<u8>;
    fn sign_blob(&self, msg: &[u8]) -> Vec<u8>;
}

pub struct Ed(pub Ed25519KeyPair);

impl Ed {
    pub fn new() -> Self {
        let pk = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).unwrap();
        Self(Ed25519KeyPair::from_pkcs8(pk.as_ref()).unwrap())
    }
}

impl TestHost for Ed {
    fn key_blob(&self) -> Vec<u8> {
        s(&[b"ssh-ed25519", self.0.public_key().as_ref()])
    }
    fn sign_blob(&self, msg: &[u8]) -> Vec<u8> {
        s(&[b"ssh-ed25519", self.0.sign(msg).as_ref()])
    }
}

pub struct Ec {
    pair: EcdsaKeyPair,
    name: &'static str,
    curve: &'static str,
    width: usize,
}

impl Ec {
    pub fn new(p384: bool) -> Self {
        let rng = SystemRandom::new();
        let alg = if p384 {
            &ECDSA_P384_SHA384_FIXED_SIGNING
        } else {
            &ECDSA_P256_SHA256_FIXED_SIGNING
        };
        let pk = EcdsaKeyPair::generate_pkcs8(alg, &rng).unwrap();
        let pair = EcdsaKeyPair::from_pkcs8(alg, pk.as_ref(), &rng).unwrap();
        let (name, curve, width) = if p384 {
            ("ecdsa-sha2-nistp384", "nistp384", 48)
        } else {
            ("ecdsa-sha2-nistp256", "nistp256", 32)
        };
        Self {
            pair,
            name,
            curve,
            width,
        }
    }
}

impl TestHost for Ec {
    fn key_blob(&self) -> Vec<u8> {
        s(&[
            self.name.as_bytes(),
            self.curve.as_bytes(),
            self.pair.public_key().as_ref(),
        ])
    }
    fn sign_blob(&self, msg: &[u8]) -> Vec<u8> {
        let fixed = self.pair.sign(&SystemRandom::new(), msg).unwrap();
        let (r, sv) = fixed.as_ref().split_at(self.width);
        let inner = s(&[&mpint(r), &mpint(sv)]);
        s(&[self.name.as_bytes(), &inner])
    }
}

/// The fixed RSA vector. Only signs [`rsa_vector::SESSION_ID`].
pub fn rsa_key_blob() -> Vec<u8> {
    s(&[
        b"ssh-rsa",
        &mpint(&[0x01, 0x00, 0x01]),
        &mpint(&hex(rsa_vector::N_HEX)),
    ])
}

pub fn rsa_sig_blob(sha512: bool) -> Vec<u8> {
    if sha512 {
        s(&[b"rsa-sha2-512", &hex(rsa_vector::SIG_SHA512_HEX)])
    } else {
        s(&[b"rsa-sha2-256", &hex(rsa_vector::SIG_SHA256_HEX)])
    }
}

/// A `session-bind@openssh.com` extension body (type byte included).
pub fn bind_body(host_key: &[u8], session_id: &[u8], sig: &[u8]) -> Vec<u8> {
    let mut b = vec![crate::ssh_proxy::wire::SSH_AGENTC_EXTENSION];
    put_string(
        &mut b,
        crate::ssh_proxy::wire::SESSION_BIND_EXTENSION.as_bytes(),
    );
    put_string(&mut b, host_key);
    put_string(&mut b, session_id);
    put_string(&mut b, sig);
    b.push(0);
    b
}

pub fn sign_body(key_blob: &[u8]) -> Vec<u8> {
    let mut b = vec![crate::ssh_proxy::wire::SSH_AGENTC_SIGN_REQUEST];
    put_string(&mut b, key_blob);
    put_string(&mut b, b"data to sign");
    put_u32(&mut b, 0);
    b
}
