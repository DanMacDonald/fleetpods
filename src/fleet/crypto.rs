//! SPAKE2 enrollment session. The six-digit code is the password and never
//! leaves this process. Identity labels come from [`super::wire`].

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Nonce};
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use rand::rngs::OsRng;
use rand::RngCore;
use sha2::Sha256;
use spake2::{Ed25519Group, Identity, Password, Spake2};
use zeroize::Zeroizing;

pub struct Pake {
    pending: Option<Spake2<Ed25519Group>>,
    key: Zeroizing<[u8; 32]>,
    context: Vec<u8>,
    initiator: bool,
    ready: bool,
}

impl Pake {
    pub fn start(initiator: bool, code: &[u8], context: &[u8]) -> (Self, Vec<u8>) {
        let inviter_id = super::wire::role_identity("inviter", context);
        let joiner_id = super::wire::role_identity("joiner", context);
        let inviter = Identity::new(&inviter_id);
        let joiner = Identity::new(&joiner_id);
        let password = Password::new(code);
        let (pending, message) = if initiator {
            Spake2::start_a(&password, &inviter, &joiner)
        } else {
            Spake2::start_b(&password, &inviter, &joiner)
        };
        (
            Self {
                pending: Some(pending),
                key: Zeroizing::new([0; 32]),
                context: context.to_vec(),
                initiator,
                ready: false,
            },
            message,
        )
    }

    pub fn finish(&mut self, message: &[u8]) -> bool {
        let Some(pending) = self.pending.take() else {
            return false;
        };
        let Ok(shared) = pending.finish(message) else {
            return false;
        };
        let shared = Zeroizing::new(shared);
        let label = super::wire::session_label();
        self.ready = Hkdf::<Sha256>::new(Some(&self.context), &shared)
            .expand(&label, self.key.as_mut())
            .is_ok();
        self.ready
    }

    pub fn confirm(&self) -> Option<[u8; 32]> {
        self.tag(self.initiator)
    }

    pub fn verify(&self, tag: &[u8]) -> bool {
        self.mac(!self.initiator).is_some_and(|mac| mac.verify_slice(tag).is_ok())
    }

    fn tag(&self, initiator: bool) -> Option<[u8; 32]> {
        let mac = self.mac(initiator)?;
        let bytes = mac.finalize().into_bytes();
        let mut out = [0u8; 32];
        out.copy_from_slice(&bytes);
        Some(out)
    }

    fn mac(&self, initiator: bool) -> Option<Hmac<Sha256>> {
        if !self.ready {
            return None;
        }
        let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(self.key.as_ref()).ok()?;
        mac.update(if initiator { b"inviter-confirm" } else { b"joiner-confirm" });
        mac.update(&self.context);
        Some(mac)
    }

    pub fn seal(&self, clear: &[u8]) -> Option<Vec<u8>> {
        self.crypt(true, clear)
    }

    pub fn open(&self, sealed: &[u8]) -> Option<Vec<u8>> {
        self.crypt(false, sealed)
    }

    fn crypt(&self, encrypt: bool, input: &[u8]) -> Option<Vec<u8>> {
        if !self.ready {
            return None;
        }
        let sender_is_initiator = if encrypt { self.initiator } else { !self.initiator };
        let mut key = Zeroizing::new([0u8; 32]);
        Hkdf::<Sha256>::new(None, self.key.as_ref())
            .expand(
                if sender_is_initiator { b"inviter-data" } else { b"joiner-data" },
                key.as_mut(),
            )
            .ok()?;
        let cipher = ChaCha20Poly1305::new_from_slice(key.as_ref()).ok()?;
        if encrypt {
            let mut nonce = [0u8; 12];
            OsRng.fill_bytes(&mut nonce);
            let body = cipher
                .encrypt(Nonce::from_slice(&nonce), Payload { msg: input, aad: &self.context })
                .ok()?;
            Some([nonce.as_slice(), &body].concat())
        } else {
            if input.len() < 28 {
                return None;
            }
            cipher
                .decrypt(
                    Nonce::from_slice(&input[..12]),
                    Payload { msg: &input[12..], aad: &self.context },
                )
                .ok()
        }
    }
}

/// Seal one fleet announcement. The nonce is fresh. Associated data is the
/// domain plus the fleet id, and that same pair derives the key.
pub fn seal_fleet(domain: &[u8], id: &[u8; 16], key: &[u8; 32], clear: &[u8]) -> Option<Vec<u8>> {
    let (cipher, context) = fleet_cipher(domain, id, key)?;
    let mut nonce = [0u8; 12];
    OsRng.fill_bytes(&mut nonce);
    let body =
        cipher.encrypt(Nonce::from_slice(&nonce), Payload { msg: clear, aad: &context }).ok()?;
    let mut out = Vec::with_capacity(nonce.len() + body.len());
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&body);
    Some(out)
}

/// Open a frame sealed by [`seal_fleet`]. A wrong key, a wrong fleet, or a
/// changed byte returns nothing.
pub fn open_fleet(domain: &[u8], id: &[u8; 16], key: &[u8; 32], sealed: &[u8]) -> Option<Vec<u8>> {
    if sealed.len() < 28 {
        return None;
    }
    let (cipher, context) = fleet_cipher(domain, id, key)?;
    cipher
        .decrypt(Nonce::from_slice(&sealed[..12]), Payload { msg: &sealed[12..], aad: &context })
        .ok()
}

fn fleet_cipher(
    domain: &[u8],
    id: &[u8; 16],
    key: &[u8; 32],
) -> Option<(ChaCha20Poly1305, Vec<u8>)> {
    let mut context = Vec::with_capacity(domain.len() + id.len());
    context.extend_from_slice(domain);
    context.extend_from_slice(id);
    let mut derived = Zeroizing::new([0u8; 32]);
    Hkdf::<Sha256>::new(Some(id), key).expand(&context, derived.as_mut()).ok()?;
    Some((ChaCha20Poly1305::new_from_slice(derived.as_ref()).ok()?, context))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matching_codes_confirm_and_a_wrong_code_does_not() {
        let context = b"ctx";
        let (mut inviter, offer) = Pake::start(true, b"012345", context);
        let (mut joiner, response) = Pake::start(false, b"012345", context);
        assert!(joiner.finish(&offer));
        assert!(inviter.finish(&response));
        let tag = joiner.confirm().unwrap();
        assert!(inviter.verify(&tag));

        let (mut inviter, offer) = Pake::start(true, b"012345", context);
        let (mut joiner, response) = Pake::start(false, b"999999", context);
        assert!(joiner.finish(&offer));
        assert!(inviter.finish(&response));
        assert!(!inviter.verify(&joiner.confirm().unwrap()));
    }
}
