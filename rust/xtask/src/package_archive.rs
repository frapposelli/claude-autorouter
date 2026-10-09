//! Bounded gzip/ustar verification. Never extracts untrusted paths.
use flate2::read::MultiGzDecoder;
use std::collections::{BTreeMap, HashSet};
use std::io::{self, Read, Write};

pub const MAX_COMPRESSED: usize = 32 * 1024 * 1024;
pub const MAX_FILE: usize = 32 * 1024 * 1024;
pub const MAX_NATIVE_EXPANDED: usize = 64 * 1024 * 1024;
const MAX_OLD_EXPANDED: usize = 32 * 1024 * 1024;

#[derive(Clone, Copy, Debug)]
pub struct ArchivePolicy {
    pub compressed: usize,
    pub expanded: usize,
    pub file: usize,
    pub entries: usize,
}
pub const NATIVE: ArchivePolicy = ArchivePolicy {
    compressed: MAX_COMPRESSED,
    expanded: MAX_NATIVE_EXPANDED,
    file: MAX_FILE,
    entries: 256,
};
pub const HISTORICAL: ArchivePolicy = ArchivePolicy {
    compressed: MAX_COMPRESSED,
    expanded: MAX_OLD_EXPANDED,
    file: MAX_FILE,
    // Every prior tar entry used at least one block, plus two end blocks.
    entries: MAX_OLD_EXPANDED / 512 - 2,
};
pub const SOURCE: ArchivePolicy = HISTORICAL;
pub const MIXED: ArchivePolicy = ArchivePolicy {
    expanded: MAX_NATIVE_EXPANDED,
    ..HISTORICAL
};
impl ArchivePolicy {
    pub fn remaining(self, expanded: usize) -> Self {
        Self {
            expanded: self.expanded.min(expanded),
            ..self
        }
    }
}

