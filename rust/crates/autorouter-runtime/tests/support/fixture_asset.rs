//! Immutable test-source assets only. This module is included by an integration
//! test, never by the product library. Source archives store the gzip file itself;
//! decoded fixture bytes are not restored source files and remain separately bounded.
//!
//! Native verification: cargo test -p autorouter-runtime --locked --offline
//! --test server_streaming_routing_contracts fixture_asset::tests::
//!
//! Optional authoring uses Apple gzip 479, with no filename/timestamp in its
//! header:
//! ```sh
//! env -i PATH=/usr/bin:/bin LC_ALL=C /usr/bin/gzip -n -9 -c ORIGINAL > NEW.jsonl.gz
//! ```
//! Verify ORIGINAL with the immutable expanded SHA below first.
//! The exact checked gzip SHA is in fixture-assets-v1.json. Other gzip versions
//! may produce different stored bytes; native decoding must still recover the
//! exact same original bytes. Regeneration with a different tool requires an
//! explicit manifest/provenance update, never a startup fallback or recapture.
use flate2::bufread::GzDecoder;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{io::Read, sync::LazyLock};

const INPUT_LIMIT: usize = 1024 * 1024;
const OUTPUT_LIMIT: usize = 4 * 1024 * 1024;
const LOGICAL: &str = "rust/parity/cases/server-streaming-routing-contracts.jsonl";
const STORED: &str = "rust/parity/cases/server-streaming-routing-contracts.jsonl.gz";
const EXPANDED_SHA: &str = "3f92130625b6e04789fd802e5322fbaba64098f6318bbd2e2b4e0d6a18151e33";
const ENCODED: &[u8] =
    include_bytes!("../../../../parity/cases/server-streaming-routing-contracts.jsonl.gz");
const MANIFEST: &str = include_str!("../../../../parity/fixture-assets-v1.json");

