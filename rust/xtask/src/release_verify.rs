//! Read-only publication-state verification. Absence and outage are distinct.
use crate::release::{
    self, Artifact, PACKAGE, REGISTRY, REPOSITORY, ReleaseError, compare_versions, error, mismatch,
    parse_version, unavailable,
};
use crate::release_install::{Installer, NativeInstaller};
use autorouter_core::js_json::JsDocument;
use autorouter_runtime::bounded_json::DecodedResponseStream;
use autorouter_runtime::http_client::{HttpTransport, NativeHttpClient};
use bytes::Bytes;
use http_body_util::Full;
use hyper::Request;
use serde_json::{Value, json};
use std::cmp::Ordering;
use std::path::Path;
use std::time::Duration;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

async fn get<T: HttpTransport>(
    transport: &T,
    url: &str,
    json_body: bool,
    timeout: Duration,
    cancel: &CancellationToken,
    authorization: Option<&str>,
) -> Result<Option<Vec<u8>>, ReleaseError> {
    let token = cancel.child_token();
    let _guard = token.clone().drop_guard();
    let action = async {
        let mut request = Request::get(url)
            .header(
                "accept",
                if json_body {
                    "application/json"
                } else {
                    "application/octet-stream"
                },
            )
            .header("cache-control", "no-cache, no-store")
            .header("pragma", "no-cache")
            .header(
                "user-agent",
                concat!(
                    "claude-autorouter-release-verifier/",
                    env!("CARGO_PKG_VERSION")
                ),
            );
        if let Some(token) = authorization {
            request = request.header("authorization", format!("Bearer {token}"));
        }
        let request = request
            .body(Full::new(Bytes::new()))
            .map_err(|_| unavailable("network_error"))?;
        let response = transport
            .request(request)
            .await
            .map_err(|_| unavailable("network_error"))?;
        if response.status() == 404 {
            return Ok(None);
        }
        if !response.status().is_success() {
            let mut err = unavailable("http_error");
            err.detail["http_status"] = json!(response.status().as_u16());
            return Err(err);
        }
        let maximum = if json_body {
            16 * 1024 * 1024
        } else {
            crate::package::archive::MAX_COMPRESSED
        };
        if response
            .headers()
            .get("content-length")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<f64>().ok())
            .is_some_and(|n| n > maximum as f64)
        {
            return Err(unavailable("oversized_response"));
        }
        let (parts, body) = response.into_parts();
        let mut reader = DecodedResponseStream::new(body, &parts.headers, &token)
            .await
            .map_err(|_| unavailable("network_error"))?;
        let mut bytes = Vec::new();
        let mut chunk = [0; 8192];
        loop {
            let count = reader
                .read(&mut chunk)
                .await
                .map_err(|_| unavailable("network_error"))?;
            if count == 0 {
                break;
            }
            if bytes.len().saturating_add(count) > maximum {
                return Err(unavailable("oversized_response"));
            }
            bytes.extend_from_slice(&chunk[..count]);
        }
        Ok(Some(bytes))
    };
    tokio::select! {biased;_=cancel.cancelled()=>Err(error("cancelled","cancelled")),_=tokio::time::sleep(timeout)=>Err(unavailable("timeout")),result=action=>result}
}
fn decode(bytes: Option<Vec<u8>>) -> Result<Option<Value>, ReleaseError> {
    bytes
        .map(|b| {
            JsDocument::parse(&b)
                .map(|doc| doc.to_serde_observation_lossy())
                .map_err(|_| unavailable("invalid_json"))
        })
        .transpose()
}
struct Evidence {
    metadata: Option<Value>,
    tags: Option<Value>,
    source: &'static str,
}
async fn registry_evidence<T: HttpTransport>(
    transport: &T,
    artifact: &Artifact,
    timeout: Duration,
    cancel: &CancellationToken,
) -> Result<Evidence, ReleaseError> {
    let nonce = crate::tool_process::token().map_err(|_| unavailable("nonce_unavailable"))?;
    let exact_url = format!(
        "{REGISTRY}/{PACKAGE}/{}?autorouter_verify={nonce}",
        artifact.version
    );
    let package_url = format!("{REGISTRY}/{PACKAGE}?autorouter_verify={nonce}");
    let (exact, packument) = tokio::join!(
        get(transport, &exact_url, true, timeout, cancel, None),
        get(transport, &package_url, true, timeout, cancel, None)
    );
    let exact = decode(exact?)?;
    let packument = decode(packument?)?;
    if let Some(package) = &packument
        && (!package.is_object()
            || package["name"] != PACKAGE
            || !package["dist-tags"].is_object()
            || !package["versions"].is_object())
    {
        return Err(unavailable("invalid_package_metadata"));
    }
    let packaged = packument
        .as_ref()
        .and_then(|p| p["versions"].get(&artifact.version))
        .cloned();
    for metadata in [&exact, &packaged].into_iter().flatten() {
        if !metadata.is_object()
            || metadata["name"] != PACKAGE
            || metadata["version"] != artifact.version
            || !metadata["dist"].is_object()
        {
            return Err(mismatch("metadata_identity"));
        }
        if metadata["dist"]["integrity"] != artifact.integrity {
            return Err(mismatch("integrity"));
        }
        if metadata["dist"]["tarball"] != format!("{REGISTRY}/{PACKAGE}/-/{}", artifact.filename) {
            return Err(mismatch("tarball_location"));
        }
    }
    let source = if exact.is_some() {
        "version"
    } else if packaged.is_some() {
        "packument"
    } else {
        "unavailable"
    };
    Ok(Evidence {
        metadata: exact.or(packaged),
        tags: packument.and_then(|p| p.get("dist-tags").cloned()),
        source,
    })
}
fn merge(mut base: Value, extra: Value) -> Value {
    base.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    base
}
pub async fn preflight<T: HttpTransport>(
    transport: &T,
    artifact: &Artifact,
    allow_publish: bool,
    cancel: &CancellationToken,
) -> Result<Value, ReleaseError> {
    let evidence = registry_evidence(transport, artifact, Duration::from_secs(10), cancel).await?;
    let tags = evidence
        .tags
        .ok_or_else(|| unavailable("package_metadata_unavailable"))?;
    let current = tags.get(&artifact.dist_tag);
    if let Some(value) = current {
        let value = value
            .as_str()
            .ok_or_else(|| unavailable("invalid_dist_tag"))?;
        let parsed = parse_version(value).map_err(|_| unavailable("invalid_dist_tag"))?;
        if artifact.dist_tag == "latest" && !parsed.prerelease.is_empty() {
            return Err(unavailable("latest_is_not_stable"));
        }
    }
    let mut report = merge(
        artifact.report_base(),
        json!({"phase":"preflight","current_dist_tag":current.cloned().unwrap_or(Value::Null)}),
    );
    if evidence.metadata.is_some() {
        return Ok(merge(
            report,
            json!({"state":"submitted","publish":false,"reason":"identical_version_exists","metadata_source":evidence.source}),
        ));
    }
    if !allow_publish {
        return Ok(merge(
            report,
            json!({"state":"validating_unavailable","publish":false,"reason":"retry_submission_unknown"}),
        ));
    }
    if artifact.dist_tag == "latest"
        && (current.is_none()
            || compare_versions(&artifact.version, current.unwrap().as_str().unwrap())?
                != Ordering::Greater)
    {
        let mut e = error("version_order", "version_order");
        e.detail = json!({"current_latest":current.cloned().unwrap_or(Value::Null)});
        return Err(e);
    }
    if !artifact.native {
        return Err(error(
            "historical_archive",
            "native_release_required_for_publication",
        ));
    }
    report["state"] = json!("preflight_ready");
    report["publish"] = json!(true);
    report["reason"] = json!("version_not_published");
    Ok(report)
}
fn remaining(
    start: Instant,
    budget: Duration,
    maximum: Duration,
) -> Result<Duration, ReleaseError> {
    budget
        .checked_sub(start.elapsed())
        .filter(|d| !d.is_zero())
        .map(|d| d.min(maximum))
        .ok_or_else(|| unavailable("verification_deadline"))
}
pub async fn verify<T: HttpTransport, I: Installer>(
    transport: &T,
    installer: &I,
    artifact: &Artifact,
    timeout_ms: u64,
    cancel: &CancellationToken,
    on_state: &mut impl FnMut(&Value) -> Result<(), ReleaseError>,
) -> Result<Value, ReleaseError> {
    if !(1..=3600000).contains(&timeout_ms) {
        return Err(error("invalid_timeout", "invalid_timeout"));
    }
    let budget = Duration::from_millis(timeout_ms);
    let start = Instant::now();
    let mut events = Vec::<Value>::new();
    let mut attempts = 0;
    let mut delay = 1000;
    let mut last = artifact.report_base();
    while start.elapsed() < budget {
        if cancel.is_cancelled() {
            return Err(error("cancelled", "cancelled"));
        }
        attempts += 1;
        let attempt=async{let evidence=registry_evidence(transport,artifact,remaining(start,budget,Duration::from_secs(10))?,cancel).await?;
let metadata=evidence.metadata.ok_or_else(||unavailable("version_not_available"))?;
let nonce=crate::tool_process::token().map_err(|_|unavailable("nonce_unavailable"))?;
let url=format!("{}?autorouter_verify={nonce}",metadata["dist"]["tarball"].as_str().unwrap());
let tarball=get(transport,&url,false,remaining(start,budget,Duration::from_secs(10))?,cancel,None).await?.ok_or_else(||unavailable("tarball_not_available"))?;
if tarball.len()!=artifact.bytes||crate::evaluation::digest(&tarball)!=artifact.sha256||release::integrity(&tarball)!=artifact.integrity{return Err(mismatch("tarball_bytes"));}let current=evidence.tags.as_ref().and_then(|t|t.get(&artifact.dist_tag)).and_then(Value::as_str).ok_or_else(||unavailable("dist_tag_unavailable"))?;
let parsed=parse_version(current).map_err(|_|unavailable("invalid_dist_tag"))?;
if artifact.dist_tag=="latest"&&!parsed.prerelease.is_empty(){return Err(unavailable("invalid_dist_tag"));}let order=compare_versions(current,&artifact.version).map_err(|_|unavailable("invalid_dist_tag"))?;
if order==Ordering::Less{return Err(unavailable("dist_tag_not_updated"));}let installed=installer.install(artifact,remaining(start,budget,Duration::from_secs(120))?,cancel).await?;Ok(json!({"state":"verified","reason":"archive_and_public_install_verified","current_dist_tag":current,"dist_tag_status":if order==Ordering::Greater{"superseded_by_newer_version"}else{"current"},"install":installed}))}.await;
        let row = match attempt {
            Ok(row) => row,
            Err(err) => {
                if cancel.is_cancelled() || err.code == "cancelled" {
                    return Err(error("cancelled", "cancelled"));
                }
                let mut row = json!({"state":if err.code=="registry_unavailable"{"validating_unavailable"}else{"failed"},"reason":err.detail.get("reason").cloned().unwrap_or(json!(err.code))});
                if let Some(status) = err.detail.get("http_status") {
                    row["http_status"] = status.clone();
                }
                row
            }
        };
        last = merge(artifact.report_base(), row.clone());
        last["attempts"] = json!(attempts);
        last["elapsed_ms"] = json!(start.elapsed().as_millis());
        events.push(
            json!({"state":row["state"],"reason":row["reason"],"elapsed_ms":last["elapsed_ms"]}),
        );
        if events.len() > 32 {
            events.remove(0);
        }
        on_state(&last)?;
        if row["state"] == "verified" || row["state"] == "failed" {
            last["events"] = json!(events);
            return Ok(last);
        }
        let Some(left) = budget.checked_sub(start.elapsed()).filter(|d| !d.is_zero()) else {
            break;
        };
        tokio::select! {biased;_=cancel.cancelled()=>return Err(error("cancelled","cancelled")),_=tokio::time::sleep(left.min(Duration::from_millis(delay)))=>{}}
        delay = (delay * 2).min(15000);
    }
    last["state"] = json!("validating_unavailable");
    last["pending"] = json!(true);
    last["elapsed_ms"] = json!(start.elapsed().as_millis());
    last["events"] = json!(events);
    last["message"] = json!(
        "Verification remains pending. Keep the original archive and rerun verification; do not submit a duplicate release."
    );
    Ok(last)
}
fn safe_id(value: &str) -> Option<u64> {
    if value.is_empty() || value.starts_with('0') || !value.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    value
        .parse::<u64>()
        .ok()
        .filter(|n| *n <= 9_007_199_254_740_991)
}
pub fn validate_artifact_source(
    run: &Value,
    artifact: &Value,
    run_id: &str,
    artifact_id: &str,
    commit: &str,
    tag: &str,
) -> Result<Value, ReleaseError> {
    let run_number =
        safe_id(run_id).ok_or_else(|| error("invalid_release_input", "artifact_identity"))?;
    let artifact_number =
        safe_id(artifact_id).ok_or_else(|| error("invalid_release_input", "artifact_identity"))?;
    if commit.len() != 40
        || !commit
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(error("invalid_release_input", "artifact_identity"));
    }
    parse_version(
        tag.strip_prefix('v')
            .ok_or_else(|| error("invalid_version", "release_tag_required"))?,
    )?;
    let name = artifact["name"].as_str().unwrap_or("");
    let prefix = format!("npm-package-{run_id}-");
    let name_valid = name.strip_prefix(&prefix).is_some_and(|s| {
        !s.is_empty() && !s.starts_with('0') && s.bytes().all(|b| b.is_ascii_digit())
    });
    if run["id"].as_f64() != Some(run_number as f64)
        || run["event"] != "push"
        || run["path"] != ".github/workflows/publish.yml"
        || run["head_sha"] != commit
        || run["head_branch"] != tag
        || run["head_repository"]["full_name"] != REPOSITORY
        || artifact["id"].as_f64() != Some(artifact_number as f64)
        || artifact["expired"] != false
        || !name_valid
        || artifact["workflow_run"]["id"].as_f64() != Some(run_number as f64)
        || artifact["workflow_run"]["head_sha"] != commit
    {
        return Err(error("invalid_release_input", "artifact_source_mismatch"));
    }
    Ok(
        json!({"run_id":run_id,"artifact_id":artifact_id,"notes_artifact_name":name.replacen("npm-package-","release-notes-",1)}),
    )
}
async fn artifact_source<T: HttpTransport>(
    transport: &T,
    root: &Path,
    tag: &str,
    args: &Value,
    cancel: &CancellationToken,
) -> Result<Value, ReleaseError> {
    let run_id = args["--run-id"]
        .as_str()
        .ok_or_else(|| error("invalid_release_input", "artifact_identity"))?;
    let artifact_id = args["--artifact-id"]
        .as_str()
        .ok_or_else(|| error("invalid_release_input", "artifact_identity"))?;
    if run_id.len() > 16
        || artifact_id.len() > 16
        || safe_id(run_id).is_none()
        || safe_id(artifact_id).is_none()
    {
        return Err(error("invalid_release_input", "artifact_identity"));
    }
    parse_version(
        tag.strip_prefix('v')
            .ok_or_else(|| error("invalid_version", "release_tag_required"))?,
    )?;
    let commit = release::git(
        root,
        &[
            "rev-parse",
            "--verify",
            &format!("refs/tags/{tag}^{{commit}}"),
        ],
    )
    .map_err(|_| error("invalid_release_input", "tag_commit"))?;
    release::git(
        root,
        &[
            "merge-base",
            "--is-ancestor",
            &commit,
            "refs/remotes/origin/main",
        ],
    )
    .map_err(|_| error("invalid_release_input", "main_ancestry"))?;
    let env = crate::env_file::effective();
    let token = env["GH_TOKEN"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| error("invalid_release_input", "github_token_required"))?;
    let base = format!("https://api.github.com/repos/{REPOSITORY}/actions");
    let run_url = format!("{base}/runs/{run_id}");
    let artifact_url = format!("{base}/artifacts/{artifact_id}");
    let (run, artifact) = tokio::join!(
        get(
            transport,
            &run_url,
            true,
            Duration::from_secs(10),
            cancel,
            Some(token)
        ),
        get(
            transport,
            &artifact_url,
            true,
            Duration::from_secs(10),
            cancel,
            Some(token)
        )
    );
    validate_artifact_source(
        &decode(run?)?.unwrap_or(json!({})),
        &decode(artifact?)?.unwrap_or(json!({})),
        run_id,
        artifact_id,
        &commit,
        tag,
    )
}
fn notes(root: &Path, artifact: &Artifact, path: &Path) -> Result<(), String> {
    let head = release::git(root, &["rev-parse", "--verify", "HEAD^{commit}"])?;
    let previous = release::git(
        root,
        &["describe", "--tags", "--abbrev=0", "--match", "v*", "HEAD^"],
    )
    .ok();
    let range = previous.as_ref().map(|p| format!("{p}..HEAD"));
    let mut args = vec!["log", "--format=- %s"];
    if let Some(range) = &range {
        args.push(range);
    } else {
        args.extend(["-n", "20"]);
    }
    args.push("--");
    let commits = release::git(root, &args)?;
    let since = previous
        .map(|s| format!(" since {s}"))
        .unwrap_or_else(|| " (recent commits)".into());
    let text = format!(
        "# {PACKAGE} {}\n\nSource commit: {head}\n\nCanonical archive: {}\nSHA256: {}\n\n## Changes{since}\n\n{commits}\n\n## Registry verification\n\nSubmission and public availability are separate states. See the retained verification report.\n\nAfter verification: `npm install -g {PACKAGE}@{}`\n",
        artifact.version, artifact.filename, artifact.sha256, artifact.version
    );
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|_| "Cannot create notes directory")?;
    }
    std::fs::write(path, text).map_err(|_| "Cannot write release notes".into())
}
fn persist(args: &Value, report: &Value) -> Result<(), ReleaseError> {
    if let Some(path) = args["--report"].as_str() {
        crate::evaluation::write_report(Path::new(path), report, false)
            .map_err(|_| error("report_failed", "report_failed"))?;
    }
    Ok(())
}
pub fn run(args: &[String], root: &Path) -> Result<bool, String> {
    if args == ["--help"] {
        println!(
            "Usage: cargo xtask release-verify preflight|submitted|verify|notes|inspect-legacy TAG --archive PATH [--report PATH] [--timeout-ms N]\n       cargo xtask release-verify artifact-source TAG --run-id N --artifact-id N\nAll registry/GitHub operations are read-only. Preflight never publishes and requires a qualified native release. inspect-legacy reads historical package evidence explicitly; verify-historical performs opt-in read-only registry/install verification. Pending availability must never trigger duplicate publication."
        );
        return Ok(true);
    }
    let mode = args.first().map(String::as_str).unwrap_or("");
    if ![
        "preflight",
        "submitted",
        "verify",
        "notes",
        "artifact-source",
        "inspect-legacy",
        "verify-historical",
    ]
    .contains(&mode)
        || args.len() < 2
        || !(args.len() - 2).is_multiple_of(2)
    {
        return Err("Use release-verify --help for supported options".into());
    }
    let mut options = json!({});
    for pair in args[2..].chunks(2) {
        if ![
            "--archive",
            "--report",
            "--timeout-ms",
            "--run-id",
            "--artifact-id",
        ]
        .contains(&pair[0].as_str())
            || options.get(&pair[0]).is_some()
        {
            return Err("Unknown or duplicate release verification option".into());
        }
        options[&pair[0]] = json!(pair[1]);
    }
    let tag = &args[1];
    crate::evaluation::runtime()?.block_on(execute(mode, tag, options, root))
}
async fn execute(mode: &str, tag: &str, options: Value, root: &Path) -> Result<bool, String> {
    let signals = crate::tool_process::Signals::new();
    let mut artifact = None;
    let action:Result<Option<Value>,ReleaseError>=async{if mode=="artifact-source"{let client=NativeHttpClient::new().map_err(|_|unavailable("network_error"))?;
let result=artifact_source(&client,root,tag,&options,&signals.token).await?;release::github_output(&result).map_err(|_|error("report_failed","github_output"))?;
println!("Canonical release artifact provenance verified.");return Ok(None);}let path=options["--archive"].as_str().ok_or_else(||error("invalid_release_input","canonical_archive_required"))?;artifact=Some(release::inspect_artifact(Path::new(path),tag,matches!(mode,"inspect-legacy"|"verify-historical"))?);
let target=artifact.as_ref().unwrap();
if mode=="notes"{let path=options["--report"].as_str().ok_or_else(||error("invalid_release_input","release_notes_path_required"))?;notes(root,target,Path::new(path)).map_err(|_|error("report_failed","release_notes"))?;return Ok(None);}
if mode=="inspect-legacy"{return Ok(Some(merge(target.report_base(),json!({"state":"inspected","format":if target.native{"native"}else{"historical_javascript"},"publish":false}))));}
if mode=="submitted"{return Ok(Some(merge(target.report_base(),json!({"state":"submitted","timestamp":autorouter_runtime::server_events::timestamp()}))));}let client=NativeHttpClient::new().map_err(|_|unavailable("network_error"))?;
if mode=="preflight"{let env=crate::env_file::effective();return preflight(&client,target,env.get("GITHUB_RUN_ATTEMPT").is_none_or(|v|*v=="1"),&signals.token).await.map(Some);}let timeout=if let Some(value)=options["--timeout-ms"].as_str(){if value.is_empty()||!value.bytes().all(|b|b.is_ascii_digit()){return Err(error("invalid_timeout","invalid_timeout"));}value.parse::<u64>().map_err(|_|error("invalid_timeout","invalid_timeout"))?}else{900000};verify(&client,&NativeInstaller,target,timeout,&signals.token,&mut |state|{persist(&options,state)?;
println!("{}: {} (attempt {})",state["state"].as_str().unwrap(),state["reason"].as_str().unwrap(),state["attempts"]);Ok(())}).await.map(Some)}.await;
    let report = match action {
        Ok(None) => return Ok(true),
        Ok(Some(report)) => report,
        Err(err) => {
            let mut row = merge(
                artifact
                    .as_ref()
                    .map(Artifact::report_base)
                    .unwrap_or(json!({"schema_version":1})),
                json!({"state":"failed","phase":mode,"reason":err.code,"error_code":err.code}),
            );
            let mut detail = err.detail.as_object().unwrap().clone();
            // Preserve the historical corrupt-sidecar report while retaining the
            // reader's typed checksum detail and all other admission reasons.
            if err.code == "invalid_archive"
                && detail.get("reason") == Some(&json!("checksum_mismatch"))
            {
                detail.remove("reason");
            }
            row.as_object_mut().unwrap().extend(detail);
            row
        }
    };
    persist(&options, &report).map_err(|e| e.to_string())?;
    let mut output = json!({"state":report["state"]});
    if mode == "preflight" && report["state"] != "failed" {
        output["publish"] = json!(report["publish"].to_string());
        output["can_verify"] = json!("true");
    }
    release::github_output(&output)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&report).map_err(|_| "Cannot serialize release report")?
    );
    Ok(report["state"] != "failed")
}

