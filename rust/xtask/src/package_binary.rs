//! Inspect executable headers rather than trusting artifact filenames.
//! ELF addresses use PT_LOAD mappings, not section headers (which may be stripped).
//! Format sources: https://refspecs.linuxbase.org/elf/gabi4+/ch5.dynamic.html
//! https://refspecs.linuxfoundation.org/LSB_5.0.0/LSB-Core-generic/LSB-Core-generic/symversion.html
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
        let header = bytes.get(..32).ok_or("Truncated executable header")?;
        let cpu = read(header, 4, 4, true)?;
        let expected = match target {
            "aarch64-apple-darwin" => 0x100000c,
            "x86_64-apple-darwin" => 0x1000007,
            _ => return Err("Unsupported native target".into()),
        };
        if cpu != expected || read(header, 12, 4, true)? != 2 {
            return Err("Mach-O architecture or executable type does not match target".into());
        }
        let commands = read(header, 16, 4, true)? as usize;
        let command_bytes = read(header, 20, 4, true)? as usize;
        if commands > 4096 || commands > command_bytes / 8 {
            return Err("Invalid Mach-O load command count".into());
        }
        // mach_header_64.sizeofcmds bounds the entire region. Each command's
        // own slice bounds every interpreted field, including strings/tools.
        let command_end = 32usize
            .checked_add(command_bytes)
            .ok_or("Invalid Mach-O load command region")?;
        let load_commands = bytes
            .get(32..command_end)
            .ok_or("Invalid Mach-O load command region")?;
        let mut offset = 0usize;
        let mut minimum = None;
        let mut libraries = Vec::new();
        for _ in 0..commands {
            let remaining = load_commands
                .get(offset..)
                .ok_or("Invalid Mach-O load command")?;
            let command = read(remaining, 0, 4, true)?;
            let size = read(remaining, 4, 4, true)? as usize;
            if size < 8 || !size.is_multiple_of(8) {
                return Err("Invalid Mach-O load command".into());
            }
            let raw = remaining.get(..size).ok_or("Invalid Mach-O load command")?;
            let version = match command {
                0x32 => {
                    // build_version_command is 24 bytes, followed by exactly
                    // ntools build_tool_version records of eight bytes each.
                    if raw.len() < 24 {
                        return Err("Invalid Mach-O build version command".into());
                    }
                    if read(raw, 8, 4, true)? != 1 {
                        return Err("Mach-O artifact does not declare the macOS platform".into());
                    }
                    let tools = read(raw, 20, 4, true)? as usize;
                    let expected_size = tools
                        .checked_mul(8)
                        .and_then(|bytes| bytes.checked_add(24))
                        .ok_or("Invalid Mach-O build version tools")?;
                    if raw.len() != expected_size {
                        return Err("Invalid Mach-O build version tools".into());
                    }
                    Some(read(raw, 12, 4, true)?)
                }
                0x24 => {
                    if raw.len() != 16 {
                        return Err("Invalid Mach-O minimum version command".into());
                    }
                    Some(read(raw, 8, 4, true)?)
                }
                0x25 | 0x2f | 0x30 => {
                    return Err("Mach-O artifact declares a non-macOS minimum platform".into());
                }
                0x8000001c => return Err("Mach-O artifact declares a library search path".into()),
                _ => None,
            };
            if let Some(version) = version {
                if minimum.is_some() {
                    return Err("Duplicate Mach-O minimum platform command".into());
                }
                minimum = Some(format!(
                    "{}.{}.{}",
                    version >> 16,
                    (version >> 8) & 255,
                    version & 255
                ));
            }
            if matches!(command, 0xc | 0x80000018 | 0x8000001f | 0x20 | 0x80000023) {
                // dylib_command includes the full 24-byte dylib structure;
                // lc_str offsets are relative to this command, never the file.
                if raw.len() < 24 {
                    return Err("Invalid Mach-O library command".into());
                }
                let name = read(raw, 8, 4, true)? as usize;
                if name < 24 || name >= raw.len() {
                    return Err("Invalid Mach-O library name".into());
                }
                let name_bytes = &raw[name..];
                let end = name_bytes
                    .iter()
                    .position(|byte| *byte == 0)
                    .ok_or("Unterminated Mach-O library name")?;
                let library = std::str::from_utf8(&name_bytes[..end])
                    .map_err(|_| "Invalid Mach-O library name")?;
                // A trusted textual prefix must not hide parent traversal or
                // non-canonical components into an external dependency.
                if (!library.starts_with("/usr/lib/") && !library.starts_with("/System/Library/"))
                    || library
                        .split('/')
                        .skip(1)
                        .any(|part| matches!(part, "" | "." | ".."))
                    || library.chars().any(char::is_control)
                {
                    return Err("Mach-O artifact depends on a non-system library".into());
                }
                libraries.push(library.to_owned());
            }
            offset = offset
                .checked_add(size)
                .ok_or("Invalid Mach-O load command")?;
        }
        if offset != load_commands.len() {
            return Err("Mach-O load command count does not cover sizeofcmds".into());
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
    let (expected, class_expected, little_expected) = match target {
        "i686-unknown-linux-gnu" => (3, 1, true),
        "x86_64-unknown-linux-gnu" | "x86_64-unknown-linux-musl" => (62, 2, true),
        "aarch64-unknown-linux-gnu" | "aarch64-unknown-linux-musl" => (183, 2, true),
        "armv7-unknown-linux-gnueabihf" => (40, 1, true),
        "powerpc64le-unknown-linux-gnu" => (21, 2, true),
        "s390x-unknown-linux-gnu" => (22, 2, false),
        "loongarch64-unknown-linux-gnu" => (258, 2, true),
        "riscv64gc-unknown-linux-gnu" => (243, 2, true),
        _ => return Err("Unsupported native target".into()),
    };
    if machine != expected
        || class != class_expected
        || little != little_expected
        || !matches!(read(bytes, 16, 2, little)?, 2 | 3)
    {
        return Err("ELF architecture or executable type does not match target".into());
    }
    if target == "armv7-unknown-linux-gnueabihf" {
        // AAELF32 e_flags: EABI5, explicit hard-float PCS, no soft-float PCS.
        // This header admission does not establish CPU/ISA or OS qualification.
        let flags = read(bytes, 36, 4, little)?;
        if flags & 0xff00_0000 != 0x0500_0000 || flags & 0x600 != 0x400 {
            return Err(
                "ARM executable must declare EABI5 and hard-float without soft-float".into(),
            );
        }
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
    let headers = file_range(
        bytes,
        phoff,
        count
            .checked_mul(entry)
            .ok_or("Invalid ELF program headers")?,
    )?;
    let mut loads = Vec::new();
    let mut interpreter = None;
    let mut dynamic = None;
    for index in 0..count {
        let header = file_range(headers, index * entry, entry)?;
        let kind = read(header, 0, 4, little)?;
        let (file_offset, address, file_size, memory_size) = if class == 2 {
            (
                read(header, 8, 8, little)?,
                read(header, 16, 8, little)?,
                read(header, 32, 8, little)?,
                read(header, 40, 8, little)?,
            )
        } else {
            (
                read(header, 4, 4, little)?,
                read(header, 8, 4, little)?,
                read(header, 16, 4, little)?,
                read(header, 20, 4, little)?,
            )
        };
        if kind == 1 {
            if file_size > memory_size || address.checked_add(memory_size).is_none() {
                return Err("Invalid ELF load segment".into());
            }
            file_range(bytes, file_offset, file_size)?;
            loads.push(ElfLoad {
                file_offset,
                address,
                file_size,
            });
        } else if kind == 3 {
            if interpreter.is_some() {
                return Err("Duplicate ELF interpreter".into());
            }
            interpreter = Some(elf_string(file_range(bytes, file_offset, file_size)?, 0)?);
        } else if kind == 2 {
            if dynamic.is_some() {
                return Err("Duplicate ELF dynamic segment".into());
            }
            dynamic = Some(file_range(bytes, file_offset, file_size)?);
        }
    }
    let mut tags = std::collections::BTreeMap::new();
    let mut needed = Vec::new();
    if let Some(raw) = dynamic {
        let width = if class == 2 { 8 } else { 4 };
        if raw.len() % (width * 2) != 0 || raw.len() / (width * 2) > 65536 {
            return Err("Invalid ELF dynamic segment length".into());
        }
        let mut terminated = false;
        for item in raw.chunks_exact(width * 2) {
            let tag = read(item, 0, width, little)?;
            if tag == 0 {
                terminated = true;
                break;
            }
            let value = read(item, width, width, little)?;
            if tag == 1 {
                needed.push(value);
            } else if matches!(tag, 5 | 10 | 15 | 29 | 0x6ffffffe | 0x6fffffff)
                && tags.insert(tag, value).is_some()
            {
                return Err("Duplicate ELF dynamic metadata tag".into());
            }
        }
        if !terminated {
            return Err("Unterminated ELF dynamic segment".into());
        }
    }
    let strings = match (tags.get(&5), tags.get(&10)) {
        (Some(address), Some(size)) => virtual_range(bytes, &loads, *address, *size)?,
        (None, None)
            if needed.is_empty()
                && !tags.contains_key(&0x6ffffffe)
                && !tags.contains_key(&15)
                && !tags.contains_key(&29) =>
        {
            &[]
        }
        _ => return Err("Missing ELF dynamic string table".into()),
    };
    let libraries = needed
        .iter()
        .map(|offset| elf_string(strings, *offset))
        .collect::<Result<Vec<_>, _>>()?;
    if target.ends_with("musl") && (interpreter.is_some() || !libraries.is_empty()) {
        return Err(
            "Bundled musl artifacts must be static with no interpreter or DT_NEEDED".into(),
        );
    }
    let mut requirements = Vec::new();
    let mut glibc = std::collections::BTreeSet::new();
    let mut minimum: Option<(Vec<u64>, String)> = None;
    match (tags.get(&0x6ffffffe), tags.get(&0x6fffffff)) {
        (None, None) => {}
        (Some(start), Some(count)) if (1..=4096).contains(count) => {
            let mut address = *start;
            let mut total = 0usize;
            for index in 0..*count {
                let row = virtual_range(bytes, &loads, address, 16)?;
                if read(row, 0, 2, little)? != 1 {
                    return Err("Unsupported ELF version-needs format".into());
                }
                let count_aux = read(row, 2, 2, little)?;
                let library = elf_string(strings, read(row, 4, 4, little)?)?;
                let first_aux = read(row, 8, 4, little)?;
                let next = read(row, 12, 4, little)?;
                total = total
                    .checked_add(count_aux as usize)
                    .ok_or("Invalid ELF version-needs count")?;
                if count_aux == 0 || total > 16384 || first_aux < 16 {
                    return Err("Invalid ELF version-needs auxiliaries".into());
                }
                let mut aux_address = address
                    .checked_add(first_aux)
                    .ok_or("Invalid ELF version-needs address")?;
                let mut versions = Vec::new();
                for aux_index in 0..count_aux {
                    let aux = virtual_range(bytes, &loads, aux_address, 16)?;
                    let flags = read(aux, 4, 2, little)?;
                    let name = elf_string(strings, read(aux, 8, 4, little)?)?;
                    let next_aux = read(aux, 12, 4, little)?;
                    let weak = flags & 2 != 0;
                    if let Some(number) = name.strip_prefix("GLIBC_") {
                        glibc.insert(name.clone());
                        if !weak
                            && let Some(version) = glibc_number(&name)
                            && minimum.as_ref().is_none_or(|(old, _)| version > *old)
                        {
                            minimum = Some((version, number.to_owned()));
                        }
                    }
                    versions.push(json!({"name":name,"flags":flags,"weak":weak}));
                    aux_address = next_record(aux_address, next_aux, aux_index + 1 == count_aux)?;
                }
                requirements.push(json!({"library":library,"versions":versions}));
                address = next_record(address, next, index + 1 == *count)?;
            }
        }
        _ => return Err("Missing or invalid ELF version-needs metadata".into()),
    }
    let search_path = |tag| {
        tags.get(&tag)
            .map(|offset| elf_string(strings, *offset))
            .transpose()
    };
    Ok(
        json!({"format":"ELF","inspection_schema_version":2,"target":target,"interpreter":interpreter,
        "dynamic_needed_count":libraries.len(),"dynamic_libraries":libraries,
        "rpath":search_path(15)?,"runpath":search_path(29)?,"version_requirements":requirements,
        "glibc_version_requirements":glibc,"minimum_glibc":minimum.map(|(_,name)|name),
        "minimum_glibc_scope":"Highest numeric non-weak GLIBC version need in this executable only; additional ABI markers and transitive dependencies may impose stricter requirements. No oldest-runtime execution is implied.",
        "execution_qualification":"pending"}),
    )
}

struct ElfLoad {
    file_offset: u64,
    address: u64,
    file_size: u64,
}
fn file_range(bytes: &[u8], offset: u64, size: u64) -> Result<&[u8], String> {
    let start = usize::try_from(offset).map_err(|_| "Invalid ELF offset")?;
    let end = usize::try_from(offset.checked_add(size).ok_or("Invalid ELF range")?)
        .map_err(|_| "Invalid ELF range")?;
    bytes
        .get(start..end)
        .ok_or_else(|| "ELF range is outside the executable".into())
}
fn virtual_range<'a>(
    bytes: &'a [u8],
    loads: &[ElfLoad],
    address: u64,
    size: u64,
) -> Result<&'a [u8], String> {
    address
        .checked_add(size)
        .ok_or("Invalid ELF virtual range")?;
    let mut mapped = None;
    for load in loads {
        let Some(relative) = address.checked_sub(load.address) else {
            continue;
        };
        if relative > load.file_size || size > load.file_size - relative {
            continue;
        }
        let offset = load
            .file_offset
            .checked_add(relative)
            .ok_or("Invalid ELF mapped offset")?;
        if mapped.is_some_and(|old| old != offset) {
            return Err("Ambiguous ELF virtual mapping".into());
        }
        mapped = Some(offset);
    }
    file_range(
        bytes,
        mapped.ok_or("ELF virtual address is not file-backed")?,
        size,
    )
}
fn elf_string(strings: &[u8], offset: u64) -> Result<String, String> {
    let start = usize::try_from(offset).map_err(|_| "Invalid ELF string offset")?;
    let raw = strings.get(start..).ok_or("Invalid ELF string offset")?;
    let end = raw
        .iter()
        .take(4097)
        .position(|byte| *byte == 0)
        .ok_or("Unterminated or oversized ELF string")?;
    if end > 4096 {
        return Err("Oversized ELF string".into());
    }
    Ok(std::str::from_utf8(&raw[..end])
        .map_err(|_| "Non-UTF-8 ELF dynamic string")?
        .to_owned())
}
fn next_record(address: u64, next: u64, last: bool) -> Result<u64, String> {
    if last {
        if next != 0 {
            return Err("ELF version-needs chain exceeds declared count".into());
        }
        return Ok(address);
    }
    if next < 16 {
        return Err("Truncated or overlapping ELF version-needs chain".into());
    }
    address
        .checked_add(next)
        .ok_or_else(|| "Invalid ELF version-needs address".into())
}
fn glibc_number(name: &str) -> Option<Vec<u64>> {
    let text = name.strip_prefix("GLIBC_")?;
    let mut parts = text
        .split('.')
        .map(|part| {
            if part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()) {
                return None;
            }
            part.parse::<u64>().ok()
        })
        .collect::<Option<Vec<_>>>()?;
    if parts.len() < 2 || parts.len() > 4 {
        return None;
    }
    parts.resize(4, 0);
    Some(parts)
}

