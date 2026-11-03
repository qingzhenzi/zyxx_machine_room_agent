//! Canonical transcript 与 PoP 签名（与服务端 `qe_device_pki::pop` 严格对称）。
//!
//! ```text
//! canonical_transcript = challenge || token_hash || slot || sha256(csr_der) || tls_cert_seen_fp
//! ```
//!
//! 任何字段顺序或编码的改动都必须两端同步，否则验签会失败。

use crate::error::{Error, Result};
use ring::rand::SystemRandom;
use ring::signature::{EcdsaKeyPair, KeyPair};
use sha2::{Digest, Sha256};

/// PoP 签名算法标签（与服务端 `SignatureAlgorithm::from_label` 对齐）。
pub const POP_ALGORITHM: &str = "ecdsa-p256-sha256";

pub fn sha256(data: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(data);
    h.finalize().into()
}

pub fn canonical_transcript(
    challenge: &[u8],
    token_hash: &[u8],
    device_slot: &str,
    csr_sha256: &[u8],
    tls_cert_seen_fp: &[u8],
) -> Vec<u8> {
    let mut out = Vec::with_capacity(
        challenge.len()
            + token_hash.len()
            + device_slot.len()
            + csr_sha256.len()
            + tls_cert_seen_fp.len(),
    );
    out.extend_from_slice(challenge);
    out.extend_from_slice(token_hash);
    out.extend_from_slice(device_slot.as_bytes());
    out.extend_from_slice(csr_sha256);
    out.extend_from_slice(tls_cert_seen_fp);
    out
}

/// P-256 密钥对，PKCS#8 DER 编码（便于 0600 落盘）。
pub struct DeviceKey {
    pkcs8: Vec<u8>,
    key_pair: EcdsaKeyPair,
}

impl DeviceKey {
    pub fn generate() -> Result<Self> {
        let rng = SystemRandom::new();
        let pkcs8 =
            EcdsaKeyPair::generate_pkcs8(&ring::signature::ECDSA_P256_SHA256_ASN1_SIGNING, &rng)
                .map_err(|e| Error::Crypto(format!("generate key: {e}")))?;
        let pkcs8 = pkcs8.as_ref().to_vec();
        let key_pair = Self::parse(&pkcs8)?;
        Ok(Self { pkcs8, key_pair })
    }

    pub fn from_pkcs8(pkcs8: Vec<u8>) -> Result<Self> {
        let key_pair = Self::parse(&pkcs8)?;
        Ok(Self { pkcs8, key_pair })
    }

    fn parse(pkcs8: &[u8]) -> Result<EcdsaKeyPair> {
        EcdsaKeyPair::from_pkcs8(
            &ring::signature::ECDSA_P256_SHA256_ASN1_SIGNING,
            pkcs8,
            &SystemRandom::new(),
        )
        .map_err(|e| Error::Crypto(format!("load key: {e}")))
    }

    pub fn pkcs8(&self) -> &[u8] {
        &self.pkcs8
    }

    pub fn public_key_der(&self) -> &[u8] {
        self.key_pair.public_key().as_ref()
    }

    /// 对 transcript 签名，返回 ASN.1 DER 编码的 ECDSA 签名。
    pub fn sign(&self, message: &[u8]) -> Result<Vec<u8>> {
        let sig = self
            .key_pair
            .sign(&SystemRandom::new(), message)
            .map_err(|e| Error::Crypto(format!("sign: {e}")))?;
        Ok(sig.as_ref().to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::signature::UnparsedPublicKey;

    #[test]
    fn transcript_matches_contract_layout() {
        let t = canonical_transcript(b"CH", b"TOK", "slot-1", b"CSR", b"FP");
        let mut expected = Vec::new();
        expected.extend_from_slice(b"CH");
        expected.extend_from_slice(b"TOK");
        expected.extend_from_slice(b"slot-1");
        expected.extend_from_slice(b"CSR");
        expected.extend_from_slice(b"FP");
        assert_eq!(t, expected);
    }

    #[test]
    fn transcript_is_deterministic() {
        let a = canonical_transcript(b"c", b"t", "s", b"x", b"f");
        let b = canonical_transcript(b"c", b"t", "s", b"x", b"f");
        assert_eq!(a, b);
    }

    #[test]
    fn transcript_distinguishes_slots() {
        let a = canonical_transcript(b"c", b"t", "slot-1", b"x", b"f");
        let b = canonical_transcript(b"c", b"t", "slot-2", b"x", b"f");
        assert_ne!(a, b);
    }

    #[test]
    fn signature_verifies_with_own_pubkey() {
        let key = DeviceKey::generate().unwrap();
        let msg = canonical_transcript(b"c", b"t", "s", b"x", b"f");
        let sig = key.sign(&msg).unwrap();
        UnparsedPublicKey::new(
            &ring::signature::ECDSA_P256_SHA256_ASN1,
            key.public_key_der(),
        )
        .verify(&msg, &sig)
        .expect("signature must verify");
    }

    #[test]
    fn key_roundtrip_from_pkcs8() {
        let key = DeviceKey::generate().unwrap();
        let restored = DeviceKey::from_pkcs8(key.pkcs8().to_vec()).unwrap();
        assert_eq!(restored.public_key_der(), key.public_key_der());
    }

    #[test]
    fn sha256_is_32_bytes() {
        assert_eq!(sha256(b"abc").len(), 32);
    }
}
