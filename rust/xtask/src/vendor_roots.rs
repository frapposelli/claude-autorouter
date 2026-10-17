//! Verify the immutable Node root corpus without linking inspected OpenSSL.
//! Pinned provenance binds the exact original subject labels; ordered DER hashes
//! bind certificate identity independently of any runtime's display formatting.
use crate::audit_support::{digest, regular};
use rustls_pki_types::CertificateDer;
use rustls_pki_types::pem::PemObject;
use serde_json::{Value, json};
use std::path::Path;
const PROVENANCE_SHA256: &str = "75eabcc62e010020332c92caa57c9463c2a1bc920a5be1ac368b7d26de2bcac9";

fn verify_materials(metadata: &[u8], pem: &[u8], license: &[u8]) -> Result<Value, String> {
    if digest(metadata) != PROVENANCE_SHA256 {
        return Err("Bundled root provenance differs from reviewed pin".into());
    }
    let metadata: Value =
        serde_json::from_slice(metadata).map_err(|_| "Invalid root provenance JSON")?;
    if metadata["schema_version"] != 1
        || metadata["node_version"] != "v22.14.0"
        || metadata["official_source_der_fingerprints_match"] != true
        || digest(pem) != metadata["pem_sha256"]
        || digest(license) != metadata["license_sha256"]
    {
        return Err("Bundled root PEM/license/source identity differs".into());
    }
    let certificates = CertificateDer::pem_slice_iter(pem)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| "Invalid bundled certificate PEM")?;
    let expected = metadata["certificates"]
        .as_array()
        .ok_or("Missing bundled root fingerprints")?;
    if certificates.len() != 149
        || metadata["certificate_count"] != 149
        || certificates.len() != expected.len()
    {
        return Err("Bundled root certificate count differs".into());
    }
    for (certificate, expected) in certificates.iter().zip(expected) {
        if digest(certificate.as_ref()) != expected["sha256"]
            || !expected["subject"]
                .as_str()
                .is_some_and(|name| !name.is_empty())
        {
            return Err("Ordered bundled root certificate identity differs".into());
        }
    }
    Ok(
        json!({"passed":true,"name":"node-ca","node_version":metadata["node_version"],"certificate_count":certificates.len(),"pem_sha256":metadata["pem_sha256"],"license_sha256":metadata["license_sha256"],"provenance_sha256":PROVENANCE_SHA256,"ordered_der_fingerprints":true,"subject_labels":"bound by exact reviewed provenance","official_source_der_fingerprints_match":true}),
    )
}
pub fn verify(path: &Path) -> Result<Value, String> {
    verify_materials(
        &regular(&path.join("provenance.json"), 2 * 1024 * 1024)?,
        &regular(&path.join("node-v22.14.0.pem"), 2 * 1024 * 1024)?,
        &regular(&path.join("LICENSE"), 2 * 1024 * 1024)?,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pinned_roots_reject_changed_license_pem_subject_order_and_fingerprints() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../vendor/node-ca");
        let metadata = regular(&root.join("provenance.json"), 2 * 1024 * 1024).unwrap();
        let pem = regular(&root.join("node-v22.14.0.pem"), 2 * 1024 * 1024).unwrap();
        let license = regular(&root.join("LICENSE"), 2 * 1024 * 1024).unwrap();
        assert_eq!(
            verify_materials(&metadata, &pem, &license).unwrap()["certificate_count"],
            149
        );
        for key in ["subject", "sha256", "order", "count", "official"] {
            let mut changed: Value = serde_json::from_slice(&metadata).unwrap();
            match key {
                "subject" => {
                    changed["certificates"][0]["subject"] = json!("synthetic changed subject")
                }
                "sha256" => changed["certificates"][0]["sha256"] = json!("0".repeat(64)),
                "order" => changed["certificates"].as_array_mut().unwrap().swap(0, 1),
                "count" => changed["certificate_count"] = json!(148),
                "official" => changed["official_source_der_fingerprints_match"] = json!(false),
                _ => unreachable!(),
            }
            assert!(
                verify_materials(&serde_json::to_vec(&changed).unwrap(), &pem, &license).is_err()
            );
        }
        let mut changed = pem.clone();
        changed[50] ^= 1;
        assert!(verify_materials(&metadata, &changed, &license).is_err());
        let mut changed = license.clone();
        changed.push(b'!');
        assert!(verify_materials(&metadata, &pem, &changed).is_err());
    }
}
