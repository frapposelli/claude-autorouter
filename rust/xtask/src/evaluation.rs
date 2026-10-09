//! Opt-in synthetic routing evaluation. Importing/parsing never makes calls.
use autorouter_core::config::{ClientProfile, Evaluator, RouterConfig, read_config};
use autorouter_core::evaluation_report::{
    EvaluationPolicy, create_evaluation_policy, evaluate_routing_report, model_tier,
    parse_quality_threshold, profile_tier,
};
use autorouter_core::js_json::JsDocument;
use autorouter_core::router::RouteOptions;
use autorouter_runtime::http_client::{HttpTransport, NativeHttpClient};
use autorouter_runtime::router::Router;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::Path;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

const TIERS: &[&str] = &["haiku", "sonnet", "opus"];
pub fn parse_args(args: &[String]) -> Result<Value, String> {
    let mut options = json!({});
    let mut index = 0;
    while index < args.len() {
        let key = args[index].as_str();
        if key == "--help" {
            options["help"] = json!(true);
            index += 1;
            continue;
        }
        if !["--min-agreement", "--max-under-route-rate", "--profile"].contains(&key)
            || args
                .get(index + 1)
                .is_none_or(|v| v.is_empty() || v.starts_with("--"))
        {
            return Err("Unknown or incomplete evaluation option; use --help".into());
        }
        let value = &args[index + 1];
        if key == "--profile" {
            options["profile"] = json!(value);
        } else {
            options[if key == "--min-agreement" {
                "minAgreement"
            } else {
                "maxUnderRouteRate"
            }] = json!(parse_quality_threshold(&json!(value))?);
        }
        index += 2;
    }
    create_evaluation_policy(Some(&options))?;
    Ok(options)
}
pub fn profile(config: &RouterConfig) -> &'static str {
    match config.client_profile {
        ClientProfile::Compatible => "compatible",
        ClientProfile::Native => "native",
        ClientProfile::Auto => "auto",
    }
}
fn evaluator(config: &RouterConfig) -> &'static str {
    match config.evaluator {
        Evaluator::Jev => "jev",
        Evaluator::Ollama => "ollama",
    }
}
fn configured_tier<'a>(config: &RouterConfig, model: &'a str) -> Option<&'a str> {
    [
        (&config.models.haiku, "haiku"),
        (&config.models.sonnet, "sonnet"),
        (&config.models.opus, "opus"),
    ]
    .into_iter()
    .find_map(|(candidate, tier)| (candidate == model).then_some(tier))
    .or_else(|| model_tier(model))
}
pub fn percentile(values: &[f64], fraction: f64) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    let mut values = values.to_vec();
    values.sort_by(f64::total_cmp);
    Some(values[(values.len() as f64 * fraction).ceil() as usize - 1])
}
pub async fn run_evaluation<T: HttpTransport + 'static>(
    transport: Arc<T>,
    config: &RouterConfig,
    cases: &Value,
    policy: EvaluationPolicy,
    cancellation: &CancellationToken,
) -> Result<Value, String> {
    // Validate everything before constructing a router or admitting HTTP work.
    let policy = create_evaluation_policy(Some(
        &serde_json::to_value(policy).map_err(|_| "Invalid policy")?,
    ))?;
    if policy.profile != profile(config) {
        return Err("Evaluation policy must match the configured client profile".into());
    }
    let cases = cases
        .as_array()
        .filter(|cases| {
            cases.iter().all(|item| {
                item["expected"]
                    .as_str()
                    .is_some_and(|v| TIERS.contains(&v))
                    && (item["prompt"].is_string()
                        || item
                            .get("request")
                            .is_some_and(|v| !v.is_null() && v != false))
            })
        })
        .ok_or("Invalid evaluation fixtures")?;
    let mut rows = Vec::new();
    for item in cases {
        let body = item.get("request").filter(|v| !v.is_null()).cloned().unwrap_or_else(||json!({"model":config.models.sonnet,"max_tokens":4096,"messages":[{"role":"user","content":item["prompt"]}]}));
        let document = Arc::new(
            JsDocument::parse(body.to_string().as_bytes())
                .map_err(|_| "Invalid evaluation request")?,
        );
        let router = Router::new(transport.clone(), config.clone());
        let result = router
            .route(
                document,
                RouteOptions {
                    request_class: "main".into(),
                    scope: format!(
                        "evaluation-{}",
                        item["name"].as_str().unwrap_or("undefined")
                    ),
                    ..Default::default()
                },
                &hyper::HeaderMap::new(),
                cancellation,
                "",
            )
            .await
            .map_err(|_| "Evaluation cancelled")?;
        router.shutdown();
        let requested_tier = configured_tier(config, body["model"].as_str().unwrap_or(""));
        let classified = result["classified_tier"].as_str();
        let confidence_tier = if config.evaluator == Evaluator::Jev
            && result["confidence"]
                .as_f64()
                .is_some_and(|v| v < config.min_confidence)
        {
            let rank =
                |tier: Option<&str>| TIERS.iter().position(|v| Some(*v) == tier).unwrap_or(0);
            Some(TIERS[1.max(rank(requested_tier)).max(rank(classified))])
        } else {
            classified
        };
        let mut row = json!({"expected":item["expected"],"classified_tier":result.get("classified_tier").unwrap_or(&Value::Null),"selected_model":result["model"],"confirmed_model":null});
        if let Some(name) = item.get("name") {
            row["case"] = name.clone();
        }
        if let Some(tier) = configured_tier(config, result["model"].as_str().unwrap_or("")) {
            row["selected_tier"] = json!(tier);
        }
        if let Some(expected) = item.get("expected_selected_tier").filter(|v| !v.is_null()) {
            row["expected_selected_tier"] = expected.clone();
        } else if let Some(tier) = confidence_tier {
            row["expected_selected_tier"] = json!(profile_tier(tier, &policy.profile));
        }
        if let Some(reason) = item
            .get("expected_reason")
            .filter(|v| v.as_str().is_some_and(|v| !v.is_empty()))
        {
            row["expected_reason"] = reason.clone();
        }
        for key in ["source", "evaluator", "reason"] {
            if let Some(value) = result.get(key) {
                row[key] = value.clone();
            }
        }
        row["confidence"] = result.get("confidence").cloned().unwrap_or(Value::Null);
        row["ms"] = result["latency_ms"].clone();
        rows.push(row);
    }
    let mut report = evaluate_routing_report(
        &json!(rows),
        &json!({"evaluator":evaluator(config),"policy":policy}),
    )?;
    let times: Vec<f64> = rows
        .iter()
        .filter_map(|row| row["ms"].as_f64().filter(|v| v.is_finite()))
        .collect();
    report["evaluator"] = json!(evaluator(config));
    report["rows"] = json!(rows);
    report["rubric_agreement"] = report["gates"]["rubric"]["agreement"].clone();
    report["fallback_count"] = report["gates"]["evaluator"]["fallbacks"].clone();
    report["routing_p50_ms"] = json!(percentile(&times, 0.5));
    report["routing_p95_ms"] = json!(percentile(&times, 0.95));
    Ok(report)
}
pub fn environment() -> Value {
    crate::env_file::effective()
}
pub fn runtime() -> Result<tokio::runtime::Runtime, String> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|_| "Cannot start native tool runtime".into())
}
pub fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
pub fn write_report(path: &Path, report: &Value, private: bool) -> Result<(), String> {
    use std::io::Write;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|_| "Cannot create report directory")?;
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        if private {
            options.mode(0o600);
        }
    }
    let mut file = options.open(path).map_err(|_| "Cannot write report")?;
    let bytes = serde_json::to_string_pretty(report).map_err(|_| "Cannot serialize report")?;
    writeln!(file, "{bytes}").map_err(|_| "Cannot write report".into())
}
pub fn run(args: &[String], root: &Path) -> Result<bool, String> {
    let options = parse_args(args)?;
    if options["help"] == true {
        println!(
            "Usage: cargo xtask evaluate [--profile compatible|native|auto] [--min-agreement 1] [--max-under-route-rate 0]\nExplicit evaluator calls only: Jev is paid; Ollama is local. Defaults require exact label agreement and no under-routing. Compatible coverage requires all three selected tiers; Auto requires Sonnet and Opus. Set thresholds before running. This measures synthetic rubric agreement and routing policy, not Claude task quality or net savings."
        );
        return Ok(true);
    }
    let mut env = environment();
    if let Some(profile) = options.get("profile") {
        env["AUTOROUTER_CLIENT_PROFILE"] = profile.clone();
    }
    let config = read_config(
        &env,
        false,
        &std::env::current_dir().map_err(|_| "Cannot resolve working directory")?,
    )?;
    let mut policy = options;
    policy["profile"] = json!(profile(&config));
    let policy = create_evaluation_policy(Some(&policy))?;
    if config.evaluator == Evaluator::Jev && config.jev_key.as_ref().is_none_or(|v| v.is_empty()) {
        return Err("Set TYPESAFE_API_KEY to run the Jev evaluation".into());
    }
    let fixture = std::fs::read(root.join("test/fixtures/routing.json"))
        .map_err(|_| "Cannot read routing fixtures")?;
    let cases: Value = serde_json::from_slice(&fixture).map_err(|_| "Invalid routing fixtures")?;
    let report = runtime()?.block_on(async {
        let client =
            Arc::new(NativeHttpClient::new().map_err(|_| "Cannot construct HTTP transport")?);
        {
            let signals = crate::tool_process::Signals::new();
            run_evaluation(client, &config, &cases, policy, &signals.token).await
        }
    })?;
    println!("case\texpected\tclassified\tselected\tsource\tms");
    for row in report["rows"].as_array().unwrap() {
        println!(
            "{}\t{}\t{}\t{}\t{}\t{}",
            row["case"].as_str().unwrap_or(""),
            row["expected"].as_str().unwrap_or(""),
            row["classified_tier"].as_str().unwrap_or("none"),
            row["selected_model"].as_str().unwrap_or(""),
            row["source"].as_str().unwrap_or(""),
            row["ms"]
        );
    }
    let mut output = json!({"type":"routing_evaluation","fixture_sha256":digest(&fixture)});
    output
        .as_object_mut()
        .unwrap()
        .extend(report.as_object().unwrap().clone());
    println!(
        "{}",
        serde_json::to_string_pretty(&output).map_err(|_| "Cannot serialize report")?
    );
    println!(
        "Labels are subjective rubric judgments. Transport and Claude task completion are not measured."
    );
    Ok(report["passed"] == true)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn args(items: &[&str]) -> Vec<String> {
        items.iter().map(|v| (*v).into()).collect()
    }
    #[test]
    fn thresholds_are_validated_before_help_or_any_provider_admission() {
        for value in ["NaN", "2", " ", "0x0", "0.5 ", "1e-1"] {
            assert!(parse_args(&args(&["--help", "--min-agreement", value])).is_err());
        }
        assert!(parse_args(&args(&["--profile", "other"])).is_err());
        assert_eq!(
            parse_args(&args(&[
                "--min-agreement",
                "0.9",
                "--max-under-route-rate",
                "0.1"
            ]))
            .unwrap()["minAgreement"],
            0.9
        );
    }
    #[test]
    fn percentile_keeps_nearest_rank_including_single_case() {
        assert_eq!(percentile(&[3.0, 1.0, 2.0], 0.5), Some(2.0));
        assert_eq!(percentile(&[3.0, 1.0, 2.0], 0.95), Some(3.0));
        assert_eq!(percentile(&[], 0.5), None);
    }
}
