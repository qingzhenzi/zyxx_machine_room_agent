//! 跨 crate 契约测试：确认客户端与服务端的 transcript 构造、PoP 验签、
//! CSR 公钥提取、CA 签发在真实数据上是自洽的。
//!
//! 这里不启动数据库与 HTTP，只验证最容易因实现漂移而失败的那部分协议。

use qe_device_pki::ca::Ca;
use qe_device_pki::pop::{self, SignatureAlgorithm};

/// 用客户端 crate 的密钥与 transcript 生成 PoP，再用服务端 crate 验签。
#[test]
fn client_pop_verifies_on_server() {
    let key = qe_device_pki_client::transcript::DeviceKey::generate().unwrap();

    let challenge = [3u8; 32];
    let token = "deadbeefcafe";
    let slot = "slot-integration-1";
    let csr_der = build_csr(slot, &key);
    let fp = [9u8; 32];

    // 客户端视角
    let client_transcript = qe_device_pki_client::transcript::canonical_transcript(
        &challenge,
        &qe_device_pki_client::transcript::sha256(token.as_bytes()),
        slot,
        &qe_device_pki_client::transcript::sha256(&csr_der),
        &fp,
    );
    let sig = key.sign(&client_transcript).unwrap();

    // 服务端视角
    let server_transcript = pop::canonical_transcript(
        &challenge,
        &qe_device_pki_client::transcript::sha256(token.as_bytes()),
        slot,
        &pop::sha256(&csr_der),
        &fp,
    );
    assert_eq!(
        client_transcript, server_transcript,
        "两端 transcript 构造必须逐字节一致"
    );

    // 服务端验签用的公钥来自 CSR（完整 SPKI）
    let spki = extract_spki(&csr_der);
    pop::verify(
        &spki,
        SignatureAlgorithm::EcdsaP256Sha256,
        &server_transcript,
        &sig,
    )
    .expect("服务端必须能验证客户端 PoP");
}

#[test]
fn server_rejects_signature_from_other_transcript() {
    let key = qe_device_pki_client::transcript::DeviceKey::generate().unwrap();
    let slot = "slot-2";
    let csr_der = build_csr(slot, &key);
    let spki = extract_spki(&csr_der);

    let good =
        pop::canonical_transcript(&[1; 32], &[2; 32], slot, &pop::sha256(&csr_der), &[3; 32]);
    let sig = key.sign(&good).unwrap();

    // 换了 slot 的 transcript 必须验签失败
    let bad = pop::canonical_transcript(
        &[1; 32],
        &[2; 32],
        "slot-3",
        &pop::sha256(&csr_der),
        &[3; 32],
    );
    assert!(
        pop::verify(&spki, SignatureAlgorithm::EcdsaP256Sha256, &bad, &sig).is_err(),
        "跨 slot 重放必须被拒绝"
    );
}

#[test]
fn ca_signs_client_key_and_chain_verifies() {
    use x509_parser::prelude::*;

    let ca = Ca::generate_self_signed("Integration Device CA").unwrap();
    let key = qe_device_pki_client::transcript::DeviceKey::generate().unwrap();
    let csr_der = build_csr("slot-integration-2", &key);

    let issued = ca.sign_csr(&csr_der, "slot-integration-2", 365).unwrap();

    let (_, leaf) = X509Certificate::from_der(&issued.cert_der).unwrap();
    let (_, ca_cert) = X509Certificate::from_der(ca.cert_der()).unwrap();

    // 1. 链验证
    assert!(leaf.verify_signature(Some(ca_cert.public_key())).is_ok());
    // 2. CN 由服务端决定
    assert_eq!(leaf.subject().to_string(), "CN=slot-integration-2");
    // 3. clientAuth EKU
    assert!(
        leaf.extended_key_usage()
            .unwrap()
            .unwrap()
            .value
            .client_auth
    );
    // 4. 证书公钥 = 客户端私钥对应公钥（关键：不能签错钥匙）
    assert_eq!(leaf.tbs_certificate.subject_pki.raw, extract_spki(&csr_der));
    // 5. 有效期按请求设置（约 365 天）
    let span = leaf.validity().not_after.timestamp() - leaf.validity().not_before.timestamp();
    assert!(span > 360 * 86400 && span < 370 * 86400, "span={span}");
}

#[test]
fn revoked_serial_appears_in_crl() {
    use qe_device_pki::ca::RevokedEntry;
    use rcgen::RevocationReason;

    let ca = Ca::generate_self_signed("Integration Device CA").unwrap();
    let key = qe_device_pki_client::transcript::DeviceKey::generate().unwrap();
    let issued = ca
        .sign_csr(&build_csr("slot-r", &key), "slot-r", 365)
        .unwrap();

    let now = time::OffsetDateTime::now_utc();
    let crl = ca
        .build_crl(
            &[RevokedEntry {
                serial_hex: issued.serial_hex.clone(),
                revoked_at: now,
                reason: RevocationReason::Superseded,
            }],
            7,
        )
        .unwrap();

    assert!(crl.contains("BEGIN X509 CRL"));
    // 解析 CRL 并确认序列号在里面
    let der = pem_to_first_der(&crl);
    use x509_parser::prelude::FromDer;
    let (_, parsed) = x509_parser::revocation_list::CertificateRevocationList::from_der(&der)
        .expect("CRL 必须是可解析的 DER");
    let revoked: Vec<String> = parsed
        .iter_revoked_certificates()
        .map(|r| hex::encode(r.raw_serial()))
        .collect();
    assert!(
        revoked.contains(&issued.serial_hex),
        "吊销列表应包含 {}，实际 {:?}",
        issued.serial_hex,
        revoked
    );
}

// ---------------- helpers ----------------

fn build_csr(cn: &str, key: &qe_device_pki_client::transcript::DeviceKey) -> Vec<u8> {
    use rcgen::{CertificateParams, DistinguishedName, DnType, KeyPair};
    let b64 = {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD.encode(key.pkcs8())
    };
    let mut pem = String::from("-----BEGIN PRIVATE KEY-----\n");
    for chunk in b64.as_bytes().chunks(64) {
        pem.push_str(std::str::from_utf8(chunk).unwrap());
        pem.push('\n');
    }
    pem.push_str("-----END PRIVATE KEY-----\n");

    let rc_key = KeyPair::from_pem(&pem).unwrap();
    let mut params = CertificateParams::default();
    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, cn);
    params.distinguished_name = dn;
    params.serialize_request(&rc_key).unwrap().der().to_vec()
}

fn extract_spki(csr_der: &[u8]) -> Vec<u8> {
    use x509_parser::prelude::*;
    let (_, csr) = X509CertificationRequest::from_der(csr_der).unwrap();
    csr.certification_request_info.subject_pki.raw.to_vec()
}

/// CRL 用 `X509 CRL` 标签，rustls_pemfile 不认，直接解 base64 主体。
fn pem_to_first_der(pem: &str) -> Vec<u8> {
    use base64::Engine;
    let b64: String = pem.lines().filter(|l| !l.starts_with("-----")).collect();
    base64::engine::general_purpose::STANDARD
        .decode(b64.trim())
        .expect("valid base64 body")
}
