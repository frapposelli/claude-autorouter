use super::*;
use flate2::{Compression, write::GzEncoder};

fn checksum(header: &mut [u8]) {
    header[148..156].fill(b' ');
    let sum: u64 = header.iter().map(|b| *b as u64).sum();
    header[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
}
fn header(name: &str, kind: u8, size: usize) -> [u8; 512] {
    let mut h = [0; 512];
    h[..name.len()].copy_from_slice(name.as_bytes());
    for (start, len, value) in [
        (100, 8, 0o644u64),
        (108, 8, 0),
        (116, 8, 0),
        (124, 12, size as u64),
        (136, 12, 0),
    ] {
        h[start..start + len]
            .copy_from_slice(format!("{value:0width$o}\0", width = len - 1).as_bytes());
    }
    h[156] = kind;
    checksum(&mut h);
    h
}
fn gzip(tar: &[u8]) -> Vec<u8> {
    let mut writer = GzEncoder::new(Vec::new(), Compression::fast());
    writer.write_all(tar).unwrap();
    writer.finish().unwrap()
}
fn tar(name: &str, kind: u8, content: &[u8]) -> Vec<u8> {
    let mut tar = header(name, kind, content.len()).to_vec();
    tar.extend(content);
    tar.resize(512 + padded(content.len()).unwrap() + 1024, 0);
    tar
}
fn files(size: usize) -> BTreeMap<String, Entry> {
    BTreeMap::from([(
        "file".into(),
        Entry {
            bytes: vec![0; size],
            mode: 0o644,
        },
    )])
}
#[test]
fn rejects_traversal_links_bad_checksums_and_truncated_payloads() {
    for (name, kind) in [
        ("package/../secret", b'0'),
        ("outside/file", b'0'),
        ("package/link", b'2'),
        ("package/hard", b'1'),
        ("package/a//b", b'0'),
        ("package/./a", b'0'),
        ("package/evil\\name", b'0'),
        ("package/pax", b'x'),
    ] {
        assert!(decode(&gzip(&tar(name, kind, b"synthetic")), NATIVE).is_err());
    }
    let good = gzip(&tar("package/README.md", b'0', b"synthetic"));
    assert_eq!(
        decode(&good, NATIVE).unwrap().files["README.md"].bytes,
        b"synthetic"
    );
    assert!(decode(&good[..good.len() - 1], NATIVE).is_err());
    let mut bad = tar("package/README.md", b'0', b"synthetic");
    bad[0] ^= 1;
    assert_eq!(
        decode(&gzip(&bad), NATIVE).unwrap_err(),
        "Invalid tar header checksum"
    );
    assert!(
        decode(
            &gzip(&tar("package/README.md", b'0', b"synthetic")[..513]),
            NATIVE
        )
        .is_err()
    );
}
#[test]
fn duplicate_paths_and_privileged_modes_are_rejected() {
    let mut bad = tar("package/README.md", b'0', b"synthetic");
    let entry = bad[..1024].to_vec();
    bad.splice(1024..1024, entry);
    assert_eq!(
        decode(&gzip(&bad), NATIVE).unwrap_err(),
        "Unsafe or duplicate tar path"
    );
    let mut bad = tar("package/README.md", b'0', b"synthetic");
    bad[100..108].copy_from_slice(b"0004755\0");
    checksum(&mut bad[..512]);
    assert_eq!(
        decode(&gzip(&bad), NATIVE).unwrap_err(),
        "Privileged tar file modes are not permitted"
    );
}
#[test]
fn exact_native_expanded_and_file_limits_with_historical_rejection() {
    // Two payloads occupy exactly 64 MiB including their headers/end blocks.
    let mut input = files(MAX_FILE);
    input.insert(
        "second".into(),
        Entry {
            bytes: vec![0; MAX_NATIVE_EXPANDED - MAX_FILE - 2048],
            mode: 0o755,
        },
    );
    assert_eq!(expanded_size(&input, NATIVE).unwrap(), MAX_NATIVE_EXPANDED);
    assert_eq!(
        expanded_size(&input, HISTORICAL).unwrap_err(),
        "Expanded archive exceeds 32 MiB"
    );
    let encoded = encode_root("package", &input, NATIVE).unwrap();
    drop(input);
    let decoded = decode(&encoded, NATIVE).unwrap();
    assert_eq!(decoded.expanded_bytes, MAX_NATIVE_EXPANDED);
    assert_eq!(decoded.files["file"].bytes.len(), MAX_FILE);
    assert_eq!(decoded.files["second"].mode, 0o755);
    assert_eq!(
        decoded.require_policy(HISTORICAL).unwrap_err(),
        "Expanded archive exceeds 32 MiB"
    );
    drop(decoded);
    assert_eq!(
        decode(&encoded, HISTORICAL).unwrap_err(),
        "Expanded archive exceeds 32 MiB"
    );
    let mut extra = encoded.clone();
    extra.extend(gzip(&[0]));
    assert_eq!(
        decode(&extra, NATIVE).unwrap_err(),
        "Expanded archive exceeds 64 MiB"
    );
}
#[test]
fn exact_historical_and_source_expansion_stays_32_mib() {
    let input = files(MAX_OLD_EXPANDED - 1536);
    let encoded = encode_root("source", &input, SOURCE).unwrap();
    drop(input);
    assert_eq!(
        decode_root(&encoded, "source", SOURCE)
            .unwrap()
            .expanded_bytes,
        MAX_OLD_EXPANDED
    );
    let mut extra = encoded;
    extra.extend(gzip(&[0]));
    assert_eq!(
        decode_root(&extra, "source", SOURCE).unwrap_err(),
        "Expanded archive exceeds 32 MiB"
    );
}
#[test]
fn forged_file_size_is_rejected_before_payload_allocation() {
    for size in [MAX_FILE + 1, 0o77777777777usize] {
        let bytes = gzip(&header("package/file", b'0', size));
        assert_eq!(
            decode(&bytes, NATIVE).unwrap_err(),
            "Individual archive entry exceeds 32 MiB"
        );
    }
    let bytes = gzip(&header("package/file", b'0', 4096));
    assert_eq!(
        decode(&bytes, NATIVE.remaining(4096)).unwrap_err(),
        "Expanded archive exceeds 4096 bytes"
    );
    assert!(padded(usize::MAX).is_err());
    let mut forged = header("package/file", b'0', 1);
    forged[124] = 0x80;
    checksum(&mut forged);
    assert_eq!(
        decode(&gzip(&forged), NATIVE).unwrap_err(),
        "Invalid tar text field"
    );
}
#[test]
fn entry_count_is_checked_before_path_and_payload_metadata() {
    let mut input = files(0);
    for n in 1..256 {
        input.insert(
            format!("f{n}"),
            Entry {
                bytes: vec![],
                mode: 0o644,
            },
        );
    }
    let encoded = encode_root("package", &input, NATIVE).unwrap();
    assert_eq!(decode(&encoded, NATIVE).unwrap().entries, 256);
    input.insert(
        "last".into(),
        Entry {
            bytes: vec![],
            mode: 0o644,
        },
    );
    assert_eq!(
        encode_root("package", &input, NATIVE).unwrap_err(),
        "Archive entry count exceeds limit"
    );
    let encoded = encode_root("package", &input, HISTORICAL).unwrap();
    assert_eq!(
        decode(&encoded, NATIVE).unwrap_err(),
        "Archive entry count exceeds limit"
    );
    let mixed = decode(&encoded, MIXED).unwrap();
    assert_eq!(mixed.entries, 257);
    assert!(mixed.require_policy(HISTORICAL).is_ok());
    assert!(mixed.require_policy(NATIVE).is_err());
    // Invalid UTF-8 in the 257th header must not be parsed or allocated.
    let mut raw = Vec::new();
    for n in 0..256 {
        raw.extend(header(&format!("package/dir{n}"), b'5', 0));
    }
    let mut last = header("package/last", b'0', MAX_FILE);
    last[0] = 255;
    checksum(&mut last);
    raw.extend(last);
    assert_eq!(
        decode(&gzip(&raw), NATIVE).unwrap_err(),
        "Archive entry count exceeds limit"
    );
}
#[test]
fn concatenated_members_share_one_expansion_budget_and_cannot_hide_data() {
    let raw = tar("package/file", b'0', b"test");
    let mut joined = gzip(&raw[..515]);
    joined.extend(gzip(&raw[515..]));
    assert_eq!(
        decode(&joined, NATIVE.remaining(raw.len())).unwrap().files["file"].bytes,
        b"test"
    );
    assert_eq!(
        decode(&joined, NATIVE.remaining(raw.len() - 1)).unwrap_err(),
        format!("Expanded archive exceeds {} bytes", raw.len() - 1)
    );
    joined.extend(gzip(&[1]));
    assert_eq!(
        decode(&joined, NATIVE).unwrap_err(),
        "Invalid tar end marker"
    );
    let mut corrupt = gzip(&raw);
    corrupt.extend(b"not gzip");
    assert!(decode(&corrupt, NATIVE).is_err());
}
#[test]
fn zero_padding_bomb_has_bounded_expansion() {
    let mut writer = GzEncoder::new(Vec::new(), Compression::fast());
    for _ in 0..MAX_NATIVE_EXPANDED / 8192 {
        writer.write_all(&[0; 8192]).unwrap();
    }
    writer.write_all(&[0]).unwrap();
    assert_eq!(
        decode(&writer.finish().unwrap(), NATIVE).unwrap_err(),
        "Expanded archive exceeds 64 MiB"
    );
}
#[test]
fn compressed_input_and_writer_limits_are_inclusive_and_checked_before_growth() {
    let mut bytes = vec![0; MAX_COMPRESSED];
    assert_eq!(decode(&bytes, NATIVE).unwrap_err(), "Invalid gzip archive");
    bytes.push(0);
    assert_eq!(
        decode(&bytes, NATIVE).unwrap_err(),
        "Compressed archive exceeds 32 MiB"
    );
    let mut writer = CompressedWriter {
        bytes: Vec::new(),
        maximum: MAX_COMPRESSED,
    };
    writer.write_all(&bytes[..MAX_COMPRESSED]).unwrap();
    assert_eq!(writer.bytes.len(), MAX_COMPRESSED);
    let capacity = writer.bytes.capacity();
    assert!(writer.write_all(&[1]).is_err());
    assert_eq!(writer.bytes.len(), MAX_COMPRESSED);
    assert_eq!(writer.bytes.capacity(), capacity);
    let input = files(1);
    let encoded = encode_root("package", &input, NATIVE).unwrap();
    let exact = ArchivePolicy {
        compressed: encoded.len(),
        ..NATIVE
    };
    assert_eq!(encode_root("package", &input, exact).unwrap(), encoded);
    assert!(
        encode_root(
            "package",
            &input,
            ArchivePolicy {
                compressed: encoded.len() - 1,
                ..NATIVE
            }
        )
        .is_err()
    );
}
#[test]
fn remaining_release_budget_is_enforced_before_allocating_next_entry() {
    let encoded = encode_root("package", &files(512), NATIVE).unwrap();
    let first = decode(&encoded, NATIVE.remaining(4096)).unwrap();
    assert_eq!(first.expanded_bytes, 2048);
    let second = decode(&encoded, NATIVE.remaining(4096 - first.expanded_bytes)).unwrap();
    assert_eq!(second.expanded_bytes, 2048);
    assert_eq!(
        decode(&encoded, NATIVE.remaining(0)).unwrap_err(),
        "Expanded archive exceeds 0 bytes"
    );
    assert_eq!(
        decode(
            &gzip(&header("package/file", b'0', MAX_FILE)),
            NATIVE.remaining(2048)
        )
        .unwrap_err(),
        "Expanded archive exceeds 2048 bytes"
    );
}
#[test]
fn empty_truncated_or_nonzero_end_markers_are_rejected() {
    assert!(decode(&gzip(&[0; 1024]), NATIVE).is_err());
    let raw = tar("package/file", b'0', b"x");
    for len in [0, 511, 512, 513, 1024, 1535, 1536, 2047] {
        assert!(decode(&gzip(&raw[..len]), NATIVE).is_err(), "len={len}");
    }
    let mut raw = raw;
    raw[1536] = 1;
    assert_eq!(
        decode(&gzip(&raw), NATIVE).unwrap_err(),
        "Invalid tar end marker"
    );
}
#[test]
fn policy_constants_are_coherent_with_distribution_metadata() {
    let matrix: serde_json::Value =
        serde_json::from_str(include_str!("../../distribution/platforms.json")).unwrap();
    assert_eq!(
        matrix["artifact_caps"],
        serde_json::json!({"compressed_bytes":NATIVE.compressed,"expanded_tar_bytes":NATIVE.expanded,"file_bytes":NATIVE.file,"entries":NATIVE.entries})
    );
    assert_eq!(HISTORICAL.expanded, 32 * 1024 * 1024);
    assert_eq!(SOURCE.expanded, HISTORICAL.expanded);
    assert_eq!(MIXED.expanded, NATIVE.expanded);
    assert_eq!(MIXED.entries, HISTORICAL.entries);
}

#[test]
fn historical_non_block_aligned_zero_tail_remains_accepted() {
    let mut encoded = gzip(&tar("package/file", b'0', b"x"));
    encoded.extend(gzip(&[0]));
    for policy in [NATIVE, HISTORICAL, SOURCE, MIXED] {
        let decoded = decode(&encoded, policy).unwrap();
        assert_eq!(decoded.expanded_bytes, 2049);
        assert_eq!(decoded.files["file"].bytes, b"x");
    }
}
