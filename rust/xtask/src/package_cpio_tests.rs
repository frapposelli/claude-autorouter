//! Independent golden framing and adversarial CPIO ingress, without extraction.
use super::super::SOURCE;
use super::*;
use std::cell::Cell;

fn one() -> BTreeMap<String, Entry> {
    BTreeMap::from([(
        "a".into(),
        Entry {
            bytes: b"abc".to_vec(),
            mode: 0o644,
        },
    )])
}
fn golden() -> Vec<u8> {
    // Independently fixed110-byte headers, not production header()/encode_root.
    let first=b"07070100000001000081a40000000000000000000000010000000000000003000000000000000000000000000000000000000700000000";
    let trailer=b"07070100000000000000000000000000000000000000010000000000000000000000000000000000000000000000000000000b00000000";
    assert_eq!((first.len(), trailer.len()), (110, 110));
    [
        first.as_slice(),
        &b"root/a\0\0\0\0abc\0"[..],
        trailer.as_slice(),
        &b"TRAILER!!!\0\0\0\0"[..],
    ]
    .concat()
}
fn gzip(raw: &[u8]) -> Vec<u8> {
    let mut encoder = flate2::GzBuilder::new()
        .mtime(0)
        .write(Vec::new(), flate2::Compression::default());
    encoder.write_all(raw).unwrap();
    encoder.finish().unwrap()
}
fn raw(bytes: &[u8]) -> Vec<u8> {
    let mut decoded = Vec::new();
    MultiGzDecoder::new(bytes)
        .read_to_end(&mut decoded)
        .unwrap();
    decoded
}
fn scalar(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 8].copy_from_slice(format!("{value:08x}").as_bytes());
}
fn reject(bytes: &[u8]) -> String {
    let error = decode_root(&gzip(bytes), "root", SOURCE, |_| 1024 * 1024).unwrap_err();
    assert!(!error.contains("PRIVATE"));
    error
}
#[test]
fn exact_golden_framing_preserves_original_data_modes_and_determinism() {
    let expected = golden();
    assert_eq!(expected.len(), 248);
    let encoded = encode_root("root", &one(), SOURCE, |_| 1024 * 1024).unwrap();
    let mut stored = expected.clone();
    stored.resize(512, 0);
    assert_eq!(raw(&encoded), stored);
    assert_eq!(
        encode_root("root", &one(), SOURCE, |_| 1024 * 1024).unwrap(),
        encoded
    );
    let decoded = decode_root(&encoded, "root", SOURCE, |_| 1024 * 1024).unwrap();
    assert_eq!(
        (decoded.files.len(), decoded.entries, decoded.expanded_bytes),
        (1, 1, 512)
    );
    assert_eq!(decoded.files["a"].bytes, b"abc");
    assert_eq!(decoded.files["a"].mode, 0o644);
    let files = BTreeMap::from([
        (
            "a".into(),
            Entry {
                bytes: vec![],
                mode: 0o644,
            },
        ),
        (
            "utf8/é".into(),
            Entry {
                bytes: vec![0, 255, 10],
                mode: 0o644,
            },
        ),
    ]);
    let encoded = encode_root("root", &files, SOURCE, |_| 1024 * 1024).unwrap();
    let decoded = decode_root(&encoded, "root", SOURCE, |_| 1024 * 1024).unwrap();
    for (path, entry) in files {
        assert_eq!(decoded.files[&path].bytes, entry.bytes);
        assert_eq!(decoded.files[&path].mode, entry.mode);
    }
}
#[test]
fn old_tar_filename_and_per_file_domains_are_preserved_before_allocation() {
    let original = golden();
    let seen = Cell::new(0);
    let error = decode_root(&gzip(&original), "root", SOURCE, |_| {
        seen.set(seen.get() + 1);
        2
    })
    .unwrap_err();
    assert!(error.contains("Individual source entry"));
    assert_eq!(seen.get(), 1);
    assert!(encode_root("root", &one(), SOURCE, |_| 2).is_err());
    let good = format!("{}/{}", "d".repeat(150), "n".repeat(100));
    let bad = "n".repeat(101);
    let good_map = BTreeMap::from([(
        good,
        Entry {
            bytes: vec![],
            mode: 0o644,
        },
    )]);
    assert!(encode_root("root", &good_map, SOURCE, |_| 1024 * 1024).is_ok());
    let bad_map = BTreeMap::from([(
        bad,
        Entry {
            bytes: vec![],
            mode: 0o644,
        },
    )]);
    assert!(encode_root("root", &bad_map, SOURCE, |_| 1024 * 1024).is_err());
    let mut oversized = original.clone();
    scalar(&mut oversized, 94, 258);
    assert!(reject(&oversized).contains("pathname exceeds bound"));
    let mut oversized = original.clone();
    scalar(&mut oversized, 54, u32::MAX);
    assert!(reject(&oversized).contains("Individual source entry"));
    assert!(add(usize::MAX, 1).is_err());
    let policy = ArchivePolicy { file: 2, ..SOURCE };
    assert!(decode_root(&gzip(&original), "root", policy, |_| usize::MAX).is_err());
}
#[test]
fn every_non_regular_privileged_link_crc_or_rdev_header_is_rejected() {
    for mode in [
        0o040644, 0o120644, 0o020644, 0o010644, 0o100600, 0o104644, 0o102644, 0o101644,
    ] {
        let mut bytes = golden();
        scalar(&mut bytes, 14, mode);
        assert!(reject(&bytes).contains("regular"));
    }
    for links in [0, 2, u32::MAX] {
        let mut bytes = golden();
        scalar(&mut bytes, 38, links);
        assert!(reject(&bytes).contains("regular"));
    }
    for offset in [78, 86, 102] {
        let mut bytes = golden();
        scalar(&mut bytes, offset, 1);
        assert!(reject(&bytes).contains("regular"));
    }
    let mut bytes = golden();
    bytes[..6].copy_from_slice(b"070702");
    assert!(reject(&bytes).contains("magic"));
    let mut bytes = golden();
    bytes[6] = b'x';
    assert!(reject(&bytes).contains("hexadecimal"));
    let mut files = one();
    files.get_mut("a").unwrap().mode = 0o755;
    assert!(encode_root("root", &files, SOURCE, |_| 1024 * 1024).is_err());
}
#[test]
fn unsafe_unknown_duplicate_utf8_or_unterminated_names_never_resolve() {
    for name in [
        b"../a/x".as_slice(),
        b"/abs/a",
        b"root//",
        b"root/.",
        b"root/\n",
    ] {
        assert_eq!(name.len(), 6);
        let mut bytes = golden();
        bytes[110..116].copy_from_slice(name);
        reject(&bytes);
    }
    let mut bytes = golden();
    bytes[110] = 255;
    assert!(reject(&bytes).contains("UTF8"));
    let mut bytes = golden();
    bytes[116] = b'x';
    assert!(reject(&bytes).contains("terminator"));
    let mut bytes = golden();
    bytes[111] = 0;
    assert!(reject(&bytes).contains("terminator"));
    let bytes = golden();
    let duplicate = [&bytes[..124], bytes.as_slice()].concat();
    assert!(reject(&duplicate).contains("Duplicate"));
    assert!(decode_root(&gzip(&golden()), "else", SOURCE, |_| 1024 * 1024).is_err());
    assert!(decode_root(&gzip(&golden()), "bad/root", SOURCE, |_| 1024 * 1024).is_err());
}
#[test]
fn mandatory_trailer_padding_and_complete_gzip_eof_are_verified() {
    let bytes = golden();
    assert!(reject(&bytes[..124]).contains("Incomplete"));
    for offset in [117, 118, 119, 123, 245, 246, 247] {
        let mut bytes = golden();
        bytes[offset] = 1;
        assert!(reject(&bytes).contains("padding"));
    }
    for offset in [
        124 + 6,
        124 + 14,
        124 + 22,
        124 + 30,
        124 + 38,
        124 + 46,
        124 + 54,
        124 + 62,
        124 + 70,
        124 + 78,
        124 + 86,
        124 + 102,
    ] {
        let mut bytes = golden();
        let changed = if offset == 124 + 38 { 0 } else { 1 };
        scalar(&mut bytes, offset, changed);
        assert!(reject(&bytes).contains("trailer"));
    }
    let mut with_zero = bytes.clone();
    with_zero.extend_from_slice(&[0; 268]);
    assert_eq!(
        decode_root(&gzip(&with_zero), "root", SOURCE, |_| 1024 * 1024)
            .unwrap()
            .expanded_bytes,
        516
    );
    let mut extra = bytes.clone();
    extra.push(1);
    assert!(reject(&extra).contains("after CPIO trailer"));
    assert!(reject(&[bytes.as_slice(), bytes.as_slice()].concat()).contains("after CPIO trailer"));
    let mut encoded = gzip(&bytes);
    let last = encoded.len() - 8;
    encoded[last] ^= 1;
    assert!(
        decode_root(&encoded, "root", SOURCE, |_| 1024 * 1024)
            .unwrap_err()
            .contains("gzip")
    );
    let valid = gzip(&bytes);
    let truncated = &valid[..valid.len() - 4];
    assert!(decode_root(truncated, "root", SOURCE, |_| 1024 * 1024).is_err());
    let concatenated = [valid.as_slice(), valid.as_slice()].concat();
    assert!(
        decode_root(&concatenated, "root", SOURCE, |_| 1024 * 1024)
            .unwrap_err()
            .contains("after CPIO trailer")
    );
}
#[test]
fn compressed_expanded_count_and_remaining_budgets_are_independent() {
    let bytes = golden();
    let encoded = gzip(&bytes);
    for policy in [
        ArchivePolicy {
            compressed: encoded.len() - 1,
            ..SOURCE
        },
        ArchivePolicy {
            expanded: 247,
            ..SOURCE
        },
        ArchivePolicy {
            entries: 0,
            ..SOURCE
        },
    ] {
        assert!(decode_root(&encoded, "root", policy, |_| 1024 * 1024).is_err());
        assert!(encode_root("root", &one(), policy, |_| 1024 * 1024).is_err());
    }
    assert_eq!(
        expanded_size("root", &one(), SOURCE, |_| 1024 * 1024).unwrap(),
        512
    );
    assert!(decode_root(&encoded, "root", SOURCE.remaining(247), |_| 1024 * 1024).is_err());
    let mut with_zero = bytes.clone();
    with_zero.extend_from_slice(&[0; 268]);
    assert!(
        decode_root(&gzip(&with_zero), "root", SOURCE.remaining(515), |_| 1024
            * 1024)
        .is_err()
    );
    assert!(
        decode_root(&gzip(&with_zero), "root", SOURCE.remaining(516), |_| 1024
            * 1024)
        .is_ok()
    );
    let empty = BTreeMap::new();
    assert!(encode_root("root", &empty, SOURCE, |_| 1024 * 1024).is_err());
    assert!(decode_root(&gzip(&golden()[124..]), "root", SOURCE, |_| 1024 * 1024).is_err());
}
