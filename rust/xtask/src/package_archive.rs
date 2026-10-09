//! Restricted npm gzip/ustar verification. Never extracts untrusted paths.
use flate2::read::MultiGzDecoder;
use std::collections::BTreeMap;
use std::io::Read;
pub const MAX_ARCHIVE: usize = 32 * 1024 * 1024;
#[derive(Clone, Debug)]
pub struct Entry {
    pub bytes: Vec<u8>,
    pub mode: u32,
}
fn field(bytes: &[u8]) -> Result<&str, String> {
    let end = bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len());
    std::str::from_utf8(&bytes[..end]).map_err(|_| "Invalid tar text field".into())
}
fn octal(bytes: &[u8]) -> Result<u64, String> {
    let text = field(bytes)?.trim();
    if text.is_empty() || !text.bytes().all(|b| (b'0'..=b'7').contains(&b)) {
        return Err("Invalid tar numeric field".into());
    }
    u64::from_str_radix(text, 8).map_err(|_| "Invalid tar numeric field".into())
}
fn path_safe(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path.contains('\\')
        && path
            .split('/')
            .all(|c| !c.is_empty() && c != "." && c != ".." && !c.chars().any(char::is_control))
}
pub fn decode(bytes: &[u8]) -> Result<(BTreeMap<String, Entry>, usize), String> {
    decode_root(bytes, "package")
}
pub fn decode_root(bytes: &[u8], root: &str) -> Result<(BTreeMap<String, Entry>, usize), String> {
    if !path_safe(root) || root.contains('/') {
        return Err("Invalid archive root".into());
    }
    if bytes.len() > MAX_ARCHIVE {
        return Err("Compressed archive exceeds 32 MiB".into());
    }
    let mut expanded = Vec::new();
    MultiGzDecoder::new(bytes)
        .take(MAX_ARCHIVE as u64 + 1)
        .read_to_end(&mut expanded)
        .map_err(|_| "Invalid gzip archive")?;
    if expanded.len() > MAX_ARCHIVE {
        return Err("Expanded archive exceeds 32 MiB".into());
    }
    let mut files = BTreeMap::new();
    let mut seen = std::collections::HashSet::new();
    let mut offset = 0;
    let mut ended = false;
    while offset + 512 <= expanded.len() {
        let header = &expanded[offset..offset + 512];
        if header.iter().all(|b| *b == 0) {
            if expanded.len() - offset < 1024 || expanded[offset..].iter().any(|b| *b != 0) {
                return Err("Invalid tar end marker".into());
            }
            ended = true;
            break;
        }
        let expected = octal(&header[148..156])?;
        let actual: u64 = header
            .iter()
            .enumerate()
            .map(|(i, b)| {
                if (148..156).contains(&i) {
                    32
                } else {
                    *b as u64
                }
            })
            .sum();
        if expected != actual {
            return Err("Invalid tar header checksum".into());
        }
        let prefix = field(&header[345..500])?;
        let raw_name = field(&header[..100])?;
        let name = if prefix.is_empty() {
            raw_name.to_owned()
        } else {
            format!("{prefix}/{raw_name}")
        };
        let relative = name
            .strip_prefix(&format!("{root}/"))
            .ok_or("Tar entry is outside the declared root")?;
        let kind = header[156];
        let path = if kind == b'5' {
            relative.trim_end_matches('/')
        } else {
            relative
        };
        if !path_safe(path) || !seen.insert(path.to_owned()) {
            return Err("Unsafe or duplicate tar path".into());
        }
        let size = usize::try_from(octal(&header[124..136])?).map_err(|_| "Invalid tar size")?;
        let mode = u32::try_from(octal(&header[100..108])?).map_err(|_| "Invalid tar mode")?;
        if mode & !0o777 != 0 {
            return Err("Privileged tar file modes are not permitted".into());
        }
        let start = offset + 512;
        let end = start
            .checked_add(size)
            .filter(|end| *end <= expanded.len())
            .ok_or("Tar entry exceeds archive")?;
        match kind {
            0 | b'0' => {
                files.insert(
                    path.into(),
                    Entry {
                        bytes: expanded[start..end].to_vec(),
                        mode,
                    },
                );
            }
            b'5' if size == 0 => {}
            _ => return Err("Links and extended tar headers are not permitted".into()),
        }
        offset = start
            .checked_add(size.div_ceil(512) * 512)
            .ok_or("Invalid tar size")?;
    }
    if !ended || files.is_empty() {
        return Err("Incomplete or empty tar archive".into());
    }
    Ok((files, expanded.len()))
}
pub fn encode_root(root: &str, files: &BTreeMap<String, Entry>) -> Result<Vec<u8>, String> {
    use std::io::Write;
    if !path_safe(root) || root.contains('/') || files.is_empty() {
        return Err("Invalid archive root or empty archive".into());
    }
    let mut tar = Vec::new();
    for (path, entry) in files {
        if !path_safe(path) || entry.mode & !0o777 != 0 {
            return Err("Unsafe archive entry".into());
        }
        let full = format!("{root}/{path}");
        let (prefix, name) = if full.len() <= 100 {
            ("", full.as_str())
        } else {
            full.char_indices()
                .filter(|(_, c)| *c == '/')
                .rev()
                .find_map(|(at, _)| {
                    (at <= 155 && full.len() - at - 1 <= 100)
                        .then_some((&full[..at], &full[at + 1..]))
                })
                .ok_or("Archive path exceeds ustar bounds")?
        };
        let mut header = [0u8; 512];
        header[..name.len()].copy_from_slice(name.as_bytes());
        header[345..345 + prefix.len()].copy_from_slice(prefix.as_bytes());
        for (start, len, value) in [
            (100, 8, entry.mode as u64),
            (108, 8, 0),
            (116, 8, 0),
            (124, 12, entry.bytes.len() as u64),
            (136, 12, 0),
        ] {
            let text = format!("{:0width$o}\0", value, width = len - 1);
            if text.len() != len {
                return Err("Archive numeric field exceeds bounds".into());
            }
            header[start..start + len].copy_from_slice(text.as_bytes());
        }
        header[156] = b'0';
        header[257..263].copy_from_slice(b"ustar\0");
        header[263..265].copy_from_slice(b"00");
        header[148..156].fill(b' ');
        let checksum: u64 = header.iter().map(|b| *b as u64).sum();
        header[148..156].copy_from_slice(format!("{checksum:06o}\0 ").as_bytes());
        let expanded = tar
            .len()
            .checked_add(512 + entry.bytes.len().div_ceil(512) * 512 + 1024)
            .ok_or("Archive size overflow")?;
        if expanded > MAX_ARCHIVE {
            return Err("Expanded archive exceeds 32 MiB".into());
        }
        tar.extend(header);
        tar.extend(&entry.bytes);
        tar.resize(tar.len().next_multiple_of(512), 0);
    }
    tar.resize(tar.len() + 1024, 0);
    let mut writer = flate2::GzBuilder::new()
        .mtime(0)
        .write(Vec::new(), flate2::Compression::default());
    writer
        .write_all(&tar)
        .map_err(|_| "Cannot compress source bundle")?;
    let bytes = writer.finish().map_err(|_| "Cannot finish source bundle")?;
    if bytes.len() > MAX_ARCHIVE {
        return Err("Compressed archive exceeds 32 MiB".into());
    }
    Ok(bytes)
}
#[cfg(test)]
mod tests {
    use super::*;
    use flate2::{Compression, write::GzEncoder};
    use std::io::Write;
    fn archive(name: &str, kind: u8, content: &[u8]) -> Vec<u8> {
        let mut h = [0u8; 512];
        h[..name.len()].copy_from_slice(name.as_bytes());
        for (start, len, value) in [
            (100, 8, 0o644u64),
            (108, 8, 0),
            (116, 8, 0),
            (124, 12, content.len() as u64),
            (136, 12, 0),
        ] {
            let field = format!("{:0width$o}\0", value, width = len - 1);
            h[start..start + len].copy_from_slice(field.as_bytes());
        }
        h[156] = kind;
        h[148..156].fill(b' ');
        let checksum: u64 = h.iter().map(|b| *b as u64).sum();
        h[148..156].copy_from_slice(format!("{checksum:06o}\0 ").as_bytes());
        let mut tar = h.to_vec();
        tar.extend(content);
        tar.resize(512 + content.len().div_ceil(512) * 512 + 1024, 0);
        let mut gzip = GzEncoder::new(Vec::new(), Compression::default());
        gzip.write_all(&tar).unwrap();
        gzip.finish().unwrap()
    }
    #[test]
    fn rejects_traversal_links_bad_checksums_and_truncated_payloads() {
        for (name, kind) in [
            ("package/../secret", b'0'),
            ("outside/file", b'0'),
            ("package/link", b'2'),
            ("package/hard", b'1'),
            ("package/a//b", b'0'),
        ] {
            assert!(decode(&archive(name, kind, b"synthetic")).is_err());
        }
        let good = archive("package/README.md", b'0', b"synthetic");
        assert_eq!(decode(&good).unwrap().0["README.md"].bytes, b"synthetic");
        assert!(decode(&good[..good.len() - 1]).is_err());
        let mut tar = Vec::new();
        MultiGzDecoder::new(good.as_slice())
            .read_to_end(&mut tar)
            .unwrap();
        tar[0] ^= 1;
        let mut gzip = GzEncoder::new(Vec::new(), Compression::default());
        gzip.write_all(&tar).unwrap();
        assert_eq!(
            decode(&gzip.finish().unwrap()).unwrap_err(),
            "Invalid tar header checksum"
        );
    }
    #[test]
    fn duplicate_entries_are_rejected_before_extraction() {
        let bytes = archive("package/README.md", b'0', b"synthetic");
        let mut tar = Vec::new();
        MultiGzDecoder::new(bytes.as_slice())
            .read_to_end(&mut tar)
            .unwrap();
        let entry = tar[..1024].to_vec();
        tar.splice(1024..1024, entry);
        let mut gzip = GzEncoder::new(Vec::new(), Compression::default());
        gzip.write_all(&tar).unwrap();
        assert_eq!(
            decode(&gzip.finish().unwrap()).unwrap_err(),
            "Unsafe or duplicate tar path"
        );
    }
    #[test]
    fn decompression_bound_is_enforced_before_parsing_entries() {
        let mut gzip = GzEncoder::new(Vec::new(), Compression::default());
        gzip.write_all(&vec![0; MAX_ARCHIVE + 1]).unwrap();
        assert_eq!(
            decode(&gzip.finish().unwrap()).unwrap_err(),
            "Expanded archive exceeds 32 MiB"
        );
    }
}