#[cfg(test)]
mod tests {
    use super::*;
    use autorouter_runtime::http_client::HttpError;
    use hyper::Response;
    use std::sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering as AtomicOrdering},
    };
    const BYTES: &[u8] = b"synthetic canonical archive, already tested";
    fn artifact() -> Artifact {
        Artifact {
            version: "0.4.0".into(),
            tag: "v0.4.0".into(),
            dist_tag: "latest".into(),
            archive: "synthetic".into(),
            filename: "claude-autorouter-0.4.0.tgz".into(),
            sha256: crate::evaluation::digest(BYTES),
            integrity: release::integrity(BYTES),
            bytes: BYTES.len(),
            native: true,
        }
    }
    #[derive(Default)]
    struct Options {
        absent: bool,
        exact404: bool,
        status: Option<u16>,
        latest: Option<String>,
        wrong_bytes: bool,
        wrong_integrity: bool,
        delayed: bool,
    }
    struct Registry {
        options: Options,
        reads: AtomicUsize,
        tarballs: AtomicUsize,
        paths: Mutex<Vec<String>>,
    }
    impl Registry {
        fn new(options: Options) -> Self {
            Self {
                options,
                reads: AtomicUsize::new(0),
                tarballs: AtomicUsize::new(0),
                paths: Mutex::new(Vec::new()),
            }
        }
    }
    impl HttpTransport for Registry {
        type ResponseBody = Full<Bytes>;
        async fn request(
            &self,
            request: Request<Full<Bytes>>,
        ) -> Result<Response<Self::ResponseBody>, HttpError> {
            assert_eq!(request.method(), "GET");
            assert_eq!(request.uri().host(), Some("registry.npmjs.org"));
            assert!(
                request
                    .uri()
                    .query()
                    .unwrap()
                    .starts_with("autorouter_verify=")
            );
            assert!(
                request.headers()["cache-control"]
                    .to_str()
                    .unwrap()
                    .contains("no-cache")
            );
            assert!(request.headers().get("authorization").is_none());
            let path = request.uri().path();
            self.paths.lock().unwrap().push(path.into());
            if let Some(status) = self.options.status {
                return Ok(Response::builder()
                    .status(status)
                    .body(Full::new(Bytes::new()))
                    .unwrap());
            }
            let target = artifact();
            let metadata = json!({"name":PACKAGE,"version":target.version,"dist":{"integrity":if self.options.wrong_integrity{"wrong".into()}else{target.integrity},"tarball":format!("{REGISTRY}/{PACKAGE}/-/{}",target.filename)}});
            let mut status = 200;
            let body = if path.contains("/-/") {
                let index = self.tarballs.fetch_add(1, AtomicOrdering::SeqCst);
                if self.options.delayed && index == 0 {
                    status = 404;
                    Vec::new()
                } else if self.options.wrong_bytes {
                    b"different archive".to_vec()
                } else {
                    BYTES.to_vec()
                }
            } else if path == "/claude-autorouter/0.4.0" {
                let reads = self.reads.fetch_add(1, AtomicOrdering::SeqCst) + 1;
                if self.options.absent
                    || self.options.exact404
                    || (self.options.delayed && reads < 3)
                {
                    status = 404;
                    Vec::new()
                } else {
                    metadata.to_string().into_bytes()
                }
            } else {
                let exists = !self.options.absent
                    && (!self.options.delayed || self.reads.load(AtomicOrdering::SeqCst) >= 3);
                json!({"name":PACKAGE,"dist-tags":{"latest":self.options.latest.as_deref().unwrap_or("0.4.0")},"versions":if exists{json!({"0.4.0":metadata})}else{json!({})}}).to_string().into_bytes()
            };
            Ok(Response::builder()
                .status(status)
                .body(Full::new(Bytes::from(body)))
                .unwrap())
        }
    }
    struct Install {
        calls: AtomicUsize,
        fail: bool,
    }
    impl Install {
        fn new(fail: bool) -> Self {
            Self {
                calls: AtomicUsize::new(0),
                fail,
            }
        }
    }
    impl Installer for Install {
        async fn install(
            &self,
            artifact: &Artifact,
            _: Duration,
            _: &CancellationToken,
        ) -> Result<Value, ReleaseError> {
            self.calls.fetch_add(1, AtomicOrdering::SeqCst);
            if self.fail {
                return Err(mismatch("installed_cli_failed"));
            }
            Ok(json!({"version":artifact.version,"isolated":true,"help":true}))
        }
    }
    #[tokio::test]
    async fn preflight_distinguishes_absence_outages_and_unknown_retries() {
        let target = artifact();
        let cancel = CancellationToken::new();
        let registry = Registry::new(Options {
            absent: true,
            latest: Some("0.3.7".into()),
            ..Default::default()
        });
        assert_eq!(
            preflight(&registry, &target, true, &cancel).await.unwrap()["publish"],
            true
        );
        assert_eq!(
            preflight(&registry, &target, false, &cancel).await.unwrap()["reason"],
            "retry_submission_unknown"
        );
        for latest in ["0.4.0", "0.5.0"] {
            let registry = Registry::new(Options {
                absent: true,
                latest: Some(latest.into()),
                ..Default::default()
            });
            assert_eq!(
                preflight(&registry, &target, true, &cancel)
                    .await
                    .unwrap_err()
                    .code,
                "version_order"
            );
        }
        for status in [401, 429, 503] {
            let registry = Registry::new(Options {
                status: Some(status),
                ..Default::default()
            });
            let err = preflight(&registry, &target, true, &cancel)
                .await
                .unwrap_err();
            assert_eq!(err.code, "registry_unavailable");
            assert_eq!(err.detail["http_status"], status);
        }
        let registry = Registry::new(Options {
            exact404: true,
            latest: Some("0.5.0".into()),
            ..Default::default()
        });
        let existing = preflight(&registry, &target, true, &cancel).await.unwrap();
        assert_eq!(existing["publish"], false);
        assert_eq!(existing["metadata_source"], "packument");
    }
    #[tokio::test(start_paused = true)]
    async fn delayed_metadata_and_tarball_are_polled_before_single_install() {
        let registry = Registry::new(Options {
            delayed: true,
            ..Default::default()
        });
        let installer = Install::new(false);
        let mut states = Vec::new();
        let report = verify(
            &registry,
            &installer,
            &artifact(),
            15000,
            &CancellationToken::new(),
            &mut |row| {
                states.push(row["state"].clone());
                Ok(())
            },
        )
        .await
        .unwrap();
        assert_eq!(report["state"], "verified");
        assert_eq!(report["elapsed_ms"], 7000);
        assert_eq!(installer.calls.load(AtomicOrdering::SeqCst), 1);
        assert_eq!(
            states,
            json!([
                "validating_unavailable",
                "validating_unavailable",
                "validating_unavailable",
                "verified"
            ])
            .as_array()
            .unwrap()
            .clone()
        );
    }
    #[tokio::test(start_paused = true)]
    async fn pending_retains_bounded_timeline_and_never_republishes() {
        let registry = Registry::new(Options {
            absent: true,
            ..Default::default()
        });
        let installer = Install::new(false);
        let report = verify(
            &registry,
            &installer,
            &artifact(),
            600000,
            &CancellationToken::new(),
            &mut |_| Ok(()),
        )
        .await
        .unwrap();
        assert_eq!(report["state"], "validating_unavailable");
        assert_eq!(report["pending"], true);
        assert_eq!(report["elapsed_ms"], 600000);
        assert_eq!(report["events"].as_array().unwrap().len(), 32);
        assert_eq!(installer.calls.load(AtomicOrdering::SeqCst), 0);
        assert!(
            report["message"]
                .as_str()
                .unwrap()
                .contains("do not submit a duplicate release")
        );
    }
    #[tokio::test(start_paused = true)]
    async fn immutable_mismatch_fails_immediately_and_superseded_versions_still_install() {
        for options in [
            Options {
                wrong_bytes: true,
                ..Default::default()
            },
            Options {
                wrong_integrity: true,
                ..Default::default()
            },
        ] {
            let registry = Registry::new(options);
            let installer = Install::new(false);
            let report = verify(
                &registry,
                &installer,
                &artifact(),
                5000,
                &CancellationToken::new(),
                &mut |_| Ok(()),
            )
            .await
            .unwrap();
            assert_eq!(report["state"], "failed");
            assert_eq!(report["attempts"], 1);
            assert_eq!(installer.calls.load(AtomicOrdering::SeqCst), 0);
        }
        let registry = Registry::new(Options {
            latest: Some("0.5.0".into()),
            ..Default::default()
        });
        let report = verify(
            &registry,
            &Install::new(false),
            &artifact(),
            5000,
            &CancellationToken::new(),
            &mut |_| Ok(()),
        )
        .await
        .unwrap();
        assert_eq!(report["state"], "verified");
        assert_eq!(report["dist_tag_status"], "superseded_by_newer_version");
        assert_eq!(registry.paths.lock().unwrap().len(), 3);
    }
    #[tokio::test(start_paused = true)]
    async fn older_tag_does_not_start_install_and_install_failure_is_not_verified() {
        let registry = Registry::new(Options {
            latest: Some("0.3.7".into()),
            ..Default::default()
        });
        let installer = Install::new(false);
        let report = verify(
            &registry,
            &installer,
            &artifact(),
            2000,
            &CancellationToken::new(),
            &mut |_| Ok(()),
        )
        .await
        .unwrap();
        assert_eq!(report["reason"], "dist_tag_not_updated");
        assert_eq!(installer.calls.load(AtomicOrdering::SeqCst), 0);
        let registry = Registry::new(Options::default());
        let report = verify(
            &registry,
            &Install::new(true),
            &artifact(),
            2000,
            &CancellationToken::new(),
            &mut |_| Ok(()),
        )
        .await
        .unwrap();
        assert_eq!(report["state"], "failed");
        assert_eq!(report["reason"], "installed_cli_failed");
    }
    #[test]
    fn original_tag_artifact_provenance_cannot_be_replaced_by_fork_or_retry_artifact() {
        let commit = "a".repeat(40);
        let run = json!({"id":123,"event":"push","path":".github/workflows/publish.yml","head_sha":commit,"head_branch":"v0.4.0","head_repository":{"full_name":REPOSITORY}});
        let artifact = json!({"id":456,"name":"npm-package-123-1","expired":false,"workflow_run":{"id":123,"head_sha":commit}});
        assert_eq!(
            validate_artifact_source(&run, &artifact, "123", "456", &commit, "v0.4.0").unwrap()["notes_artifact_name"],
            "release-notes-123-1"
        );
        for (key, value) in [
            ("event", json!("pull_request")),
            ("head_branch", json!("main")),
            ("head_repository", json!({"full_name":"untrusted/fork"})),
        ] {
            let mut changed = run.clone();
            changed[key] = value;
            assert!(
                validate_artifact_source(&changed, &artifact, "123", "456", &commit, "v0.4.0")
                    .is_err()
            );
        }
        let mut changed = artifact;
        changed["expired"] = json!(true);
        assert!(validate_artifact_source(&run, &changed, "123", "456", &commit, "v0.4.0").is_err());
    }
}
