//! Strict newc source carrier. No filesystem extraction or format fallback.
use super::{ArchivePolicy, CompressedWriter, DecodedArchive, Entry, Inflater, bound, path_safe};
use flate2::read::MultiGzDecoder;
use std::collections::BTreeMap;
use std::io::{self, Read, Write};

const HEADER: usize = 110;
const NAME_MAX: usize = 257; // Existing ustar256-byte full pathname plus NUL.
const TRAILER: &str = "TRAILER!!!";
const END_BYTES: usize = 124; // align4(110 + 11), zero payload.
fn padding(value: usize) -> usize {
    (4 - value % 4) % 4
}
fn add(total: usize, value: usize) -> Result<usize, String> {
    total
        .checked_add(value)
        .ok_or_else(|| "CPIO byte sum overflow".into())
}
fn error(error: io::Error) -> String {
    match error.kind() {
        io::ErrorKind::FileTooLarge => error.to_string(),
        io::ErrorKind::UnexpectedEof => "Incomplete CPIO archive".into(),
        _ => "Invalid gzip archive".into(),
    }
}
fn zeros(reader: &mut Inflater<'_>, count: usize) -> Result<(), String> {
    let mut bytes = [0; 3];
    reader.read_exact(&mut bytes[..count]).map_err(error)?;
    if bytes[..count].iter().any(|byte| *byte != 0) {
        return Err("Nonzero CPIO alignment padding".into());
    }
    Ok(())
}
fn root_valid(root: &str) -> bool {
    path_safe(root) && !root.contains('/')
}
// Preserve the original ustar filename domain, independently of newc framing.
fn name_valid(name: &str) -> bool {
    name.len() <= 100
        || name
            .char_indices()
            .filter(|(_, c)| *c == '/')
            .any(|(at, _)| at <= 155 && name.len() - at - 1 <= 100)
}
/// `per_file` is the trusted source policy, intersected with ArchivePolicy.file.
/// It may distinguish only the exact predeclared four-MiB synthetic corpora.
pub(crate) fn decode_root(
    bytes: &[u8],
    root: &str,
    policy: ArchivePolicy,
    per_file: impl Fn(&str) -> usize,
) -> Result<DecodedArchive, String> {
    if !root_valid(root) {
        return Err("Invalid CPIO archive root".into());
    }
    if bytes.len() > policy.compressed {
        return Err(bound("Compressed archive", policy.compressed));
    }
    let mut reader = Inflater {
        reader: MultiGzDecoder::new(bytes),
        maximum: policy.expanded,
        count: 0,
    };
    let mut files = BTreeMap::<String, Entry>::new();
    loop {
        let mut header = [0; HEADER];
        reader.read_exact(&mut header).map_err(error)?;
        if &header[..6] != b"070701" {
            return Err("Unsupported CPIO magic".into());
        }
        let mut fields = [0u32; 13];
        for (field, bytes) in fields.iter_mut().zip(header[6..].as_chunks::<8>().0) {
            if !bytes.iter().all(u8::is_ascii_hexdigit) {
                return Err("Invalid CPIO hexadecimal field".into());
            }
            *field = u32::from_str_radix(
                std::str::from_utf8(bytes).map_err(|_| "Invalid CPIO hexadecimal field")?,
                16,
            )
            .map_err(|_| "Invalid CPIO hexadecimal field")?;
        }
        let namesize = usize::try_from(fields[11]).map_err(|_| "Invalid CPIO name size")?;
        if !(1..=NAME_MAX).contains(&namesize) {
            return Err("CPIO pathname exceeds bound".into());
        }
        let mut buffer = [0; NAME_MAX];
        reader.read_exact(&mut buffer[..namesize]).map_err(error)?;
        let bytes = &buffer[..namesize];
        if bytes[namesize - 1] != 0 || bytes[..namesize - 1].contains(&0) {
            return Err("Invalid CPIO pathname terminator".into());
        }
        let name = std::str::from_utf8(&bytes[..namesize - 1])
            .map_err(|_| "Invalid CPIO UTF8 pathname")?;
        zeros(&mut reader, padding(HEADER + namesize))?;
        if name == TRAILER {
            if fields != [0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 11, 0] {
                return Err("Invalid CPIO trailer fields".into());
            }
            let mut tail = [0; 8192];
            loop {
                let count = reader.read(&mut tail).map_err(error)?;
                if count == 0 {
                    break;
                }
                if tail[..count].iter().any(|byte| *byte != 0) {
                    return Err("Nonzero data after CPIO trailer".into());
                }
            }
            if files.is_empty() {
                return Err("Incomplete or empty CPIO archive".into());
            }
            return Ok(DecodedArchive {
                entries: files.len(),
                files,
                expanded_bytes: reader.count,
            });
        }
        if !name_valid(name) {
            return Err("CPIO source pathname exceeds original ustar domain".into());
        }
        let count = files
            .len()
            .checked_add(1)
            .ok_or("CPIO entry count overflow")?;
        if count > policy.entries {
            return Err("CPIO entry count exceeds limit".into());
        }
        let relative = name
            .strip_prefix(root)
            .and_then(|name| name.strip_prefix('/'))
            .filter(|path| path_safe(path))
            .ok_or("CPIO entry is outside the declared root or unsafe")?;
        if files.contains_key(relative) {
            return Err("Duplicate CPIO path".into());
        }
        // Inode/dev metadata is never restored or interpreted as link identity.
        if fields[1] != 0o100644
            || fields[4] != 1
            || fields[9] != 0
            || fields[10] != 0
            || fields[12] != 0
        {
            return Err(
                "CPIO source member must be a regular file with one link and mode0644".into(),
            );
        }
        let size = usize::try_from(fields[6]).map_err(|_| "Invalid CPIO file size")?;
        let maximum = policy.file.min(per_file(relative));
        if size > maximum {
            return Err(bound("Individual source entry", maximum));
        }
        let minimum = add(add(add(reader.count, size)?, padding(size))?, END_BYTES)?;
        if minimum > policy.expanded {
            return Err(bound("Expanded archive", policy.expanded));
        }
        let mut payload = Vec::new();
        payload
            .try_reserve_exact(size)
            .map_err(|_| "Cannot allocate bounded CPIO entry")?;
        payload.resize(size, 0);
        reader.read_exact(&mut payload).map_err(error)?;
        zeros(&mut reader, padding(size))?;
        files.insert(
            relative.into(),
            Entry {
                bytes: payload,
                mode: 0o644,
            },
        );
    }
}
fn header(writer: &mut impl Write, name: &str, fields: [u32; 13]) -> Result<(), String> {
    writer.write_all(b"070701").map_err(error)?;
    for value in fields {
        write!(writer, "{value:08x}").map_err(error)?;
    }
    writer
        .write_all(name.as_bytes())
        .and_then(|()| writer.write_all(&[0]))
        .map_err(error)?;
    writer
        .write_all(&[0; 3][..padding(HEADER + name.len() + 1)])
        .map_err(error)
}
fn raw_size(
    root: &str,
    files: &BTreeMap<String, Entry>,
    policy: ArchivePolicy,
    per_file: impl Fn(&str) -> usize,
) -> Result<usize, String> {
    if !root_valid(root) {
        return Err("Invalid CPIO archive root".into());
    }
    if files.is_empty() || files.len() > policy.entries {
        return Err("CPIO entry count exceeds limit".into());
    }
    let mut total = END_BYTES;
    for (path, entry) in files {
        if !path_safe(path) || entry.mode != 0o644 {
            return Err("Unsafe CPIO source entry or mode".into());
        }
        let namesize = add(add(add(root.len(), 1)?, path.len())?, 1)?;
        if namesize > NAME_MAX || !name_valid(&format!("{root}/{path}")) {
            return Err("CPIO source pathname exceeds original ustar domain".into());
        }
        let maximum = policy.file.min(per_file(path));
        if entry.bytes.len() > maximum {
            return Err(bound("Individual source entry", maximum));
        }
        let framing = add(HEADER, namesize)?;
        total = add(
            add(
                add(add(total, framing)?, padding(framing))?,
                entry.bytes.len(),
            )?,
            padding(entry.bytes.len()),
        )?;
        if total > policy.expanded {
            return Err(bound("Expanded archive", policy.expanded));
        }
    }
    Ok(total)
}
pub(crate) fn expanded_size(
    root: &str,
    files: &BTreeMap<String, Entry>,
    policy: ArchivePolicy,
    per_file: impl Fn(&str) -> usize,
) -> Result<usize, String> {
    let total = super::padded(raw_size(root, files, policy, per_file)?)?;
    if total > policy.expanded {
        return Err(bound("Expanded archive", policy.expanded));
    }
    Ok(total)
}
pub(crate) fn encode_root(
    root: &str,
    files: &BTreeMap<String, Entry>,
    policy: ArchivePolicy,
    per_file: impl Fn(&str) -> usize,
) -> Result<Vec<u8>, String> {
    let raw = raw_size(root, files, policy, &per_file)?;
    let expanded = super::padded(raw)?;
    if expanded > policy.expanded {
        return Err(bound("Expanded archive", policy.expanded));
    }
    let sink = CompressedWriter {
        bytes: Vec::new(),
        maximum: policy.compressed,
    };
    let mut writer = flate2::GzBuilder::new()
        .mtime(0)
        .write(sink, flate2::Compression::default());
    for (index, (path, entry)) in files.iter().enumerate() {
        let name = format!("{root}/{path}");
        let inode = u32::try_from(index.checked_add(1).ok_or("CPIO inode count overflow")?)
            .map_err(|_| "CPIO inode count overflow")?;
        let size = u32::try_from(entry.bytes.len()).map_err(|_| "CPIO file size overflow")?;
        let namesize = u32::try_from(name.len() + 1).map_err(|_| "CPIO name size overflow")?;
        header(
            &mut writer,
            &name,
            [inode, 0o100644, 0, 0, 1, 0, size, 0, 0, 0, 0, namesize, 0],
        )?;
        writer
            .write_all(&entry.bytes)
            .and_then(|()| writer.write_all(&[0; 3][..padding(entry.bytes.len())]))
            .map_err(error)?;
    }
    header(
        &mut writer,
        TRAILER,
        [0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 11, 0],
    )?;
    // Conventional512-byte final block, fully charged in the same bound.
    writer
        .write_all(&[0; 512][..expanded - raw])
        .map_err(error)?;
    Ok(writer.finish().map_err(error)?.bytes)
}
#[cfg(test)]
#[path = "package_cpio_tests.rs"]
mod tests;
