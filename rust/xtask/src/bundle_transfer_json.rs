//! Transfer metadata parser; every object rejects duplicate keys before use.
use serde::de::{self, Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Number, Value};
use std::fmt;

struct Unique(Value);
impl<'de> Deserialize<'de> for Unique {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Json;
        impl<'de> Visitor<'de> for Json {
            type Value = Unique;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("bounded unique-key JSON")
            }
            fn visit_bool<E: de::Error>(self, value: bool) -> Result<Unique, E> {
                Ok(Unique(Value::Bool(value)))
            }
            fn visit_i64<E: de::Error>(self, value: i64) -> Result<Unique, E> {
                Ok(Unique(Value::Number(value.into())))
            }
            fn visit_u64<E: de::Error>(self, value: u64) -> Result<Unique, E> {
                Ok(Unique(Value::Number(value.into())))
            }
            fn visit_f64<E: de::Error>(self, value: f64) -> Result<Unique, E> {
                Number::from_f64(value)
                    .map(|number| Unique(Value::Number(number)))
                    .ok_or_else(|| E::custom("Invalid transfer JSON number"))
            }
            fn visit_str<E: de::Error>(self, value: &str) -> Result<Unique, E> {
                Ok(Unique(Value::String(value.into())))
            }
            fn visit_string<E: de::Error>(self, value: String) -> Result<Unique, E> {
                Ok(Unique(Value::String(value)))
            }
            fn visit_unit<E: de::Error>(self) -> Result<Unique, E> {
                Ok(Unique(Value::Null))
            }
            fn visit_none<E: de::Error>(self) -> Result<Unique, E> {
                Ok(Unique(Value::Null))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut input: A) -> Result<Unique, A::Error> {
                let mut values = Vec::new();
                while let Some(value) = input.next_element::<Unique>()? {
                    values.push(value.0);
                }
                Ok(Unique(Value::Array(values)))
            }
            fn visit_map<A: MapAccess<'de>>(self, mut input: A) -> Result<Unique, A::Error> {
                let mut values = Map::new();
                while let Some((key, value)) = input.next_entry::<String, Unique>()? {
                    if values.insert(key, value.0).is_some() {
                        return Err(de::Error::custom("Duplicate transfer JSON key"));
                    }
                }
                Ok(Unique(Value::Object(values)))
            }
        }
        deserializer.deserialize_any(Json)
    }
}
pub fn parse(bytes: &[u8], maximum: usize) -> Result<Value, String> {
    if bytes.len() > maximum {
        return Err("Transfer JSON exceeds byte limit".into());
    }
    serde_json::from_slice::<Unique>(bytes)
        .map(|value| value.0)
        .map_err(|_| "Invalid or duplicate-key transfer JSON".into())
}
pub fn keys(value: &Value, expected: &[&str]) -> Result<(), String> {
    let map = value
        .as_object()
        .ok_or("Transfer record must be an object")?;
    if map.len() != expected.len() || expected.iter().any(|key| !map.contains_key(*key)) {
        return Err("Transfer record has missing or foreign fields".into());
    }
    Ok(())
}
pub fn digest(value: &Value) -> Result<&str, String> {
    value
        .as_str()
        .filter(|value| {
            value.len() == 64
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
        .ok_or_else(|| "Invalid transfer digest".into())
}

const SOURCE_MAGIC: &[u8; 8] = b"ARSM\0\0\0\x04";
const SOURCE_CODEC: &str = "indexed-native-paths-sha256-v1";
const SOURCE_HEADER_LIMIT: usize = 64 * 1024;

// Bound the complete reconstructed schema2 JSON before allocating its path
// vector encoding or files map. Each UTF8 byte needs at most six JSON bytes.
fn source_paths(paths: &[&str], header_bytes: usize, maximum: usize) -> Result<(), String> {
    let mut previous = "";
    let mut array_bytes = 2usize;
    for path in paths {
        if !super::super::safe(path) || path.len() > 256 || *path <= previous {
            return Err("Unsafe or unordered source inventory path".into());
        }
        array_bytes = path
            .len()
            .checked_mul(6)
            .and_then(|n| n.checked_add(3))
            .and_then(|n| n.checked_add(array_bytes))
            .ok_or("Decoded source inventory size overflow")?;
        previous = path;
    }
    // A map entry adds colon + quoted64hex instead of an array element;
    // the additional field in the nonempty normalized header costs nine bytes.
    if paths.is_empty()
        || paths
            .len()
            .checked_mul(67)
            .and_then(|n| n.checked_add(array_bytes))
            .and_then(|n| n.checked_add(header_bytes))
            .and_then(|n| n.checked_add(9))
            .is_none_or(|n| n > maximum)
    {
        return Err("Decoded source inventory exceeds byte limit".into());
    }
    Ok(())
}

fn header_identity(record: &Value, version: u64) -> Result<(), String> {
    if record["schema_version"].as_u64() != Some(version)
        || record["kind"] != "native_benchmark_public_source_bundle"
        || record["private_material_included"] != false
        || record["live_calls_performed"] != false
        || record["build_requires"].as_str().is_none()
    {
        return Err("Invalid binary source manifest identity".into());
    }
    digest(&record["fixture_sha256"])?;
    Ok(())
}

/// Paired-only generated metadata. Every raw source byte stays in its member;
/// payload names are independently committed and every full digest is retained.
pub fn encode_source(record: &Value, maximum: usize) -> Result<Vec<u8>, String> {
    keys(
        record,
        &[
            "schema_version",
            "kind",
            "fixture_sha256",
            "files",
            "build_requires",
            "private_material_included",
            "live_calls_performed",
        ],
    )?;
    header_identity(record, 2)?;
    let files = record["files"]
        .as_object()
        .ok_or("Missing source inventory")?;
    // Explicit order is independent of serde_json's map feature.
    let mut rows: Vec<_> = files.iter().collect();
    rows.sort_by(|a, b| a.0.cmp(b.0));
    let paths: Vec<_> = rows.iter().map(|(path, _)| path.as_str()).collect();
    let mut header = record.clone();
    header.as_object_mut().unwrap().remove("files");
    let normalized = serde_json::to_vec(&header).map_err(|_| "Cannot encode source header")?;
    source_paths(&paths, normalized.len(), maximum)?;
    header["schema_version"] = Value::from(4);
    header["files_encoding"] = Value::from(SOURCE_CODEC);
    header["paths_sha256"] = Value::from(super::super::sha(
        &serde_json::to_vec(&paths).map_err(|_| "Cannot encode source paths")?,
    ));
    let header = serde_json::to_vec(&header).map_err(|_| "Cannot encode source header")?;
    if header.len() > SOURCE_HEADER_LIMIT
        || rows
            .len()
            .checked_mul(32)
            .and_then(|n| n.checked_add(header.len()))
            .and_then(|n| n.checked_add(16))
            .is_none_or(|n| n > maximum)
    {
        return Err("Binary source header exceeds byte limit".into());
    }
    let mut bytes = SOURCE_MAGIC.to_vec();
    bytes.extend_from_slice(&(header.len() as u32).to_be_bytes());
    bytes.extend_from_slice(&header);
    bytes.extend_from_slice(
        &u32::try_from(files.len())
            .map_err(|_| "Source inventory too large")?
            .to_be_bytes(),
    );
    for (_, hash) in rows {
        let digest = digest(hash)?.as_bytes();
        for pair in digest.as_chunks::<2>().0 {
            let nibble = |b: u8| if b <= b'9' { b - b'0' } else { b - b'a' + 10 };
            bytes.push((nibble(pair[0]) << 4) | nibble(pair[1]));
        }
    }
    Ok(bytes)
}

fn take<'a>(bytes: &'a [u8], at: &mut usize, count: usize) -> Result<&'a [u8], String> {
    let end = at
        .checked_add(count)
        .ok_or("Binary source offset overflow")?;
    let found = bytes
        .get(*at..end)
        .ok_or("Truncated binary source manifest")?;
    *at = end;
    Ok(found)
}