#[derive(Clone, Debug)]
pub struct Entry {
    pub bytes: Vec<u8>,
    pub mode: u32,
}
#[derive(Clone, Debug)]
pub struct DecodedArchive {
    pub files: BTreeMap<String, Entry>,
    pub expanded_bytes: usize,
    pub entries: usize,
}
impl DecodedArchive {
    pub fn into_parts(self) -> (BTreeMap<String, Entry>, usize) {
        (self.files, self.expanded_bytes)
    }
    /// Recheck the selected format after an explicitly mixed-format inspection.
    pub fn require_policy(&self, policy: ArchivePolicy) -> Result<(), String> {
        if self.expanded_bytes > policy.expanded {
            return Err(bound("Expanded archive", policy.expanded));
        }
        if self.entries > policy.entries {
            return Err("Archive entry count exceeds limit".into());
        }
        if self
            .files
            .values()
            .any(|file| file.bytes.len() > policy.file)
        {
            return Err(bound("Individual archive entry", policy.file));
        }
        Ok(())
    }
}
fn bound(kind: &str, maximum: usize) -> String {
    if maximum.is_multiple_of(1024 * 1024) && maximum != 0 {
        format!("{kind} exceeds {} MiB", maximum / (1024 * 1024))
    } else {
        format!("{kind} exceeds {maximum} bytes")
    }
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
fn padded(size: usize) -> Result<usize, String> {
    size.checked_add(511)
        .map(|size| size / 512 * 512)
        .ok_or_else(|| "Archive size overflow".into())
}
struct Inflater<'a> {
    reader: MultiGzDecoder<&'a [u8]>,
    maximum: usize,
    count: usize,
}
impl Read for Inflater<'_> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if output.is_empty() {
            return Ok(0);
        }
        let remaining = self.maximum - self.count;
        if remaining == 0 {
            let mut probe = [0];
            return if self.reader.read(&mut probe)? == 0 {
                Ok(0)
            } else {
                Err(io::Error::new(
                    io::ErrorKind::FileTooLarge,
                    bound("Expanded archive", self.maximum),
                ))
            };
        }
        let maximum = remaining.min(output.len());
        let count = self.reader.read(&mut output[..maximum])?;
        self.count += count;
        Ok(count)
    }
}
fn read_error(error: io::Error) -> String {
    if error.kind() == io::ErrorKind::FileTooLarge {
        error.to_string()
    } else if error.kind() == io::ErrorKind::UnexpectedEof {
        "Incomplete tar archive".into()
    } else {
        "Invalid gzip archive".into()
    }
}
pub fn decode(bytes: &[u8], policy: ArchivePolicy) -> Result<DecodedArchive, String> {
    decode_root(bytes, "package", policy)
}
pub fn decode_root(
    bytes: &[u8],
    root: &str,
    policy: ArchivePolicy,
) -> Result<DecodedArchive, String> {
    if !path_safe(root) || root.contains('/') {
        return Err("Invalid archive root".into());
    }
    if bytes.len() > policy.compressed {
        return Err(bound("Compressed archive", policy.compressed));
    }
    let mut reader = Inflater {
        reader: MultiGzDecoder::new(bytes),
        maximum: policy.expanded,
        count: 0,
    };
    let mut files = BTreeMap::new();
    let mut seen = HashSet::new();
    let mut entries = 0usize;
    loop {
        let mut header = [0u8; 512];
        reader.read_exact(&mut header).map_err(read_error)?;
        if header.iter().all(|b| *b == 0) {
            reader.read_exact(&mut header).map_err(read_error)?;
            if header.iter().any(|b| *b != 0) {
                return Err("Invalid tar end marker".into());
            }
            let mut tail = [0; 8192];
            loop {
                let count = reader.read(&mut tail).map_err(read_error)?;
                if count == 0 {
                    break;
                }
                if tail[..count].iter().any(|b| *b != 0) {
                    return Err("Invalid tar end marker".into());
                }
            }
            break;
        }
        // Bound count before allocating path strings, sets, maps or payloads.
        entries = entries
            .checked_add(1)
            .ok_or("Archive entry count overflow")?;
        if entries > policy.entries {
            return Err("Archive entry count exceeds limit".into());
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
        let size = usize::try_from(octal(&header[124..136])?).map_err(|_| "Invalid tar size")?;
        if size > policy.file {
            return Err(bound("Individual archive entry", policy.file));
        }
        let padded = padded(size)?;
        let minimum = reader
            .count
            .checked_add(padded)
            .and_then(|n| n.checked_add(1024))
            .ok_or("Archive size overflow")?;
        if minimum > policy.expanded {
            return Err(bound("Expanded archive", policy.expanded));
        }
        let prefix = field(&header[345..500])?;
        let raw = field(&header[..100])?;
        let name = if prefix.is_empty() {
            raw.to_owned()
        } else {
            format!("{prefix}/{raw}")
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
        let mode = u32::try_from(octal(&header[100..108])?).map_err(|_| "Invalid tar mode")?;
        if mode & !0o777 != 0 {
            return Err("Privileged tar file modes are not permitted".into());
        }
        match kind {
            0 | b'0' => {
                let mut payload = Vec::new();
                payload
                    .try_reserve_exact(size)
                    .map_err(|_| "Cannot allocate bounded archive entry")?;
                payload.resize(size, 0);
                reader.read_exact(&mut payload).map_err(read_error)?;
                files.insert(
                    path.into(),
                    Entry {
                        bytes: payload,
                        mode,
                    },
                );
            }
            b'5' if size == 0 => {}
            _ => return Err("Links and extended tar headers are not permitted".into()),
        }
        let mut padding = [0; 512];
        reader
            .read_exact(&mut padding[..padded - size])
            .map_err(read_error)?;
    }
    if files.is_empty() {
        return Err("Incomplete or empty tar archive".into());
    }
    Ok(DecodedArchive {
        files,
        expanded_bytes: reader.count,
        entries,
    })
}
struct CompressedWriter {
    bytes: Vec<u8>,
    maximum: usize,
}
impl Write for CompressedWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let end = self
            .bytes
            .len()
            .checked_add(bytes.len())
            .filter(|n| *n <= self.maximum)
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::FileTooLarge,
                    bound("Compressed archive", self.maximum),
                )
            })?;
        if end > self.bytes.capacity() {
            // Clamp growth before allocation, including the gzip trailer.
            let capacity = end
                .max(self.bytes.capacity().saturating_mul(2))
                .min(self.maximum);
            self.bytes
                .try_reserve_exact(capacity - self.bytes.len())
                .map_err(|_| io::Error::other("Cannot allocate bounded compressed archive"))?;
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
pub fn expanded_size(
    files: &BTreeMap<String, Entry>,
    policy: ArchivePolicy,
) -> Result<usize, String> {
    if files.is_empty() || files.len() > policy.entries {
        return Err("Archive entry count exceeds limit".into());
    }
    let mut expanded = 1024usize;
    for (path, entry) in files {
        if !path_safe(path) || entry.mode & !0o777 != 0 {
            return Err("Unsafe archive entry".into());
        }
        if entry.bytes.len() > policy.file {
            return Err(bound("Individual archive entry", policy.file));
        }
        expanded = expanded
            .checked_add(512)
            .and_then(|n| n.checked_add(padded(entry.bytes.len()).ok()?))
            .ok_or("Archive size overflow")?;
        if expanded > policy.expanded {
            return Err(bound("Expanded archive", policy.expanded));
        }
    }
    Ok(expanded)
}
pub fn encode_root(
    root: &str,
    files: &BTreeMap<String, Entry>,
    policy: ArchivePolicy,
) -> Result<Vec<u8>, String> {
    if !path_safe(root) || root.contains('/') {
        return Err("Invalid archive root".into());
    }
    expanded_size(files, policy)?;
    let sink = CompressedWriter {
        bytes: Vec::new(),
        maximum: policy.compressed,
    };
    let mut writer = flate2::GzBuilder::new()
        .mtime(0)
        .write(sink, flate2::Compression::default());
    for (path, entry) in files {
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
        writer
            .write_all(&header)
            .and_then(|()| writer.write_all(&entry.bytes))
            .and_then(|()| {
                writer
                    .write_all(&[0; 512][..padded(entry.bytes.len()).unwrap() - entry.bytes.len()])
            })
            .map_err(|e| e.to_string())?;
    }
    writer.write_all(&[0; 1024]).map_err(|e| e.to_string())?;
    Ok(writer.finish().map_err(|e| e.to_string())?.bytes)
}

#[cfg(test)]
#[path = "package_archive_tests.rs"]
mod tests;
