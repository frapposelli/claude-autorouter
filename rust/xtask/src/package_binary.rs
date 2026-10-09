//! Inspect executable headers rather than trusting artifact filenames.
use serde_json::{Value, json};
fn read(bytes: &[u8], offset: usize, size: usize, little: bool) -> Result<u64, String> {
    let slice = bytes
        .get(
            offset
                ..offset
                    .checked_add(size)
                    .ok_or("Invalid executable offset")?,
        )
        .ok_or("Truncated executable header")?;
    let mut value = 0;
    for (i, byte) in slice.iter().enumerate() {
        let shift = if little { i } else { size - i - 1 };
        value |= (*byte as u64) << (8 * shift);
    }
    Ok(value)
}
pub fn inspect(target: &str, bytes: &[u8]) -> Result<Value, String> {
    if target.ends_with("apple-darwin") {
        if bytes.get(..4) != Some(&[0xcf, 0xfa, 0xed, 0xfe]) {
            return Err("Expected a little-endian 64-bit Mach-O executable".into());
        }
        let cpu = read(bytes, 4, 4, true)?;
        let expected = match target {
            "aarch64-apple-darwin" => 0x100000c,
            "x86_64-apple-darwin" => 0x1000007,
            _ => return Err("Unsupported native target".into()),
        };
        if cpu != expected || read(bytes, 12, 4, true)? != 2 {
            return Err("Mach-O architecture or executable type does not match target".into());
        }
        let commands = read(bytes, 16, 4, true)? as usize;
        if commands > 4096 {
            return Err("Invalid Mach-O load command count".into());
        }
        let mut offset = 32;
        let mut minimum = None;
        let mut libraries = Vec::new();
        for _ in 0..commands {
            let command = read(bytes, offset, 4, true)?;
            let size = read(bytes, offset + 4, 4, true)? as usize;
            if size < 8 || offset.checked_add(size).is_none_or(|n| n > bytes.len()) {
                return Err("Invalid Mach-O load command".into());
            }
            let version = match command {
                0x32 => Some(read(bytes, offset + 12, 4, true)?),
                0x24 => Some(read(bytes, offset + 8, 4, true)?),
                _ => None,
            };
            if let Some(version) = version {
                minimum = Some(format!(
                    "{}.{}.{}",
                    version >> 16,
                    (version >> 8) & 255,
                    version & 255
                ));
            }
            if matches!(command, 0xc | 0x80000018 | 0x8000001f | 0x20 | 0x80000023) {
                let name = read(bytes, offset + 8, 4, true)? as usize;
                if name >= size {
                    return Err("Invalid Mach-O library name".into());
                }
                let raw = &bytes[offset + name..offset + size];
                let end = raw.iter().position(|b| *b == 0).unwrap_or(raw.len());
                let library =
                    std::str::from_utf8(&raw[..end]).map_err(|_| "Invalid Mach-O library name")?;
                if !library.starts_with("/usr/lib/") && !library.starts_with("/System/Library/") {
                    return Err("Mach-O artifact depends on a non-system library".into());
                }
                libraries.push(library.to_owned());
            }
            offset += size;
        }
        return Ok(
            json!({"format":"Mach-O","target":target,"minimum_macos":minimum,"dynamic_libraries":libraries,"execution_qualification":"pending"}),
        );
    }
    if bytes.get(..4) != Some(b"\x7fELF") {
        return Err("Expected an ELF executable".into());
    }
    let class = *bytes.get(4).ok_or("Truncated ELF header")?;
    let little = match bytes.get(5) {
        Some(1) => true,
        Some(2) => false,
        _ => return Err("Invalid ELF endianness".into()),
    };
    let machine = read(bytes, 18, 2, little)?;
    let (expected, class_expected) = match target {
        "x86_64-unknown-linux-gnu" | "x86_64-unknown-linux-musl" => (62, 2),
        "aarch64-unknown-linux-gnu" | "aarch64-unknown-linux-musl" => (183, 2),
        "armv7-unknown-linux-gnueabihf" => (40, 1),
        "powerpc64le-unknown-linux-gnu" => (21, 2),
        "s390x-unknown-linux-gnu" => (22, 2),
        "loongarch64-unknown-linux-gnu" => (258, 2),
        "riscv64gc-unknown-linux-gnu" => (243, 2),
        _ => return Err("Unsupported native target".into()),
    };
    if machine != expected
        || class != class_expected
        || !matches!(read(bytes, 16, 2, little)?, 2 | 3)
    {
        return Err("ELF architecture or executable type does not match target".into());
    }
    let (phoff, entry, count) = if class == 2 {
        (
            read(bytes, 32, 8, little)?,
            read(bytes, 54, 2, little)?,
            read(bytes, 56, 2, little)?,
        )
    } else {
        (
            read(bytes, 28, 4, little)?,
            read(bytes, 42, 2, little)?,
            read(bytes, 44, 2, little)?,
        )
    };
    if count > 1024 || entry < if class == 2 { 56 } else { 32 } {
        return Err("Invalid ELF program headers".into());
    }
    let mut interpreter = None;
    let mut needed = 0;
    for index in 0..count {
        let offset = usize::try_from(
            phoff
                .checked_add(index.checked_mul(entry).ok_or("Invalid ELF offset")?)
                .ok_or("Invalid ELF offset")?,
        )
        .map_err(|_| "Invalid ELF offset")?;
        let kind = read(bytes, offset, 4, little)?;
        let (file_offset, file_size) = if class == 2 {
            (
                read(bytes, offset + 8, 8, little)?,
                read(bytes, offset + 32, 8, little)?,
            )
        } else {
            (
                read(bytes, offset + 4, 4, little)?,
                read(bytes, offset + 16, 4, little)?,
            )
        };
        if kind == 3 || kind == 2 {
            let start = usize::try_from(file_offset).map_err(|_| "Invalid ELF offset")?;
            let size = usize::try_from(file_size).map_err(|_| "Invalid ELF size")?;
            let raw = bytes
                .get(start..start.checked_add(size).ok_or("Invalid ELF size")?)
                .ok_or("Invalid ELF section")?;
            if kind == 3 {
                let end = raw
                    .iter()
                    .position(|b| *b == 0)
                    .ok_or("Invalid ELF interpreter")?;
                interpreter = Some(
                    std::str::from_utf8(&raw[..end])
                        .map_err(|_| "Invalid ELF interpreter")?
                        .to_owned(),
                );
            } else {
                let width = if class == 2 { 8 } else { 4 };
                for dynamic in raw.chunks_exact(width * 2) {
                    let tag = read(dynamic, 0, width, little)?;
                    if tag == 0 {
                        break;
                    }
                    if tag == 1 {
                        needed += 1;
                    }
                }
            }
        }
    }
    if target.ends_with("musl") && (interpreter.is_some() || needed > 0) {
        return Err(
            "Bundled musl artifacts must be static with no interpreter or DT_NEEDED".into(),
        );
    }
    Ok(
        json!({"format":"ELF","target":target,"interpreter":interpreter,"dynamic_needed_count":needed,"minimum_glibc":"unverified; inspect versioned symbols and run oldest image","execution_qualification":"pending"}),
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn binary_names_do_not_override_architecture_checks() {
        assert!(inspect("aarch64-apple-darwin", b"#!/bin/sh\n").is_err());
        let mut header = vec![0u8; 64];
        header[..4].copy_from_slice(b"\x7fELF");
        header[4] = 2;
        header[5] = 1;
        header[16] = 2;
        header[18] = 62;
        header[54] = 56;
        assert!(inspect("aarch64-unknown-linux-gnu", &header).is_err());
        assert_eq!(
            inspect("x86_64-unknown-linux-musl", &header).unwrap()["format"],
            "ELF"
        );
    }
}