/// A symbol-version gate, deliberately separate from runtime qualification.
pub fn enforce_glibc_baseline(inspection: &Value, baseline: &str) -> Result<(), String> {
    let maximum = glibc_number(&format!("GLIBC_{baseline}"))
        .ok_or("GLIBC baseline must be a numeric dotted version")?;
    if inspection["format"] != "ELF" || inspection["inspection_schema_version"] != 2 {
        return Err("GLIBC baseline requires versioned ELF inspection".into());
    }
    let mut required = 0;
    for library in inspection["version_requirements"]
        .as_array()
        .ok_or("Missing ELF version requirements")?
    {
        for version in library["versions"]
            .as_array()
            .ok_or("Missing ELF version requirements")?
        {
            if version["weak"] == true {
                continue;
            }
            let name = version["name"]
                .as_str()
                .ok_or("Invalid ELF version requirement")?;
            if name.starts_with("GLIBC_") {
                required += 1;
                let number = glibc_number(name).ok_or(
                    "Nonnumeric GLIBC ABI requirement needs explicit compatibility review",
                )?;
                if number > maximum {
                    return Err(format!(
                        "Required {name} exceeds declared GLIBC {baseline} baseline"
                    ));
                }
            }
        }
    }
    if required == 0 {
        return Err("No non-weak GLIBC version requirement establishes a GNU baseline".into());
    }
    Ok(())
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
    fn put(bytes: &mut [u8], offset: usize, width: usize, value: u64, little: bool) {
        for index in 0..width {
            let shift = if little { index } else { width - index - 1 };
            bytes[offset + index] = (value >> (shift * 8)) as u8;
        }
    }
    fn macho_targets() -> [(&'static str, u64); 2] {
        [
            ("aarch64-apple-darwin", 0x100000c),
            ("x86_64-apple-darwin", 0x1000007),
        ]
    }
    fn macho_command(kind: u64, size: usize) -> Vec<u8> {
        let mut command = vec![0; size];
        put(&mut command, 0, 4, kind, true);
        put(&mut command, 4, 4, size as u64, true);
        command
    }
    fn macho_fixture(cpu: u64, commands: &[Vec<u8>]) -> Vec<u8> {
        let mut bytes = vec![0; 32];
        bytes[..4].copy_from_slice(&[0xcf, 0xfa, 0xed, 0xfe]);
        put(&mut bytes, 4, 4, cpu, true);
        put(&mut bytes, 12, 4, 2, true);
        put(&mut bytes, 16, 4, commands.len() as u64, true);
        put(
            &mut bytes,
            20,
            4,
            commands.iter().map(Vec::len).sum::<usize>() as u64,
            true,
        );
        for command in commands {
            bytes.extend_from_slice(command);
        }
        bytes
    }
    fn macho_minimum(version: u64) -> Vec<u8> {
        let mut command = macho_command(0x24, 16);
        put(&mut command, 8, 4, version, true);
        put(&mut command, 12, 4, 0x0f0203, true);
        command
    }
    fn macho_build(platform: u64, version: u64, tools: &[(u64, u64)]) -> Vec<u8> {
        let mut command = macho_command(0x32, 24 + tools.len() * 8);
        put(&mut command, 8, 4, platform, true);
        put(&mut command, 12, 4, version, true);
        put(&mut command, 16, 4, 0x0f0203, true);
        put(&mut command, 20, 4, tools.len() as u64, true);
        for (index, (tool, version)) in tools.iter().enumerate() {
            put(&mut command, 24 + index * 8, 4, *tool, true);
            put(&mut command, 28 + index * 8, 4, *version, true);
        }
        command
    }
    fn macho_library(kind: u64, name: &[u8], name_offset: usize) -> Vec<u8> {
        let size = (name_offset + name.len() + 1).div_ceil(8) * 8;
        let mut command = macho_command(kind, size);
        put(&mut command, 8, 4, name_offset as u64, true);
        command[name_offset..name_offset + name.len()].copy_from_slice(name);
        command
    }
    #[test]
    fn macho_header_only_fixtures_keep_the_existing_report_schema_for_both_cpus() {
        for (target, cpu) in macho_targets() {
            let mut bytes = macho_fixture(cpu, &[]);
            let expected = json!({"format":"Mach-O","target":target,"minimum_macos":null,"dynamic_libraries":[],"execution_qualification":"pending"});
            assert_eq!(inspect(target, &bytes).unwrap(), expected);
            // File payload outside a declared empty load-command region is not
            // interpreted as commands or invented minimum-OS evidence.
            bytes.extend_from_slice(&[0xff; 24]);
            assert_eq!(inspect(target, &bytes).unwrap(), expected);
            for length in 0..32 {
                assert!(
                    inspect(target, &bytes[..length]).is_err(),
                    "{target} {length}"
                );
            }
            for (offset, value) in [(4, cpu ^ 1), (12, 6), (16, 1), (20, 8)] {
                let mut wrong = bytes.clone();
                put(&mut wrong, offset, 4, value, true);
                assert!(inspect(target, &wrong).is_err(), "{target} {offset}");
            }
        }
    }
    #[test]
    fn macho_macos_versions_and_all_dependency_kinds_preserve_report_values() {
        let kinds = [0xc, 0x80000018, 0x8000001f, 0x20, 0x80000023];
        let names = [
            "/usr/lib/libSystem.B.dylib",
            "/System/Library/Frameworks/Security.framework/Versions/A/Security",
            "/usr/lib/libiconv.2.dylib",
            "/usr/lib/libobjc.A.dylib",
            "/System/Library/Frameworks/CoreFoundation.framework/CoreFoundation",
        ];
        for (target, cpu) in macho_targets() {
            for (version, text) in [
                (0x0a0f07, "10.15.7"),
                (0x0b0000, "11.0.0"),
                (0x0f0203, "15.2.3"),
            ] {
                for modern in [false, true] {
                    let mut commands = vec![if modern {
                        macho_build(1, version, &[(3, 0x040000)])
                    } else {
                        macho_minimum(version)
                    }];
                    for (index, (kind, name)) in kinds.iter().zip(names).enumerate() {
                        commands.push(macho_library(
                            *kind,
                            name.as_bytes(),
                            if index % 2 == 0 { 24 } else { 32 },
                        ));
                    }
                    let result = inspect(target, &macho_fixture(cpu, &commands)).unwrap();
                    assert_eq!(
                        result,
                        json!({"format":"Mach-O","target":target,"minimum_macos":text,"dynamic_libraries":names,"execution_qualification":"pending"})
                    );
                }
            }
        }
    }
    #[test]
    fn macho_command_region_and_alignment_do_not_borrow_following_file_bytes() {
        for (target, cpu) in macho_targets() {
            let mut good = macho_fixture(cpu, &[macho_minimum(0x0b0000), macho_command(0x7777, 8)]);
            good.extend_from_slice(&macho_command(0x7777, 8));
            assert_eq!(inspect(target, &good).unwrap()["minimum_macos"], "11.0.0");
            for (offset, value) in [
                (16, 0),
                (16, 1),
                (16, 3),
                (16, 4097),
                (20, 0),
                (20, 8),
                (20, 16),
                (20, 23),
                (20, 32),
                (20, u32::MAX as u64),
                (36, 0),
                (36, 4),
                (36, 7),
                (36, 8),
                (36, 12),
                (36, 15),
                (36, 24),
                (36, 0xfffffff8),
            ] {
                let mut bytes = good.clone();
                put(&mut bytes, offset, 4, value, true);
                assert!(
                    inspect(target, &bytes).is_err(),
                    "{target} {offset} {value}"
                );
            }
            let no_tail = macho_fixture(cpu, &[macho_minimum(0x0b0000)]);
            for length in 32..no_tail.len() {
                assert!(inspect(target, &no_tail[..length]).is_err());
            }
        }
    }
    #[test]
    fn macho_known_command_fields_cannot_overlap_a_following_command() {
        for (target, cpu) in macho_targets() {
            for (kind, sizes) in [
                (0x24, &[8, 24][..]),
                (0x32, &[8, 16][..]),
                (0xc, &[8, 16][..]),
                (0x80000018, &[8, 16][..]),
                (0x8000001f, &[8, 16][..]),
                (0x20, &[8, 16][..]),
                (0x80000023, &[8, 16][..]),
            ] {
                for &size in sizes {
                    let mut short = macho_command(kind, size);
                    if kind == 0x32 && size >= 16 {
                        put(&mut short, 8, 4, 1, true);
                        put(&mut short, 12, 4, 0x0b0000, true);
                    }
                    let bytes = macho_fixture(cpu, &[short, macho_command(0x7777, 32)]);
                    assert!(inspect(target, &bytes).is_err(), "{target} {kind:x} {size}");
                }
            }
        }
    }
    #[test]
    fn macho_build_version_tools_exactly_fit_the_current_command() {
        for (target, cpu) in macho_targets() {
            for tools in [
                &[][..],
                &[(1, 0x010203)][..],
                &[(1, 0x010203), (3, 0x040506)][..],
            ] {
                let command = macho_build(1, 0x0b0000, tools);
                assert_eq!(
                    inspect(target, &macho_fixture(cpu, std::slice::from_ref(&command))).unwrap()["minimum_macos"],
                    "11.0.0"
                );
                for count in [tools.len() as u64 + 1, u32::MAX as u64] {
                    let mut invalid = command.clone();
                    put(&mut invalid, 20, 4, count, true);
                    // The trailing bytes are a separate valid command. They
                    // cannot complete an under-sized build-tool table.
                    assert!(
                        inspect(
                            target,
                            &macho_fixture(cpu, &[invalid, macho_command(0x7777, 8)])
                        )
                        .is_err()
                    );
                }
                if !tools.is_empty() {
                    let mut extra = command;
                    put(&mut extra, 20, 4, 0, true);
                    assert!(inspect(target, &macho_fixture(cpu, &[extra])).is_err());
                }
            }
        }
    }
    #[test]
    fn macho_rejects_duplicate_or_conflicting_minimum_platform_commands() {
        for (target, cpu) in macho_targets() {
            let legacy = macho_minimum(0x0b0000);
            let modern = macho_build(1, 0x0b0000, &[]);
            for commands in [
                vec![legacy.clone(), legacy.clone()],
                vec![modern.clone(), modern.clone()],
                vec![legacy.clone(), modern.clone()],
                vec![modern.clone(), legacy.clone()],
                vec![legacy, macho_minimum(0x0c0000)],
                vec![modern, macho_build(1, 0x0c0000, &[])],
            ] {
                assert_eq!(
                    inspect(target, &macho_fixture(cpu, &commands)).unwrap_err(),
                    "Duplicate Mach-O minimum platform command"
                );
            }
        }
    }
    #[test]
    fn macho_rejects_other_build_platforms_and_legacy_minimum_platforms() {
        for (target, cpu) in macho_targets() {
            for platform in [0, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 15, u32::MAX as u64] {
                let command = macho_build(platform, 0x0b0000, &[]);
                assert!(
                    inspect(target, &macho_fixture(cpu, std::slice::from_ref(&command))).is_err()
                );
                assert!(
                    inspect(
                        target,
                        &macho_fixture(cpu, &[macho_minimum(0x0b0000), command])
                    )
                    .is_err()
                );
            }
            for kind in [0x25, 0x2f, 0x30] {
                let mut command = macho_minimum(0x0b0000);
                put(&mut command, 0, 4, kind, true);
                assert!(inspect(target, &macho_fixture(cpu, &[command])).is_err());
            }
        }
    }
    #[test]
    fn macho_rejects_rpaths_even_when_the_path_and_dependencies_are_system_paths() {
        for (target, cpu) in macho_targets() {
            let mut rpath = macho_command(0x8000001c, 24);
            put(&mut rpath, 8, 4, 12, true);
            rpath[12..21].copy_from_slice(b"/usr/lib/");
            let bytes = macho_fixture(
                cpu,
                &[
                    macho_minimum(0x0b0000),
                    macho_library(0xc, b"/usr/lib/libSystem.B.dylib", 24),
                    rpath,
                ],
            );
            assert_eq!(
                inspect(target, &bytes).unwrap_err(),
                "Mach-O artifact declares a library search path"
            );
        }
    }
    #[test]
    fn macho_library_names_stay_inside_their_complete_command() {
        for (target, cpu) in macho_targets() {
            let good = macho_library(0xc, b"/usr/lib/libSystem.B.dylib", 24);
            for offset in [
                0,
                8,
                12,
                16,
                23,
                good.len(),
                good.len() + 8,
                u32::MAX as usize,
            ] {
                let mut command = good.clone();
                put(&mut command, 8, 4, offset as u64, true);
                assert!(
                    inspect(target, &macho_fixture(cpu, &[command])).is_err(),
                    "{target} {offset}"
                );
            }
            // A path embedded inside timestamp/current/compatibility fields is
            // a valid-looking prefix but not a legal lc_str name offset.
            let mut fields = macho_command(0xc, 24);
            put(&mut fields, 8, 4, 12, true);
            fields[12..23].copy_from_slice(b"/usr/lib/x\0");
            assert!(inspect(target, &macho_fixture(cpu, &[fields])).is_err());
            let mut no_nul = good;
            no_nul[24..].fill(b'x');
            no_nul[24..33].copy_from_slice(b"/usr/lib/");
            let mut bytes = macho_fixture(cpu, &[no_nul]);
            bytes.push(0); // A terminator outside cmdsize must not be borrowed.
            assert_eq!(
                inspect(target, &bytes).unwrap_err(),
                "Unterminated Mach-O library name"
            );
            for name in [&b""[..], &b"/usr/lib/\xff"[..]] {
                assert!(
                    inspect(target, &macho_fixture(cpu, &[macho_library(0xc, name, 24)])).is_err()
                );
            }
        }
    }
    #[test]
    fn macho_dependencies_must_be_lexically_absolute_system_paths() {
        for (target, cpu) in macho_targets() {
            for name in [
                "libSystem.B.dylib",
                "@rpath/libSystem.B.dylib",
                "@loader_path/libSystem.B.dylib",
                "/opt/homebrew/lib/libssl.dylib",
                "/usr/library/libSystem.B.dylib",
                "/usr/lib/../../tmp/libexternal.dylib",
                "/System/Library/../../../tmp/libexternal.dylib",
                "/usr/lib/./libSystem.B.dylib",
                "/usr/lib//libSystem.B.dylib",
                "/usr/lib/",
                "/usr/lib/lib\nSystem.B.dylib",
            ] {
                assert_eq!(
                    inspect(
                        target,
                        &macho_fixture(cpu, &[macho_library(0xc, name.as_bytes(), 24)])
                    )
                    .unwrap_err(),
                    "Mach-O artifact depends on a non-system library",
                    "{target} {name:?}"
                );
            }
        }
    }
    #[test]
    fn elf_target_admission_checks_byte_order_as_well_as_machine_and_class() {
        for (target, machine, class, expected_little) in [
            ("i686-unknown-linux-gnu", 3, 1, true),
            ("x86_64-unknown-linux-gnu", 62, 2, true),
            ("x86_64-unknown-linux-musl", 62, 2, true),
            ("aarch64-unknown-linux-gnu", 183, 2, true),
            ("aarch64-unknown-linux-musl", 183, 2, true),
            ("armv7-unknown-linux-gnueabihf", 40, 1, true),
            ("powerpc64le-unknown-linux-gnu", 21, 2, true),
            ("s390x-unknown-linux-gnu", 22, 2, false),
            ("loongarch64-unknown-linux-gnu", 258, 2, true),
            ("riscv64gc-unknown-linux-gnu", 243, 2, true),
        ] {
            for little in [expected_little, !expected_little] {
                // Encode the complete header in its declared byte order; merely
                // flipping EI_DATA would also corrupt the machine/type checks.
                let mut bytes = vec![0; 64];
                bytes[..4].copy_from_slice(b"\x7fELF");
                bytes[4] = class;
                bytes[5] = if little { 1 } else { 2 };
                bytes[6] = 1;
                put(&mut bytes, 16, 2, 2, little);
                put(&mut bytes, 18, 2, machine, little);
                if machine == 40 {
                    put(&mut bytes, 36, 4, 0x0500_0400, little);
                }
                let (entry_offset, entry_size) = if class == 2 { (54, 56) } else { (42, 32) };
                put(&mut bytes, entry_offset, 2, entry_size, little);
                let result = inspect(target, &bytes);
                if little == expected_little {
                    let result = result.unwrap_or_else(|error| panic!("{target}: {error}"));
                    assert_eq!(result["target"], target);
                    assert_eq!(result["execution_qualification"], "pending");
                } else {
                    assert_eq!(
                        result.unwrap_err(),
                        "ELF architecture or executable type does not match target",
                        "{target} little={little}",
                    );
                }
            }
        }
    }
    // No section headers: all dynamic pointers differ from file offsets.
    fn elf_fixture(class: u8, little: bool) -> (String, Vec<u8>) {
        let mut bytes = vec![0; 0x800];
        bytes[..4].copy_from_slice(b"\x7fELF");
        bytes[4] = class;
        bytes[5] = if little { 1 } else { 2 };
        bytes[6] = 1;
        put(&mut bytes, 16, 2, 3, little);
        let (target, machine) = match (class, little) {
            (1, true) => ("armv7-unknown-linux-gnueabihf", 40),
            (2, true) => ("x86_64-unknown-linux-gnu", 62),
            (2, false) => ("s390x-unknown-linux-gnu", 22),
            _ => panic!("No supported target has this ELF class and byte order"),
        };
        put(&mut bytes, 18, 2, machine, little);
        if machine == 40 {
            put(&mut bytes, 36, 4, 0x0500_0400, little);
        }
        let width = if class == 2 { 8 } else { 4 };
        let entry = if class == 2 { 56 } else { 32 };
        let header = if class == 2 { 64 } else { 52 };
        for (offset, size, value) in if class == 2 {
            vec![(32, 8, header), (54, 2, entry), (56, 2, 3)]
        } else {
            vec![(28, 4, header), (42, 2, entry), (44, 2, 3)]
        } {
            put(&mut bytes, offset, size, value, little);
        }
        for (index, (kind, file_offset, size)) in
            [(1, 0x200, 0x600), (3, 0x210, 12), (2, 0x280, 8 * width * 2)]
                .into_iter()
                .enumerate()
        {
            let position = header as usize + index * entry as usize;
            put(&mut bytes, position, 4, kind, little);
            if class == 2 {
                for (offset, value) in [
                    (8, file_offset),
                    (16, 0x8000 + file_offset - 0x200),
                    (32, size),
                    (40, size + 0x100),
                ] {
                    put(&mut bytes, position + offset, 8, value as u64, little);
                }
            } else {
                for (offset, value) in [
                    (4, file_offset),
                    (8, 0x8000 + file_offset - 0x200),
                    (16, size),
                    (20, size + 0x100),
                ] {
                    put(&mut bytes, position + offset, 4, value as u64, little);
                }
            }
        }
        bytes[0x210..0x21c].copy_from_slice(b"/ld-test.so\0");
        let mut strings = vec![0];
        let names = [
            "libc.so.6",
            "libgcc_s.so.1",
            "GLIBC_2.9",
            "GLIBC_2.34",
            "GLIBC_2.40",
            "GLIBC_ABI_DT_RELR",
            "GCC_3.0",
            "$ORIGIN/lib",
        ];
        let mut offsets = Vec::new();
        for name in names {
            offsets.push(strings.len() as u64);
            strings.extend_from_slice(name.as_bytes());
            strings.push(0);
        }
        bytes[0x480..0x480 + strings.len()].copy_from_slice(&strings);
        for (index, (tag, value)) in [
            (1, offsets[0]),
            (5, 0x8280),
            (1, offsets[1]),
            (10, strings.len() as u64),
            (0x6ffffffe, 0x8400),
            (0x6fffffff, 2),
            (29, offsets[7]),
            (0, 0),
        ]
        .into_iter()
        .enumerate()
        {
            put(&mut bytes, 0x280 + index * width * 2, width, tag, little);
            put(
                &mut bytes,
                0x280 + index * width * 2 + width,
                width,
                value,
                little,
            );
        }
        for (position, count, library, next) in
            [(0x600, 4, offsets[0], 80), (0x650, 1, offsets[1], 0)]
        {
            put(&mut bytes, position, 2, 1, little);
            put(&mut bytes, position + 2, 2, count, little);
            put(&mut bytes, position + 4, 4, library, little);
            put(&mut bytes, position + 8, 4, 16, little);
            put(&mut bytes, position + 12, 4, next, little);
        }
        for (index, name) in offsets[2..6].iter().enumerate() {
            let position = 0x610 + index * 16;
            put(
                &mut bytes,
                position + 4,
                2,
                if index == 2 { 2 } else { 0 },
                little,
            );
            put(&mut bytes, position + 6, 2, index as u64 + 2, little);
            put(&mut bytes, position + 8, 4, *name, little);
            put(
                &mut bytes,
                position + 12,
                4,
                if index == 3 { 0 } else { 16 },
                little,
            );
        }
        put(&mut bytes, 0x668, 4, offsets[6], little);
        (target.to_owned(), bytes)
    }
    #[test]
    fn elf_dependencies_and_version_needs_use_load_mappings_for_both_classes_and_byte_orders() {
        // The only supported big-endian target is ELF64 s390x.
        for (class, byte_orders) in [(1, &[true][..]), (2, &[false, true][..])] {
            for &little in byte_orders {
                let (target, bytes) = elf_fixture(class, little);
                let result = inspect(&target, &bytes).unwrap();
                assert_eq!(
                    result["dynamic_libraries"],
                    json!(["libc.so.6", "libgcc_s.so.1"])
                );
                assert_eq!(result["dynamic_needed_count"], 2);
                assert_eq!(result["interpreter"], "/ld-test.so");
                assert_eq!(result["minimum_glibc"], "2.34");
                assert_eq!(result["runpath"], "$ORIGIN/lib");
                assert_eq!(result["rpath"], Value::Null);
                assert_eq!(
                    result["glibc_version_requirements"],
                    json!(["GLIBC_2.34", "GLIBC_2.40", "GLIBC_2.9", "GLIBC_ABI_DT_RELR"])
                );
                assert_eq!(
                    result["version_requirements"],
                    json!([
                        {"library":"libc.so.6","versions":[{"name":"GLIBC_2.9","flags":0,"weak":false},{"name":"GLIBC_2.34","flags":0,"weak":false},{"name":"GLIBC_2.40","flags":2,"weak":true},{"name":"GLIBC_ABI_DT_RELR","flags":0,"weak":false}]},
                        {"library":"libgcc_s.so.1","versions":[{"name":"GCC_3.0","flags":0,"weak":false}]}
                    ])
                );
                assert_eq!(result["execution_qualification"], "pending");
            }
        }
    }
    #[test]
    fn armv7_hard_float_admission_requires_eabi5_and_explicit_compatible_float_flags() {
        let (target, good) = elf_fixture(1, true);
        for executable_type in [2, 3] {
            let mut valid = good.clone();
            put(&mut valid, 16, 2, executable_type, true);
            let result = inspect(&target, &valid).unwrap();
            assert_eq!(result["execution_qualification"], "pending");
            assert_eq!(result["interpreter"], "/ld-test.so");
            for flags in [
                0,           // Missing ABI and float declarations.
                0x0000_0400, // Hard-float with unspecified EABI version.
                0x0400_0400, // Legacy EABI4.
                0x0600_0400, // Unknown future EABI version.
                0x0500_0000, // EABI5 with implicit base/soft-float PCS.
                0x0500_0200, // Explicit soft-float.
                0x0500_0600, // Conflicting hard- and soft-float declarations.
            ] {
                let mut bytes = valid.clone();
                put(&mut bytes, 36, 4, flags, true);
                assert_eq!(
                    inspect(&target, &bytes).unwrap_err(),
                    "ARM executable must declare EABI5 and hard-float without soft-float",
                    "type={executable_type} flags={flags:x}"
                );
            }
        }
        for length in 36..40 {
            assert!(inspect(&target, &good[..length]).is_err());
        }
    }
    #[test]
    fn i686_uses_elf32_dynamic_tables_and_never_admits_a_different_machine_or_class() {
        let (_, mut bytes) = elf_fixture(1, true);
        put(&mut bytes, 18, 2, 3, true); // EM_386, ELFCLASS32, ELFDATA2LSB.
        put(&mut bytes, 36, 4, 0, true); // Clear ARM-specific e_flags.
        let result = inspect("i686-unknown-linux-gnu", &bytes).unwrap();
        assert_eq!(result["target"], "i686-unknown-linux-gnu");
        assert_eq!(result["interpreter"], "/ld-test.so");
        assert_eq!(
            result["dynamic_libraries"],
            json!(["libc.so.6", "libgcc_s.so.1"])
        );
        assert_eq!(result["minimum_glibc"], "2.34");
        assert_eq!(result["execution_qualification"], "pending");
        assert!(inspect("x86_64-unknown-linux-gnu", &bytes).is_err());
        for (offset, width, value) in [(4, 1, 2), (18, 2, 40), (16, 2, 1)] {
            let mut wrong = bytes.clone();
            put(&mut wrong, offset, width, value, true);
            assert!(inspect("i686-unknown-linux-gnu", &wrong).is_err());
        }
    }
    #[test]
    fn malformed_dynamic_pointers_strings_and_version_chains_fail_closed() {
        for (class, byte_orders) in [(1, &[true][..]), (2, &[false, true][..])] {
            for &little in byte_orders {
                let (target, good) = elf_fixture(class, little);
                let width = if class == 2 { 8 } else { 4 };
                // Every parser-specific mutation must fail, rather than hiding a
                // dependency or accepting a fabricated low GLIBC requirement.
                let cases = [
                    (0x280 + width, width, 0xffff_ffff), // DT_NEEDED outside STRSZ
                    (0x280 + 3 * width, width, 0x8600),  // STRTAB into zero-filled BSS
                    (0x280 + 7 * width, width, 0xffff_ffff), // huge STRSZ
                    (0x280 + 9 * width, width, 0xffff_ffff), // unmapped VERNEED
                    (0x280 + 11 * width, width, 0),      // missing version count
                    (0x280 + 11 * width, width, 4097),   // version count cap
                    (0x600, 2, 2),                       // unsupported vn_version
                    (0x602, 2, 0),                       // no auxiliaries
                    (0x602, 2, 16385),                   // auxiliary work cap
                    (0x608, 4, 8),                       // overlaps parent record
                    (0x60c, 4, 0),                       // early end of Verneed chain
                    (0x618, 4, 0xffff_ffff),             // vna_name outside STRSZ
                    (0x61c, 4, 0),                       // early end of Vernaux chain
                    (0x61c, 4, 8),                       // overlapping Vernaux records
                    (0x64c, 4, 16),                      // extra Vernaux past count
                    (0x65c, 4, 32),                      // extra Verneed past count
                ];
                for (offset, size, value) in cases {
                    let mut bytes = good.clone();
                    put(&mut bytes, offset, size, value, little);
                    assert!(
                        inspect(&target, &bytes).is_err(),
                        "class{class} little{little} offset{offset:x} value{value:x}"
                    );
                }
                let mut missing_null = good.clone();
                put(&mut missing_null, 0x280 + 14 * width, width, 0x100, little);
                assert!(inspect(&target, &missing_null).is_err());
                let mut bad_name = good.clone();
                bad_name[0x481] = 255;
                assert!(inspect(&target, &bad_name).is_err());
                let mut unterminated = good.clone();
                // Shrink STRSZ to exclude libc.so.6's NUL.
                put(&mut unterminated, 0x280 + 7 * width, width, 10, little);
                assert!(inspect(&target, &unterminated).is_err());
                let mut duplicate = good.clone();
                put(&mut duplicate, 0x280 + 12 * width, width, 5, little);
                assert!(inspect(&target, &duplicate).is_err());
                for length in [0, 4, 19, 51, 63, 0x290, 0x500, 0x660] {
                    assert!(inspect(&target, &good[..length]).is_err());
                }
            }
        }
    }
    #[test]
    fn oversized_offsets_and_ambiguous_mappings_never_wrap_or_panic() {
        let (target, good) = elf_fixture(2, true);
        for (offset, value) in [
            (32, u64::MAX),
            (64 + 8, u64::MAX),
            (64 + 16, u64::MAX),
            (64 + 32, u64::MAX),
            (64 + 40, u64::MAX),
            (0x298, u64::MAX),
            (0x2c8, u64::MAX),
        ] {
            let mut bytes = good.clone();
            put(&mut bytes, offset, 8, value, true);
            assert!(inspect(&target, &bytes).is_err(), "{offset:x}");
        }
        let mut overlap = good;
        // Convert PT_INTERP to another LOAD mapping same addresses differently.
        put(&mut overlap, 64 + 56, 4, 1, true);
        put(&mut overlap, 64 + 56 + 8, 8, 0x300, true);
        put(&mut overlap, 64 + 56 + 16, 8, 0x8000, true);
        put(&mut overlap, 64 + 56 + 32, 8, 0x500, true);
        put(&mut overlap, 64 + 56 + 40, 8, 0x500, true);
        assert!(
            inspect(&target, &overlap)
                .unwrap_err()
                .contains("Ambiguous")
        );
    }
    #[test]
    fn static_artifacts_have_no_inferred_glibc_floor_and_dynamic_musl_is_rejected() {
        let mut bytes = vec![0; 64];
        bytes[..4].copy_from_slice(b"\x7fELF");
        bytes[4] = 2;
        bytes[5] = 1;
        bytes[16] = 2;
        bytes[18] = 62;
        bytes[54] = 56;
        let result = inspect("x86_64-unknown-linux-musl", &bytes).unwrap();
        assert_eq!(result["minimum_glibc"], Value::Null);
        assert_eq!(result["version_requirements"], json!([]));
        assert_eq!(result["dynamic_libraries"], json!([]));
        assert_eq!(result["execution_qualification"], "pending");
        assert!(inspect("x86_64-unknown-linux-musl", &elf_fixture(2, true).1).is_err());
    }

    #[test]
    fn glibc_baseline_gate_rejects_new_strong_versions_and_unknown_abi_markers() {
        let version = |name: &str, weak: bool| json!({"format":"ELF","inspection_schema_version":2,"version_requirements":[{"library":"libc.so.6","versions":[{"name":"GLIBC_2.17","weak":false},{"name":name,"weak":weak}]}]});
        assert!(enforce_glibc_baseline(&version("GLIBC_2.28", false), "2.28").is_ok());
        assert!(enforce_glibc_baseline(&version("GLIBC_2.39", true), "2.28").is_ok());
        assert!(enforce_glibc_baseline(&version("GLIBC_2.9", false), "2.28").is_ok());
        assert!(
            enforce_glibc_baseline(&version("GLIBC_2.34", false), "2.28")
                .unwrap_err()
                .contains("GLIBC_2.34")
        );
        assert!(
            enforce_glibc_baseline(&version("GLIBC_ABI_DT_RELR", false), "2.28")
                .unwrap_err()
                .contains("explicit compatibility review")
        );
        assert!(
            enforce_glibc_baseline(
                &json!({"format":"ELF","inspection_schema_version":2,"version_requirements":[]}),
                "2.28"
            )
            .is_err()
        );
        assert!(enforce_glibc_baseline(&version("GLIBC_2.28", false), "latest").is_err());
    }
}
