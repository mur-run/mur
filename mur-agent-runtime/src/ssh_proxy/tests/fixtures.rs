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
    bind_body_fwd(host_key, session_id, sig, false)
}

/// [`bind_body`] with the `is_forwarding` flag chosen.
pub fn bind_body_fwd(host_key: &[u8], session_id: &[u8], sig: &[u8], fwd: bool) -> Vec<u8> {
    let mut b = vec![crate::ssh_proxy::wire::SSH_AGENTC_EXTENSION];
    put_string(
        &mut b,
        crate::ssh_proxy::wire::SESSION_BIND_EXTENSION.as_bytes(),
    );
    put_string(&mut b, host_key);
    put_string(&mut b, session_id);
    put_string(&mut b, sig);
    b.push(u8::from(fwd));
    b
}

/// The session id every test bind uses unless it says otherwise.
pub const SID: &[u8] = b"sid";

/// A sign request for `key_blob` whose data is a valid `publickey` userauth
/// request for session [`SID`].
pub fn sign_body(key_blob: &[u8]) -> Vec<u8> {
    sign_data_body(key_blob, &Userauth::new(SID, key_blob).encode())
}

/// A sign request with arbitrary data.
pub fn sign_data_body(key_blob: &[u8], data: &[u8]) -> Vec<u8> {
    let mut b = vec![crate::ssh_proxy::wire::SSH_AGENTC_SIGN_REQUEST];
    put_string(&mut b, key_blob);
    put_string(&mut b, data);
    put_u32(&mut b, 0);
    b
}

/// The data an ssh client asks the agent to sign during publickey auth
/// (RFC 4252 section 7; OpenSSH PROTOCOL section 2.5 for the hostbound
/// method). Built independently of the proxy's parser.
pub struct Userauth<'a> {
    pub session_id: &'a [u8],
    pub user: &'a [u8],
    pub service: &'a [u8],
    pub method: &'a [u8],
    pub key_blob: &'a [u8],
    /// `Some` selects the hostbound method's trailing server host key.
    pub host_key: Option<&'a [u8]>,
}

impl<'a> Userauth<'a> {
    pub fn new(session_id: &'a [u8], key_blob: &'a [u8]) -> Self {
        Self {
            session_id,
            user: b"git",
            service: b"ssh-connection",
            method: b"publickey",
            key_blob,
            host_key: None,
        }
    }

    pub fn hostbound(mut self, host_key: &'a [u8]) -> Self {
        self.method = b"publickey-hostbound-v00@openssh.com";
        self.host_key = Some(host_key);
        self
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut d = Vec::new();
        put_string(&mut d, self.session_id);
        d.push(50); // SSH_MSG_USERAUTH_REQUEST
        put_string(&mut d, self.user);
        put_string(&mut d, self.service);
        put_string(&mut d, self.method);
        d.push(1); // TRUE: this is a real signature, not a query
        put_string(&mut d, b"ssh-ed25519");
        put_string(&mut d, self.key_blob);
        if let Some(h) = self.host_key {
            put_string(&mut d, h);
        }
        d
    }
}
