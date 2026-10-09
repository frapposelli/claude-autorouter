//! Opt-in local model comparison with separate cold, warm and stress evidence.
use crate::evaluation::{digest, percentile, runtime, write_report};
use autorouter_core::config::{
    DEFAULT_OLLAMA_MODEL, RouterConfig, read_config, validate_ollama_endpoint,
    validate_ollama_model,
};
use autorouter_core::evaluation_report::{
    create_evaluation_policy, evaluate_routing_report, parse_quality_threshold,
};
use autorouter_core::js_json::JsDocument;
use autorouter_core::prompt_state::build_ollama_state_document;
use autorouter_runtime::bounded_json::read_response_document;
use autorouter_runtime::evaluator::{
    EvaluationError, evaluate_serialized_answer, ollama_questions,
};
use autorouter_runtime::http_client::{HttpTransport, NativeHttpClient};
use autorouter_runtime::ollama_setup::inspect_ollama;
use bytes::Bytes;
use http_body_util::Full;
use hyper::Request;
use serde_json::{Value, json};
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

const TIERS: &[&str] = &["haiku", "sonnet", "opus"];
pub fn parse_args(args: &[String], cwd: &Path) -> Result<Value, String> {
    let mut options = json!({"models":[DEFAULT_OLLAMA_MODEL],"split":"heldout","rounds":3,"timeoutMs":1500,"coldTimeoutMs":60000,"endpoint":"http://127.0.0.1:11434","stressRounds":0});
    let mut index = 0;
    while index < args.len() {
        let key = args[index].as_str();
        if key == "--help" {
            options["help"] = json!(true);
            index += 1;
            continue;
        }
        if ![
            "--models",
            "--split",
            "--rounds",
            "--stress-rounds",
            "--timeout-ms",
            "--cold-timeout-ms",
            "--endpoint",
            "--output",
            "--min-agreement",
            "--max-under-route-rate",
        ]
        .contains(&key)
            || args.get(index + 1).is_none_or(|v| v.is_empty())
        {
            return Err("Unknown or incomplete option; use --help".into());
        }
        let value = &args[index + 1];
        match key {
            "--models" => {
                options["models"] = json!(
                    value
                        .split(',')
                        .map(validate_ollama_model)
                        .collect::<Result<Vec<_>, _>>()?
                );
            }
            "--split" => options["split"] = json!(value),
            "--endpoint" => options["endpoint"] = json!(validate_ollama_endpoint(value)?),
            "--output" => {
                options["output"] = json!(
                    autorouter_core::config::resolve_path(cwd, Path::new(value)).to_string_lossy()
                )
            }
            "--min-agreement" | "--max-under-route-rate" => {
                options[if key == "--min-agreement" {
                    "minAgreement"
                } else {
                    "maxUnderRouteRate"
                }] = json!(parse_quality_threshold(&json!(value))?)
            }
            _ => {
                if !value.bytes().all(|c| c.is_ascii_digit()) {
                    return Err(format!("Invalid {key}"));
                }
                let number = value.parse::<u64>().map_err(|_| format!("Invalid {key}"))?;
                let field = match key {
                    "--rounds" => "rounds",
                    "--stress-rounds" => "stressRounds",
                    "--timeout-ms" => "timeoutMs",
                    _ => "coldTimeoutMs",
                };
                options[field] = json!(number);
            }
        }
        index += 2;
    }
    if !["tuning", "heldout", "all"].contains(&options["split"].as_str().unwrap_or("")) {
        return Err("--split must be tuning, heldout, or all".into());
    }
    for (field, flag, min, max) in [
        ("rounds", "rounds", 1, 20),
        ("timeoutMs", "timeout-ms", 0, 30000),
        ("coldTimeoutMs", "cold-timeout-ms", 1, 120000),
        ("stressRounds", "stress-rounds", 0, 20),
    ] {
        if options[field].as_u64().is_none_or(|n| n < min || n > max) {
            return Err(format!("Invalid --{flag}"));
        }
    }
    let models = options["models"].as_array().unwrap();
    if models.is_empty()
        || models
            .iter()
            .enumerate()
            .any(|(i, v)| models[..i].contains(v))
    {
        return Err("Model tags must be distinct".into());
    }
    options["endpoint"] = json!(validate_ollama_endpoint(
        options["endpoint"].as_str().unwrap()
    )?);
    let mut thresholds = json!({});
    for name in ["minAgreement", "maxUnderRouteRate"] {
        if let Some(value) = options.get(name) {
            thresholds[name] = value.clone();
        }
    }
    options["policy"] = serde_json::to_value(create_evaluation_policy(Some(&thresholds))?)
        .map_err(|_| "Invalid policy")?;
    Ok(options)
}
pub fn body_for(item: &Value) -> JsDocument {
    let messages = item
        .get("messages")
        .filter(|v| !v.is_null())
        .cloned()
        .unwrap_or_else(|| {
            let mut messages = item["history"].as_array().cloned().unwrap_or_default();
            messages.push(json!({"role":"user","content":item["prompt"]}));
            json!(messages)
        });
    let mut body = json!({"model":"claude-haiku-4-5-20251001","max_tokens":4096});
    if let Some(system) = item
        .get("system")
        .filter(|v| !v.is_null() && *v != false && *v != "")
    {
        body["system"] = system.clone();
    }
    body["messages"] = messages;
    JsDocument::parse(body.to_string().as_bytes()).expect("synthetic JSON")
}
pub fn stress_case(index: usize) -> Value {
    let nonce = digest(format!("synthetic-full-budget-{index}").as_bytes())[..24].to_owned();
    let lines=(0..160).map(|row|format!("Synthetic file entry {row}: export const label_{row} = 'amber'; unrelated background description {nonce}.")).collect::<Vec<_>>().join("\n");
    json!({"id":format!("full-budget-{index}"),"split":"performance_stress","expected":"haiku","system":format!("Synthetic performance nonce {nonce}. The following background text is not a new task."),
        "messages":[{"role":"user","content":"Replace the exact label 'amber' with 'blue' in the supplied file. Make only this literal text replacement."},
            {"role":"assistant","content":[{"type":"tool_use","id":"synthetic-read","name":"Read","input":{"file_path":"synthetic.txt"}}]},
            {"role":"user","content":[{"type":"tool_result","tool_use_id":"synthetic-read","content":lines}]}]})
}
pub fn order_for_round(items: &[Value], round: u32) -> Vec<Value> {
    let mut ordered = items.to_vec();
    let mut state = 1907_u32.wrapping_add(round.wrapping_mul(7919));
    for index in (1..ordered.len()).rev() {
        state = state.wrapping_mul(1664525).wrapping_add(1013904223);
        let other = state as usize % (index + 1);
        ordered.swap(index, other);
    }
    ordered
}
pub fn summarize(rows: &[Value]) -> Value {
    let mut matrix = json!({});
    for tier in TIERS {
        matrix[*tier] = json!({"haiku":0,"sonnet":0,"opus":0,"error":0});
    }
    for row in rows {
        let tier = row["expected"].as_str().unwrap_or("");
        let actual = row["actual"].as_str().unwrap_or("error");
        let n = matrix[tier][actual].as_u64().unwrap_or(0);
        matrix[tier][actual] = json!(n + 1);
    }
    let valid: Vec<_> = rows.iter().filter(|r| !r["actual"].is_null()).collect();
    let latencies: Vec<f64> = rows.iter().filter_map(|r| r["wall_ms"].as_f64()).collect();
    let successful: Vec<f64> = valid.iter().filter_map(|r| r["wall_ms"].as_f64()).collect();
    let rank = |value: &Value| TIERS.iter().position(|tier| Some(*tier) == value.as_str());
    json!({"requests":rows.len(),"valid":valid.len(),"errors":rows.len()-valid.len(),"exact_rubric_agreement":if rows.is_empty(){Value::Null}else{json!(rows.iter().filter(|r|r["actual"]==r["expected"]).count()as f64/rows.len()as f64)},
        "under_routes":valid.iter().filter(|r|rank(&r["actual"])<rank(&r["expected"])).count(),"over_routes":valid.iter().filter(|r|rank(&r["actual"])>rank(&r["expected"])).count(),
        "opus_under_routes":valid.iter().filter(|r|r["expected"]=="opus"&&r["actual"]!="opus").count(),"wall_p50_ms":percentile(&latencies,0.5),"wall_p95_ms":percentile(&latencies,0.95),
        "successful_wall_p50_ms":percentile(&successful,0.5),"successful_wall_p95_ms":percentile(&successful,0.95),"confusion":matrix})
}
pub async fn local_api<T: HttpTransport>(
    transport: &T,
    endpoint: &str,
    path: &str,
    body: Option<Value>,
    cancel: &CancellationToken,
) -> Result<Value, String> {
    let token = cancel.child_token();
    let _guard = token.clone().drop_guard();
    let action = async {
        let request = if let Some(body) = body {
            Request::post(format!("{endpoint}{path}"))
                .header("content-type", "application/json")
                .body(Full::new(Bytes::from(body.to_string())))
        } else {
            Request::get(format!("{endpoint}{path}")).body(Full::new(Bytes::new()))
        }
        .map_err(|_| "Invalid local Ollama request")?;
        let response = transport
            .request(request)
            .await
            .map_err(|_| "Cannot reach local Ollama")?;
        if !response.status().is_success() {
            return Err(format!(
                "Local Ollama {path} returned HTTP {}",
                response.status().as_u16()
            ));
        }
        let (parts, body) = response.into_parts();
        let document = read_response_document(body, &parts.headers, 1024 * 1024, &token)
            .await
            .map_err(|_| "Invalid, oversized or interrupted local Ollama response")?;
        Ok(document.to_serde_observation_lossy())
    };
    tokio::select! {biased;_=cancel.cancelled()=>Err("Local evaluation cancelled".into()),_=tokio::time::sleep(Duration::from_secs(60))=>Err("Local Ollama request timed out".into()),result=action=>result}
}
pub async fn measure<T: HttpTransport>(
    transport: &T,
    item: &Value,
    config: &RouterConfig,
    cancel: &CancellationToken,
) -> Value {
    let state = build_ollama_state_document(&body_for(item), 3000);
    let started = Instant::now();
    let result = evaluate_serialized_answer(transport, config, &state.stringify(), cancel).await;
    let elapsed = (started.elapsed().as_secs_f64() * 100000.0).round() / 100.0;
    let mut row = json!({"case":item["id"],"split":item["split"],"expected":item["expected"]});
    match result {
        Ok(answer) => {
            row["actual"] = json!(answer.choice);
            row["classified_tier"] = json!(answer.choice);
            row["source"] = json!("ollama");
            if let Some(metrics) = answer.metrics {
                row["metrics"] = json!(metrics);
            }
        }
        Err(error) => {
            row["actual"] = Value::Null;
            row["classified_tier"] = Value::Null;
            row["source"] = json!("error");
            row["error"] = json!(match error {
                EvaluationError::Timeout => "timeout",
                EvaluationError::Http { .. } => "http_error",
                EvaluationError::InvalidResponse => "invalid_response",
                _ => "local_error",
            });
            if let EvaluationError::Http { status, .. } = error {
                row["http_status"] = json!(status);
            }
        }
    }
    row["evaluator"] = json!("ollama");
    row["selected_model"] = Value::Null;
    row["confirmed_model"] = Value::Null;
    row["wall_ms"] = json!(elapsed);
    row["state_bytes"] = json!(state.stringify().len());
    row
}
fn command_text(command: &str, args: &[&str]) -> Option<String> {
    crate::process::capture(
        Command::new(command).args(args),
        b"",
        Duration::from_secs(5),
    )
    .ok()
    .and_then(|b| String::from_utf8(b).ok())
    .map(|s| s.trim().to_owned())
}
fn proc_number(name: &str) -> Option<u64> {
    std::fs::read_to_string("/proc/meminfo")
        .ok()?
        .lines()
        .find_map(|line| {
            line.strip_prefix(name)?
                .split_whitespace()
                .next()?
                .parse::<u64>()
                .ok()
                .map(|n| n * 1024)
        })
}
fn free_memory() -> Option<u64> {
    if cfg!(target_os = "linux") {
        return proc_number("MemFree:");
    }
    let output = command_text("/usr/bin/vm_stat", &[])?;
    let first = output.lines().next()?;
    let page = first
        .split("page size of ")
        .nth(1)?
        .split_whitespace()
        .next()?
        .parse::<u64>()
        .ok()?;
    output.lines().find_map(|line| {
        line.strip_prefix("Pages free:")?
            .trim()
            .trim_end_matches('.')
            .parse::<u64>()
            .ok()
            .map(|n| n * page)
    })
}
fn load_average() -> Value {
    let text = if cfg!(target_os = "linux") {
        std::fs::read_to_string("/proc/loadavg").ok()
    } else {
        command_text("/usr/sbin/sysctl", &["-n", "vm.loadavg"])
    };
    json!(text.map(|s| {
        s.split_whitespace()
            .filter_map(|n| n.parse::<f64>().ok())
            .take(3)
            .collect::<Vec<_>>()
    }))
}
fn hardware() -> Value {
    let cpu = if cfg!(target_os = "linux") {
        std::fs::read_to_string("/proc/cpuinfo").ok().and_then(|s| {
            s.lines().find_map(|l| {
                l.strip_prefix("model name")
                    .and_then(|l| l.split_once(':').map(|(_, v)| v.trim().to_owned()))
            })
        })
    } else {
        command_text("/usr/sbin/sysctl", &["-n", "machdep.cpu.brand_string"])
    };
    let total = if cfg!(target_os = "linux") {
        proc_number("MemTotal:")
    } else {
        command_text("/usr/sbin/sysctl", &["-n", "hw.memsize"]).and_then(|v| v.parse::<u64>().ok())
    };
    let logical = if cfg!(target_os = "linux") {
        std::fs::read_to_string("/proc/cpuinfo")
            .ok()
            .map(|s| s.lines().filter(|l| l.starts_with("processor\t")).count())
    } else {
        command_text("/usr/sbin/sysctl", &["-n", "hw.logicalcpu"])
            .and_then(|v| v.parse::<usize>().ok())
    };
    json!({"cpu":cpu,"logical_cpus":logical,"architecture":if cfg!(target_arch="aarch64"){"arm64"}else{std::env::consts::ARCH},"platform":if cfg!(target_os="macos"){"darwin"}else{std::env::consts::OS},
        "os_release":command_text("/usr/bin/uname",&["-r"]),"runtime":"Rust","tool_version":env!("CARGO_PKG_VERSION"),"total_memory_bytes":total,"initial_free_memory_bytes":free_memory(),"initial_load_average":load_average()})
}
async fn resident_memory<T: HttpTransport>(
    transport: &T,
    endpoint: &str,
    model: &str,
    cancel: &CancellationToken,
) -> Result<Value, String> {
    let status = local_api(transport, endpoint, "/api/ps", None, cancel).await?;
    let resident = status["models"].as_array().and_then(|models| {
        models
            .iter()
            .find(|item| item["name"] == model || item["model"] == model)
    });
    let rss = command_text("/bin/ps", &["-axo", "rss=,comm="]).map(|output| {
        output
            .lines()
            .filter_map(|line| {
                let line = line.trim_start();
                let at = line.find(char::is_whitespace)?;
                let bytes = line[..at].parse::<u64>().ok()? * 1024;
                let executable = line[at..].split_whitespace().next()?;
                ["ollama", "llama-server"]
                    .contains(&executable.rsplit('/').next()?)
                    .then_some(bytes)
            })
            .sum::<u64>()
    });
    let mut result = json!({"runtime_process_rss_bytes":rss,"system_free_bytes":free_memory()});
    if let Some(resident) = resident {
        for (output, input) in [
            ("model_size_bytes", "size"),
            ("model_vram_bytes", "size_vram"),
            ("context_length", "context_length"),
        ] {
            if let Some(v) = resident.get(input) {
                result[output] = v.clone();
            }
        }
        for (output, input) in [
            ("parameter_size", "parameter_size"),
            ("quantization", "quantization_level"),
        ] {
            if let Some(v) = resident["details"].get(input) {
                result[output] = v.clone();
            }
        }
    }
    Ok(result)
}
fn persist(options: &Value, report: &Value) -> Result<(), String> {
    if let Some(path) = options["output"].as_str() {
        write_report(Path::new(path), report, false)?;
    }
    Ok(())
}
pub async fn run_benchmark<T: HttpTransport + 'static>(
    transport: Arc<T>,
    options: &Value,
    fixture: &[u8],
    cancel: &CancellationToken,
) -> Result<Value, String> {
    let cases: Value = serde_json::from_slice(fixture).map_err(|_| "Invalid benchmark fixtures")?;
    let cases = cases.as_array().ok_or("Invalid benchmark fixtures")?;
    let selected: Vec<Value> = cases
        .iter()
        .filter(|item| options["split"] == "all" || item["split"] == options["split"])
        .cloned()
        .collect();
    if selected.is_empty()
        || cases.iter().enumerate().any(|(i, item)| {
            cases[..i].iter().any(|prior| prior["id"] == item["id"])
                || !item["expected"]
                    .as_str()
                    .is_some_and(|v| TIERS.contains(&v))
                || !item["split"]
                    .as_str()
                    .is_some_and(|v| ["tuning", "heldout"].contains(&v))
                || (!item["messages"].is_array() && !item["prompt"].is_string())
        })
    {
        return Err("Invalid benchmark fixtures".into());
    }
    let endpoint = options["endpoint"].as_str().unwrap();
    let active = local_api(transport.as_ref(), endpoint, "/api/ps", None, cancel).await?;
    if active["models"].as_array().is_some_and(|a| !a.is_empty()) {
        return Err("Ollama already has resident models. Leave them unchanged and rerun when the instance is idle.".into());
    }
    let installed = local_api(transport.as_ref(), endpoint, "/api/tags", None, cancel).await?;
    let installed = installed["models"].as_array().cloned().unwrap_or_default();
    for model in options["models"].as_array().unwrap() {
        if !installed
            .iter()
            .any(|item| item["name"] == *model || item["model"] == *model)
        {
            return Err(format!(
                "Pull {} explicitly before benchmarking",
                model.as_str().unwrap()
            ));
        }
    }
    let version = local_api(transport.as_ref(), endpoint, "/api/version", None, cancel).await?;
    let mut report = json!({"type":"ollama_routing_evaluation","timestamp":autorouter_runtime::server_events::timestamp(),"quality_policy":options["policy"],"hardware":hardware(),"ollama_version":version["version"],"fixture_sha256":digest(fixture),
        "split":options["split"],"rounds":options["rounds"],"stress_rounds":options["stressRounds"],"warm_timeout_ms":options["timeoutMs"],"cold_timeout_ms":options["coldTimeoutMs"],"state_character_budget":3000,
        "model_confidence":"not used for routing; native entropy confidence is not calibrated accuracy","models":[]});
    for model in options["models"].as_array().unwrap() {
        let model = model.as_str().unwrap();
        let env = json!({"AUTOROUTER_EVALUATOR":"ollama","AUTOROUTER_OLLAMA_URL":endpoint,"AUTOROUTER_OLLAMA_MODEL":model,"AUTOROUTER_OLLAMA_KEEP_ALIVE":"5m","AUTOROUTER_OLLAMA_TIMEOUT_MS":options["timeoutMs"].to_string()});
        let config = read_config(&env, false, Path::new("/tmp"))?;
        inspect_ollama(transport.as_ref(), &config, cancel, 5000)
            .await
            .map_err(|e| e.message)?;
        let info = installed
            .iter()
            .find(|item| item["name"] == model || item["model"] == model)
            .unwrap();
        let mut entry = json!({"model":model,"protocol":"/v1/systemone","rubric_sha256":digest(ollama_questions().to_string().as_bytes()),"context_configuration":"model/server defaults; measured below","rows":[]});
        for (out, key) in [("digest", "digest"), ("download_bytes", "size")] {
            if let Some(value) = info.get(key) {
                entry[out] = value.clone();
            }
        }
        let index = report["models"].as_array().unwrap().len();
        report["models"].as_array_mut().unwrap().push(entry.clone());
        let action:Result<(),String>=async{
            println!("Measuring {model}: cold load, then {} warm synthetic requests.",selected.len()*options["rounds"].as_u64().unwrap()as usize);
            let mut cold=config.clone();cold.ollama_timeout_ms=options["coldTimeoutMs"].as_u64().unwrap();entry["cold"]=measure(transport.as_ref(),&selected[0],&cold,cancel).await;
            entry["resident_memory"]=resident_memory(transport.as_ref(),endpoint,model,cancel).await?;
            if entry["cold"]["actual"].is_null(){entry["skipped"]=json!("cold request did not produce a valid tier");
println!("{model}: cold request failed ({}); skipping warm run.",entry["cold"]["error"].as_str().unwrap_or("local_error"));return Ok(());}
            for round in 0..options["rounds"].as_u64().unwrap(){for item in order_for_round(&selected,round as u32){let mut row=json!({"round":round+1});row.as_object_mut().unwrap().extend(measure(transport.as_ref(),&item,&config,cancel).await.as_object().unwrap().clone());entry["rows"].as_array_mut().unwrap().push(row);}
                report["models"][index]=entry.clone();persist(options,&report)?;
println!("{model}: completed round {}/{}.",round+1,options["rounds"]);
            }
            entry["summary"]=summarize(entry["rows"].as_array().unwrap());entry["acceptance"]=evaluate_routing_report(&entry["rows"],&json!({"evaluator":"ollama","policy":options["policy"],"classifierOnly":true}))?;
            entry["final_resident_memory"]=resident_memory(transport.as_ref(),endpoint,model,cancel).await?;
            let mut summary=json!({"model":model,"cold_wall_ms":entry["cold"]["wall_ms"],"resident_memory":entry["resident_memory"]});summary.as_object_mut().unwrap().extend(entry["summary"].as_object().unwrap().clone());
println!("{summary}");
            if options["stressRounds"].as_u64().unwrap()>0{let mut rows=Vec::new();
for i in 0..options["stressRounds"].as_u64().unwrap(){rows.push(measure(transport.as_ref(),&stress_case(i as usize),&config,cancel).await);}let mut policy=options["policy"].clone();policy["requiredTiers"]=json!(["haiku"]);
                entry["max_budget_stress"]=json!({"summary":summarize(&rows),"acceptance":evaluate_routing_report(&json!(rows),&json!({"evaluator":"ollama","classifierOnly":true,"policy":policy}))?,"rows":rows});
println!("{}",json!({"model":model,"max_budget_stress":entry["max_budget_stress"]["summary"]}));}
            Ok(())
        }.await;
        // Release only the candidate that this explicit benchmark admitted.
        // A fresh cancellation token allows bounded cleanup after interruption.
        let cleanup = local_api(
            transport.as_ref(),
            endpoint,
            "/api/generate",
            Some(json!({"model":model,"stream":false,"keep_alive":0})),
            &CancellationToken::new(),
        )
        .await;
        report["models"][index] = entry;
        persist(options, &report)?;
        cleanup?;
        action?;
        if cancel.is_cancelled() {
            return Err("Local evaluation cancelled".into());
        }
    }
    report["complete"] = json!(true);
    report["hardware"]["final_free_memory_bytes"] = json!(free_memory());
    report["hardware"]["final_load_average"] = load_average();
    report["passed"] = json!(
        !report["models"].as_array().unwrap().is_empty()
            && report["models"]
                .as_array()
                .unwrap()
                .iter()
                .all(|entry| entry.get("skipped").is_none()
                    && entry["acceptance"]["passed"] == true
                    && (entry.get("max_budget_stress").is_none()
                        || entry["max_budget_stress"]["acceptance"]["passed"] == true))
    );
    persist(options, &report)?;
    Ok(report)
}
pub fn run(args: &[String], root: &Path) -> Result<bool, String> {
    let cwd = std::env::current_dir().map_err(|_| "Cannot resolve working directory")?;
    let options = parse_args(args, &cwd)?;
    if options["help"] == true {
        println!(
            "Usage: cargo xtask evaluate-ollama --models nimble:9b-q4_K_M [--split tuning|heldout|all] [--rounds 3] [--stress-rounds 8] [--timeout-ms 1500] [--cold-timeout-ms 60000] [--min-agreement 1] [--max-under-route-rate 0] [--output artifacts/ollama-evaluation.json]\nUses checked-in synthetic cases and an already-running local Ollama. Downloads nothing and does not contact Claude/Jev. Requires no resident models; loads one candidate at a time and unloads only that candidate afterward. Cold calls, warm agreement, and optional full-budget stress remain separate. --timeout-ms 0 disables only the warm deadline. Set acceptance thresholds before running; labels do not measure downstream task quality."
        );
        return Ok(true);
    }
    let fixture = std::fs::read(root.join("test/fixtures/ollama-routing.json"))
        .map_err(|_| "Cannot read benchmark fixtures")?;
    let report = runtime()?.block_on(async {
        let transport =
            Arc::new(NativeHttpClient::new().map_err(|_| "Cannot construct HTTP transport")?);
        {
            let signals = crate::tool_process::Signals::new();
            run_benchmark(transport, &options, &fixture, &signals.token).await
        }
    })?;
    if let Some(output) = options["output"].as_str() {
        println!("Saved {output}");
    }
    let models: Vec<Value> = report["models"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| {
            let mut out = json!({"model":entry["model"]});
            for key in ["skipped", "acceptance"] {
                if let Some(value) = entry.get(key) {
                    out[key] = value.clone();
                }
            }
            if let Some(value) = entry["max_budget_stress"].get("acceptance") {
                out["stress_acceptance"] = value.clone();
            }
            out
        })
        .collect();
    println!(
        "{}",
        json!({"type":"evaluation_acceptance","quality_policy":options["policy"],"passed":report["passed"],"models":models})
    );
    Ok(report["passed"] == true)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parser_keeps_heldout_and_positive_cold_deadline_defaults() {
        let defaults = parse_args(&[], Path::new("/tmp")).unwrap();
        assert_eq!(defaults["split"], "heldout");
        assert_eq!(defaults["coldTimeoutMs"], 60000);
        for (flag, value) in [
            ("--rounds", "0"),
            ("--timeout-ms", "1e3"),
            ("--cold-timeout-ms", "0"),
            ("--stress-rounds", "21"),
            ("--models", "nimble:cloud"),
        ] {
            assert!(parse_args(&[flag.into(), value.into()], Path::new("/tmp")).is_err());
        }
        assert_eq!(
            parse_args(&["--timeout-ms".into(), "0".into()], Path::new("/tmp")).unwrap()["timeoutMs"],
            0
        );
    }
    #[test]
    fn stress_input_keeps_task_and_changes_early_background_nonce() {
        let a = stress_case(0);
        let b = stress_case(1);
        assert_ne!(a["system"], b["system"]);
        let state = build_ollama_state_document(&body_for(&a), 3000);
        assert!(state.stringify().len() <= 3000);
        assert!(
            state
                .current_task
                .to_well_formed()
                .starts_with("Replace the exact label")
        );
    }
    #[test]
    fn reports_keep_errors_in_latency_denominator_and_separate_wrong_labels() {
        let rows = vec![
            json!({"expected":"haiku","actual":"sonnet","wall_ms":1}),
            json!({"expected":"opus","actual":null,"wall_ms":100}),
        ];
        let summary = summarize(&rows);
        assert_eq!(summary["valid"], 1);
        assert_eq!(summary["errors"], 1);
        assert_eq!(summary["wall_p95_ms"], 100.0);
        assert_eq!(summary["successful_wall_p95_ms"], 1.0);
        assert_eq!(summary["over_routes"], 1);
    }
}
