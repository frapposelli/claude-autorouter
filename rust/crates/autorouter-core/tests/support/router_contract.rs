use autorouter_core::config::{ClientProfile, RouterConfig, read_config};
use autorouter_core::js_json::JsDocument;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::Path;

pub const CASES: &str = include_str!("../../../../parity/cases/router-contracts.jsonl");
pub const CASES_SHA256: &str = "296f68fc81b50e6e24d277c43adb50d9613b4816a1b9d6a1e610299faa6126e3";
pub fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
pub fn text<'a>(value: &'a Value, field: &str) -> &'a str {
    value[field].as_str().unwrap_or("")
}
pub fn document(input: &Value, dictionary: &Value) -> JsDocument {
    JsDocument::parse(decode_body(input, dictionary).unwrap().as_bytes()).unwrap()
}
pub fn decode_body(input: &Value, dictionary: &Value) -> Result<String, &'static str> {
    let mut encoded = String::new();
    for index in input["body_chunks"].as_array().ok_or("Missing chunks")? {
        let index = index
            .as_u64()
            .and_then(|v| usize::try_from(v).ok())
            .ok_or("Invalid chunk index")?;
        let chunk = dictionary["chunks"]
            .get(index)
            .and_then(Value::as_str)
            .ok_or("Missing dictionary chunk")?;
        if encoded.len().saturating_add(chunk.len()) > 32 * 1024 * 1024 {
            return Err("Captured body exceeds original input limit");
        }
        encoded.push_str(chunk);
    }
    if digest(encoded.as_bytes()) != input["body_sha256"] {
        return Err("Captured body hash differs");
    }
    Ok(encoded)
}
pub fn configuration(snapshot: &Value) -> RouterConfig {
    let values = &snapshot["values"];
    let mut env = json!({"AUTOROUTER_EVALUATOR":values["evaluator"]});
    for (field, key) in [
        ("jevKey", "TYPESAFE_API_KEY"),
        ("anthropicKey", "ANTHROPIC_API_KEY"),
    ] {
        if let Some(value) = values.get(field) {
            env[key] = value.clone();
        }
    }
    let mut config = read_config(&env, false, Path::new("/synthetic")).unwrap();
    config.client_profile = match text(values, "clientProfile") {
        "compatible" => ClientProfile::Compatible,
        "native" => ClientProfile::Native,
        "auto" => ClientProfile::Auto,
        _ => panic!("Unexpected captured profile"),
    };
    config.models.haiku = text(&values["models"], "haiku").into();
    config.models.sonnet = text(&values["models"], "sonnet").into();
    config.models.opus = text(&values["models"], "opus").into();
    config.jev_timeout_ms = values["jevTimeoutMs"].as_u64().unwrap();
    config.turn_ttl_ms = values["turnTtlMs"].as_u64().unwrap();
    // Any additional configuration variation requires an explicit replay seam,
    // rather than silently substituting current defaults for frozen inputs.
    assert_eq!(serde_json::to_value(&config).unwrap(), *values);
    config
}