#[derive(Debug, PartialEq, Eq)]
enum Invalid {
    Manifest,
    StoredBound,
    StoredIdentity,
    Header,
    OutputBound,
    Gzip,
    Suffix,
    ExpandedIdentity,
    Utf8,
    Rows,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    schema_version: u8,
    kind: String,
    assets: Vec<Asset>,
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Asset {
    logical_path: String,
    stored_path: String,
    encoding: String,
    stored_bytes: u64,
    stored_sha256: String,
    expanded_bytes: u64,
    expanded_sha256: String,
    rows: u64,
    row_ids: Vec<String>,
    utf8: bool,
    final_newline: bool,
}
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn manifest(text: &str) -> Result<Asset, Invalid> {
    if text.len() > 16 * 1024 {
        return Err(Invalid::Manifest);
    }
    let mut parsed: Manifest = serde_json::from_str(text).map_err(|_| Invalid::Manifest)?;
    if parsed.schema_version != 1
        || parsed.kind != "immutable-parity-source-assets"
        || parsed.assets.len() != 1
    {
        return Err(Invalid::Manifest);
    }
    let asset = parsed.assets.pop().unwrap();
    if asset.logical_path != LOGICAL
        || asset.stored_path != STORED
        || asset.encoding != "gzip-single-v1"
        || asset.expanded_bytes != 3_970_358
        || asset.expanded_sha256 != EXPANDED_SHA
        || asset.rows != 4
        || asset.row_ids.iter().map(String::as_str).ne([
            "test/server.test.mjs#1",
            "test/server.test.mjs#8",
            "test/server.test.mjs#9",
            "test/server.test.mjs#10",
        ])
        || !asset.utf8
        || !asset.final_newline
    {
        return Err(Invalid::Manifest);
    }
    Ok(asset)
}
fn decode(asset: &Asset, input: &[u8]) -> Result<String, Invalid> {
    if input.len() > INPUT_LIMIT || asset.stored_bytes > INPUT_LIMIT as u64 {
        return Err(Invalid::StoredBound);
    }
    if asset.stored_bytes != input.len() as u64 || hash(input) != asset.stored_sha256 {
        return Err(Invalid::StoredIdentity);
    }
    // The closed source format does not permit optional names/comments/members.
    if input.len() < 18 || input[..4] != [0x1f, 0x8b, 8, 0] || input[4..8] != [0; 4] {
        return Err(Invalid::Header);
    }
    let expanded = usize::try_from(asset.expanded_bytes).map_err(|_| Invalid::OutputBound)?;
    if expanded > OUTPUT_LIMIT {
        return Err(Invalid::OutputBound);
    }
    let limit = expanded.checked_add(1).ok_or(Invalid::OutputBound)?;
    let mut decoder = GzDecoder::new(input);
    // A fixed output slab avoids read_to_end growth at the bound. One extra
    // byte distinguishes overflow, without another allocation or normalization.
    let mut output = vec![0; limit];
    let mut used = 0;
    loop {
        let count = decoder
            .read(&mut output[used..])
            .map_err(|_| Invalid::Gzip)?;
        if count == 0 {
            break;
        }
        used += count;
        if used > expanded {
            return Err(Invalid::OutputBound);
        }
    }
    output.truncate(used);
    // bufread::GzDecoder validates the footer/CRC and stops at the first member.
    // Requiring the underlying slice to be empty rejects every suffix/member.
    if !decoder.get_ref().is_empty() {
        return Err(Invalid::Suffix);
    }
    if output.len() != expanded || hash(&output) != asset.expanded_sha256 {
        return Err(Invalid::ExpandedIdentity);
    }
    let text = String::from_utf8(output).map_err(|_| Invalid::Utf8)?;
    if !text.ends_with('\n')
        || text.split_terminator('\n').any(str::is_empty)
        || text.split_terminator('\n').count() as u64 != asset.rows
    {
        return Err(Invalid::Rows);
    }
    Ok(text)
}
pub(super) fn server_streaming_routing_contracts() -> &'static str {
    static DECODED: LazyLock<String> = LazyLock::new(|| {
        let asset = manifest(MANIFEST)
            .unwrap_or_else(|reason| panic!("Immutable fixture manifest: {reason:?}"));
        decode(&asset, ENCODED)
            .unwrap_or_else(|reason| panic!("Immutable fixture asset: {reason:?}"))
    });
    DECODED.as_str()
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::{Compression, GzBuilder};
    use std::io::Write;
    fn gzip(input: &[u8]) -> Vec<u8> {
        let mut writer = GzBuilder::new()
            .mtime(0)
            .write(Vec::new(), Compression::best());
        writer.write_all(input).unwrap();
        writer.finish().unwrap()
    }
    fn synthetic(input: &[u8], encoded: &[u8]) -> Asset {
        Asset {
            logical_path: LOGICAL.into(),
            stored_path: STORED.into(),
            encoding: "gzip-single-v1".into(),
            stored_bytes: encoded.len() as u64,
            stored_sha256: hash(encoded),
            expanded_bytes: input.len() as u64,
            expanded_sha256: hash(input),
            rows: 1,
            row_ids: vec!["synthetic#1".into()],
            utf8: true,
            final_newline: true,
        }
    }
    fn encoded_identity(asset: &mut Asset, encoded: &[u8]) {
        asset.stored_bytes = encoded.len() as u64;
        asset.stored_sha256 = hash(encoded);
    }
    #[test]
    fn stored_asset_preserves_original_expanded_identity_and_rows() {
        let bytes = server_streaming_routing_contracts();
        assert_eq!(bytes.len(), 3_970_358);
        assert_eq!(hash(bytes.as_bytes()), EXPANDED_SHA);
        let ids: Vec<String> = bytes
            .lines()
            .map(|line| {
                let row: serde_json::Value = serde_json::from_str(line).unwrap();
                row["source_test"].as_str().unwrap().to_owned()
            })
            .collect();
        assert_eq!(ids, manifest(MANIFEST).unwrap().row_ids);
    }
    #[test]
    fn corruption_truncation_and_extra_members_fail_after_identity_admission() {
        let plain = b"synthetic\n";
        let encoded = gzip(plain);
        let mut asset = synthetic(plain, &encoded);
        let mut truncated = encoded.clone();
        truncated.pop();
        encoded_identity(&mut asset, &truncated);
        assert_eq!(decode(&asset, &truncated).err(), Some(Invalid::Gzip));
        for footer_offset in [8, 4] {
            let mut corrupt = encoded.clone();
            let at = corrupt.len() - footer_offset;
            corrupt[at] ^= 1;
            encoded_identity(&mut asset, &corrupt);
            assert_eq!(decode(&asset, &corrupt).err(), Some(Invalid::Gzip));
        }
        for suffix in [b"suffix".to_vec(), gzip(b"")] {
            let mut doubled = encoded.clone();
            doubled.extend_from_slice(&suffix);
            encoded_identity(&mut asset, &doubled);
            assert_eq!(decode(&asset, &doubled).err(), Some(Invalid::Suffix));
        }
    }
    #[test]
    fn independent_input_output_and_expanded_identity_limits_fail_closed() {
        let plain = b"synthetic\n";
        let encoded = gzip(plain);
        let asset = synthetic(plain, &encoded);
        assert_eq!(decode(&asset, &encoded).unwrap(), "synthetic\n");
        let mut bad = asset.clone();
        bad.stored_bytes += 1;
        assert_eq!(decode(&bad, &encoded).err(), Some(Invalid::StoredIdentity));
        let mut bad = asset.clone();
        bad.stored_sha256 = "0".repeat(64);
        assert_eq!(decode(&bad, &encoded).err(), Some(Invalid::StoredIdentity));
        let oversized = vec![0; INPUT_LIMIT + 1];
        assert_eq!(decode(&asset, &oversized).err(), Some(Invalid::StoredBound));
        let mut bad = asset.clone();
        bad.expanded_bytes = OUTPUT_LIMIT as u64 + 1;
        assert_eq!(decode(&bad, &encoded).err(), Some(Invalid::OutputBound));
        let mut bad = asset.clone();
        bad.expanded_bytes -= 1;
        assert_eq!(decode(&bad, &encoded).err(), Some(Invalid::OutputBound));
        let mut bad = asset.clone();
        bad.expanded_bytes += 1;
        assert_eq!(
            decode(&bad, &encoded).err(),
            Some(Invalid::ExpandedIdentity)
        );
        let mut bad = asset.clone();
        bad.expanded_sha256 = "0".repeat(64);
        assert_eq!(
            decode(&bad, &encoded).err(),
            Some(Invalid::ExpandedIdentity)
        );
    }
    #[test]
    fn header_utf8_row_and_manifest_negatives_do_not_normalize_bytes() {
        for (plain, reason) in [
            (b"\xff\n".as_slice(), Invalid::Utf8),
            (b"synthetic".as_slice(), Invalid::Rows),
            (b"\n".as_slice(), Invalid::Rows),
        ] {
            let encoded = gzip(plain);
            assert_eq!(
                decode(&synthetic(plain, &encoded), &encoded).err(),
                Some(reason)
            );
        }
        let plain = b"synthetic\n";
        let encoded = gzip(plain);
        let mut bad = synthetic(plain, &encoded);
        bad.rows = 2;
        assert_eq!(decode(&bad, &encoded).err(), Some(Invalid::Rows));
        let mut named = encoded.clone();
        named[3] = 8;
        let mut asset = synthetic(plain, &encoded);
        encoded_identity(&mut asset, &named);
        assert_eq!(decode(&asset, &named).err(), Some(Invalid::Header));
        let mut value: serde_json::Value = serde_json::from_str(MANIFEST).unwrap();
        value["assets"][0]["stored_path"] = "rust/parity/cases/adjacent.jsonl.gz".into();
        assert_eq!(manifest(&value.to_string()).err(), Some(Invalid::Manifest));
        value["assets"][0]["stored_path"] = STORED.into();
        for (key, replacement) in [
            ("encoding", serde_json::json!("gzip-concatenated")),
            ("expanded_bytes", serde_json::json!(3_970_357)),
        ] {
            let mut changed = value.clone();
            changed["assets"][0][key] = replacement;
            assert_eq!(
                manifest(&changed.to_string()).err(),
                Some(Invalid::Manifest)
            );
        }
        let mut changed = value.clone();
        changed["schema_version"] = 2.into();
        assert_eq!(
            manifest(&changed.to_string()).err(),
            Some(Invalid::Manifest)
        );
        value["unknown"] = true.into();
        assert_eq!(manifest(&value.to_string()).err(), Some(Invalid::Manifest));
        assert_eq!(
            manifest(&" ".repeat(16 * 1024 + 1)).err(),
            Some(Invalid::Manifest)
        );
    }
}
