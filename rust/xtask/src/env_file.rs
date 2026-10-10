//! Explicit source-tool env-file compatibility; never an implicit dotenv load.
//! Grammar follows Node 22's node_dotenv.cc (v22.14.0), including its final-line
//! handling. Existing process values win; later explicitly listed files win
//! over earlier files. No shell expansion, source command, or runtime hooks.
use serde_json::{Map, Value};
use std::path::Path;
use std::sync::OnceLock;

static EFFECTIVE: OnceLock<Value> = OnceLock::new();
pub fn parse(text: &str) -> Map<String, Value> {
    let lines = text.replace('\r', "");
    let mut content = lines.trim_matches(' ');
    let mut result = Map::new();
    while !content.is_empty() {
        if matches!(content.as_bytes().first(), Some(b'\n' | b'#'))
            && let Some(newline) = content.find('\n')
        {
            content = &content[newline + 1..];
            continue;
        }
        let Some(equal) = content.find('=') else {
            break;
        };
        let key = content[..equal].trim_matches(' ');
        content = content[equal + 1..].trim_matches(' ');
        if key.is_empty() {
            break;
        }
        let key = key.strip_prefix("export ").unwrap_or(key);
        if content.is_empty() {
            result.insert(key.into(), Value::String(String::new()));
            break;
        }
        if content.starts_with('"')
            && let Some(end) = content[1..].find('"').map(|n| n + 1)
        {
            result.insert(
                key.into(),
                Value::String(content[1..end].replace("\\n", "\n")),
            );
            if let Some(newline) = content[end + 1..].find('\n').map(|n| n + end + 1) {
                content = &content[newline..];
            }
            continue;
        }
        let first = content.as_bytes()[0];
        if matches!(first, b'\'' | b'"' | b'`') {
            if let Some(end) = content[1..].find(first as char).map(|n| n + 1) {
                result.insert(key.into(), Value::String(content[1..end].into()));
                if let Some(newline) = content[end + 1..].find('\n').map(|n| n + end + 1) {
                    content = &content[newline..];
                }
            } else if let Some(newline) = content.find('\n') {
                result.insert(key.into(), Value::String(content[..newline].into()));
                content = &content[newline..];
            }
        } else {
            let value = if let Some(newline) = content.find('\n') {
                let value = &content[..newline];
                content = &content[newline..];
                value.split('#').next().unwrap_or("")
            } else {
                content
            };
            result.insert(key.into(), Value::String(value.trim_matches(' ').into()));
        }
    }
    result
}
pub fn effective() -> Value {
    EFFECTIVE.get().cloned().unwrap_or_else(|| {
        Value::Object(
            std::env::vars_os()
                .map(|(k, v)| {
                    (
                        k.to_string_lossy().into_owned(),
                        Value::String(v.to_string_lossy().into_owned()),
                    )
                })
                .collect(),
        )
    })
}
pub fn load_args(args: &[String], cwd: &Path) -> Result<Vec<String>, String> {
    let mut files = Map::new();
    let mut remaining = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        let parsed = if let Some(path) = arg.strip_prefix("--env-file=") {
            Some((path, false))
        } else if let Some(path) = arg.strip_prefix("--env-file-if-exists=") {
            Some((path, true))
        } else if ["--env-file", "--env-file-if-exists"].contains(&arg.as_str()) {
            i += 1;
            Some((
                args.get(i)
                    .ok_or("Missing explicit env-file path")?
                    .as_str(),
                arg == "--env-file-if-exists",
            ))
        } else {
            None
        };
        if let Some((path, optional)) = parsed {
            let path = autorouter_core::config::resolve_path(cwd, Path::new(path));
            match crate::process::read_bounded(&path, 16 * 1024 * 1024) {
                Ok(bytes) => files.extend(parse(&String::from_utf8_lossy(&bytes))),
                Err(_) if optional && !path.exists() => {}
                Err(_) => return Err("Cannot read explicit env file".into()),
            }
        } else {
            remaining.extend_from_slice(&args[i..]);
            break;
        }
        i += 1;
    }
    files.extend(std::env::vars_os().map(|(k, v)| {
        (
            k.to_string_lossy().into_owned(),
            Value::String(v.to_string_lossy().into_owned()),
        )
    }));
    EFFECTIVE
        .set(Value::Object(files))
        .map_err(|_| "Tool environment already initialized")?;
    Ok(remaining)
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn explicit_file_grammar_does_not_execute_expansions_or_hooks() {
        assert_eq!(
            parse("export A=literal\nB=\"line\\nnext\"\nC='$(not-executed)'\nD=`multi\nline`\n"),
            json!({"A":"literal","B":"line\nnext","C":"$(not-executed)","D":"multi\nline"})
                .as_object()
                .unwrap()
                .clone()
        );
    }
    #[test]
    fn node22_final_line_quotes_and_space_rules_are_preserved() {
        assert_eq!(parse("A=x#comment\nB=x#comment")["A"], "x");
        assert_eq!(parse("A=x#comment\nB=x#comment")["B"], "x#comment");
        assert_eq!(parse("A=1\nA=2")["A"], "2");
        assert_eq!(parse("A=\t x \t\n")["A"], "\t x \t");
        assert_eq!(parse("A='unfinished\nB=value")["A"], "'unfinished");
    }
}
