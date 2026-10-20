//! Ed25519 signatures (ring): Tessera Guard signs what it says about this
//! computer's Stop, and a server checks it with the key it pinned.

use ring::rand::SystemRandom;
use ring::signature::{ED25519, Ed25519KeyPair, KeyPair, UnparsedPublicKey};

/// A new key: its private part (PKCS#8, to be kept where only the signer
/// reads it) and its public part.
pub fn generate() -> Option<(Vec<u8>, Vec<u8>)> {
    let pkcs8 = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).ok()?;
    let pair = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).ok()?;
    Some((pkcs8.as_ref().to_vec(), pair.public_key().as_ref().to_vec()))
}

/// The public part of a kept private key.
pub fn public_of(pkcs8: &[u8]) -> Option<Vec<u8>> {
    let pair = Ed25519KeyPair::from_pkcs8(pkcs8).ok()?;
    Some(pair.public_key().as_ref().to_vec())
}

/// `message` signed with the kept private key.
pub fn sign(pkcs8: &[u8], message: &[u8]) -> Option<Vec<u8>> {
    let pair = Ed25519KeyPair::from_pkcs8(pkcs8).ok()?;
    Some(pair.sign(message).as_ref().to_vec())
}

/// `signature` is `message` signed by the key whose public part is `public`.
pub fn verify(public: &[u8], message: &[u8], signature: &[u8]) -> bool {
    UnparsedPublicKey::new(&ED25519, public)
        .verify(message, signature)
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_kept_key_signs_what_its_public_part_accepts() {
        let (private, public) = generate().unwrap();
        assert_eq!(public_of(&private).unwrap(), public);
        let sig = sign(&private, b"stopped").unwrap();
        assert!(verify(&public, b"stopped", &sig));
        assert!(!verify(&public, b"not stopped", &sig));
        let (_, other) = generate().unwrap();
        assert!(!verify(&other, b"stopped", &sig));
    }
}