/// Decode only after the enclosing schema5 carrier supplies its exact member
/// names (excluding only the active generated manifest). Count, path commitment
/// and independent digest rows are checked before the unchanged source validator.
pub fn decode_source(bytes: &[u8], maximum: usize, paths: &[&str]) -> Result<Value, String> {
    if bytes.len() > maximum {
        return Err("Binary source manifest exceeds byte limit".into());
    }
    let mut at = 0;
    if take(bytes, &mut at, 8)? != SOURCE_MAGIC {
        return Err("Unknown binary source manifest magic".into());
    }
    let length = u32::from_be_bytes(take(bytes, &mut at, 4)?.try_into().unwrap()) as usize;
    let mut header = parse(take(bytes, &mut at, length)?, SOURCE_HEADER_LIMIT)?;
    keys(
        &header,
        &[
            "schema_version",
            "kind",
            "fixture_sha256",
            "files_encoding",
            "paths_sha256",
            "build_requires",
            "private_material_included",
            "live_calls_performed",
        ],
    )?;
    header_identity(&header, 4)?;
    if header["files_encoding"] != SOURCE_CODEC {
        return Err("Unknown binary source manifest codec".into());
    }
    let paths_digest = digest(&header["paths_sha256"])?.to_owned();
    let count = u32::from_be_bytes(take(bytes, &mut at, 4)?.try_into().unwrap()) as usize;
    if count != paths.len() || count == 0 || count.checked_mul(32) != Some(bytes.len() - at) {
        return Err("Binary source inventory count differs".into());
    }
    header.as_object_mut().unwrap().remove("files_encoding");
    header.as_object_mut().unwrap().remove("paths_sha256");
    header["schema_version"] = Value::from(2);
    let normalized = serde_json::to_vec(&header).map_err(|_| "Cannot encode source header")?;
    source_paths(paths, normalized.len(), maximum)?;
    if super::super::sha(&serde_json::to_vec(paths).map_err(|_| "Cannot encode source paths")?)
        != paths_digest
    {
        return Err("Native source path commitment differs".into());
    }
    let mut files = Map::new();
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for path in paths {
        let mut hash = String::with_capacity(64);
        for byte in take(bytes, &mut at, 32)? {
            hash.push(HEX[(byte >> 4) as usize] as char);
            hash.push(HEX[(byte & 15) as usize] as char);
        }
        files.insert((*path).to_owned(), Value::from(hash));
    }
    header["files"] = Value::Object(files);
    Ok(header)
}
