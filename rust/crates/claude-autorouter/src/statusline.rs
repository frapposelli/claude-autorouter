//! Internal renderer: bounded local reads only, no configuration or network.
use autorouter_core::js_json::{JsDocument, JsNode, JsString};
use autorouter_core::statusline::render_status_line;
use autorouter_runtime::status_store::process_alive;
use serde_json::{Value, json};
use std::io::Read;
use std::time::{SystemTime, UNIX_EPOCH};
const LIMIT: usize = 1024 * 1024;

fn read_json(mut reader: impl Read) -> Option<JsDocument> {
    let mut bytes = Vec::new();
    let mut buffer = [0; 8192];
    let mut size = 0usize;
    loop {
        let Ok(count) = reader.read(&mut buffer) else {
            return None;
        };
        if count == 0 {
            break;
        }
        size = size.saturating_add(count);
        if size <= LIMIT {
            bytes.extend_from_slice(&buffer[..count]);
        } else {
            bytes.clear();
        }
    }
    if size > LIMIT {
        return None;
    }
    JsDocument::parse(&bytes).ok()
}
// Select by authoritative UTF-16 identity before creating a display projection.
// Otherwise distinct lone-surrogate session keys collapse to the same U+FFFD
// string, which can show another session's model or accounting.
fn observations(input: Option<JsDocument>, snapshot: Option<JsDocument>) -> (Value, Value) {
    let session = input.as_ref().and_then(|input| {
        if !matches!(input.node(input.root()), Some(JsNode::Object(_))) {
            return None;
        }
        match input.get(input.root(), "session_id") {
            None => Some(JsString::from_scalar("")),
            Some(node) => input.string(node).cloned(),
        }
    });
    let mut input = input
        .map(|input| input.to_serde_observation_lossy())
        .unwrap_or(Value::Null);
    if session.is_some() {
        input["session_id"] = json!("");
    }
    let snapshot = snapshot.map(|mut snapshot| {
        if matches!(snapshot.node(snapshot.root()), Some(JsNode::Object(_))) {
            for collection in ["sessions", "savings"] {
                let selected = session.as_ref().and_then(|session| {
                    let node = snapshot.get(snapshot.root(), collection)?;
                    let JsNode::Object(entries) = snapshot.node(node)? else {
                        return None;
                    };
                    entries.entries().iter().find_map(|(key, node)| {
                        (key == session).then(|| snapshot.stringify_node(*node))
                    })
                });
                let selected = selected
                    .map(|value| format!("{{\"\":{value}}}"))
                    .unwrap_or_else(|| "{}".into());
                snapshot
                    .set_root_field_json(collection, selected.as_bytes())
                    .expect("selected existing JSON value");
            }
        }
        snapshot.to_serde_observation_lossy()
    });
    (input, snapshot.unwrap_or(Value::Null))
}
pub fn run() {
    let input = read_json(std::io::stdin().lock());
    let snapshot = std::env::var_os("AUTOROUTER_STATUS_FILE").and_then(|path| {
        let metadata = std::fs::metadata(&path).ok()?;
        if !metadata.is_file() || metadata.len() > LIMIT as u64 {
            return None;
        }
        read_json(std::fs::File::open(path).ok()?.take(LIMIT as u64 + 1))
    });
    let (input, snapshot) = observations(input, snapshot);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64;
    let alive = snapshot["pid"]
        .as_f64()
        .filter(|pid| {
            pid.is_finite() && *pid > 0.0 && pid.fract() == 0.0 && *pid <= u32::MAX as f64
        })
        .map(|pid| pid as u32)
        .is_some_and(process_alive);
    let mut options = json!({"color":std::env::var_os("NO_COLOR").is_none() && std::env::var_os("TERM").as_deref()!=Some(std::ffi::OsStr::new("dumb")),"now":now,"alive":alive});
    if let Some(columns) = std::env::var_os("COLUMNS") {
        options["columns"] = json!(columns.to_string_lossy());
    }
    println!("{}", render_status_line(&input, &snapshot, &options));
}
