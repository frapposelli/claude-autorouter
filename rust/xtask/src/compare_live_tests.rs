use super::*;
use std::fs;
use std::os::unix::fs::PermissionsExt;

fn declaration_stub() -> Value {
    let mut config = json!({});
    for profile in PROFILES {
        config[profile] = json!({"config":{"evaluator":"jev","jevTimeoutMs":1500},"permission_mode":if *profile=="auto"{"auto"}else{"dontAsk"},"client_model":if *profile=="auto"{"claude-sonnet-5-5"}else{"claude-haiku-4-5-20251001"},"thinking_env":null});
    }
    let mut files = json!({});
    for path in [
        "rust/xtask/fixtures/merge-intervals.rs",
        "rust/xtask/fixtures/merge-intervals.test.rs",
        "rust/xtask/fixtures/repair-cargo.toml",
        "rust/parity/live-gateway.mjs",
    ] {
        files[path] = json!({"sha256":"a".repeat(64)});
    }
    json!({"metadata":{"claude_version":"2.1.292 (Claude Code)","max_turns_supported":true},"configuration":config,"identities":{"files":files}})
}
fn synthetic(declaration: &Value, profile: &str, engine: &str) -> Value {
    use autorouter_core::evaluation_report::{
        create_evaluation_policy, evaluate_live_case, evaluate_live_report,
    };
    let fixtures = crate::live::cases();
    let mut cases = Vec::new();
    let defaults = crate::live::parse_args(&[]).unwrap();
    for value in defaults["cases"].as_array().unwrap() {
        let name = value.as_str().unwrap();
        let scenario = &fixtures[name];
        let mut checks = json!({"claude_success":true,"expected_result":true,"requests_reached_router":true,"upstream_model_evidence":true,"no_upstream_api_errors":true,"no_proxy_errors":true,"no_permission_denials":true});
        if name == "coding" {
            for key in [
                "original_tests_preserved",
                "independent_tests_pass",
                "read_edit_bash_exercised",
                "tool_continuation_reached_router",
            ] {
                checks[key] = json!(true);
            }
        }
        if name == "thinking_continuation" {
            for key in ["first_request_routed_to_opus", "thinking_history_present"] {
                checks[key] = json!(true);
            }
            if profile == "auto" {
                checks["auto_thinking_uses_supported_models"] = json!(true);
            } else {
                checks["thinking_continuation_pinned"] = json!(true);
                checks["actual_upstream_stayed_opus"] = json!(true);
            }
        }
        let expected = if let Some(tiers) = scenario["expectedClassifiedTiers"].as_array() {
            tiers.clone()
        } else {
            vec![
                scenario["expectedClassifiedTier"].clone();
                scenario["prompts"].as_array().unwrap().len()
            ]
        };
        let routes:Vec<_>=expected.iter().enumerate().map(|(index,tier)| {
            let selected=if name=="thinking_continuation"&&profile=="compatible"{"opus"}else if profile=="auto"&&tier=="haiku"{"sonnet"}else{tier.as_str().unwrap()};
            json!({"classified_tier":tier,"expected_classified_tier":tier,"scenario_prompt_index":index,"source":"jev","model":format!("claude-{selected}-5"),"requested_model":declaration["configuration"][profile]["client_model"],"reason":if name=="thinking_continuation"&&profile=="compatible"&&index>0{"thinking_history"}else{"classified"},"request_class":"main","message_count":1,"message_roles":["user"],"request_bytes":100,"system_bytes":4,"tools_bytes":2,"messages_bytes":50,"has_system_messages":false,"thinking_type":"disabled","thinking_history_count":usize::from(name=="thinking_continuation"&&index>0),"tool_result_count":usize::from(name=="coding"),"tool_count":if name=="coding"{3}else{0},"typed_tools":[],"has_output_effort":false,"has_context_management":false,"latency_ms":1,"evaluation_latency_ms":1})
        }).collect();
        let observations:Vec<_>=routes.iter().enumerate().map(|(index,r)|json!({"request_id":format!("synthetic-{index}"),"requested_model":r["requested_model"],"selected_model":r["model"],"confirmed_model":r["model"],"source":"jev","classified_tier":r["classified_tier"],"http_status":200,"outcome":"request_complete"})).collect();
        let models: Vec<_> = routes.iter().map(|r| r["model"].clone()).collect();
        let tools = if name == "coding" {
            json!(["Read", "Edit", "Bash"])
        } else {
            json!([])
        };
        let outcomes = if name == "coding" {
            json!([{"tool":"Read","target_category":"fixture_source","result_received":true,"is_error":false,"permission_denied":false},{"tool":"Edit","target_category":"fixture_source","result_received":true,"is_error":false,"permission_denied":false},{"tool":"Bash","command_category":"exact_fixture_test","result_received":true,"is_error":false,"permission_denied":false}])
        } else {
            json!([])
        };

        let mut input =
            json!({"checks":checks,"routes":routes,"evaluator":"jev","expectOutage":false});
        if let Some(tier) = scenario.get("expectedClassifiedTier") {
            input["expectedClassifiedTier"] = tier.clone();
        }
        let accepted = evaluate_live_case(&input).unwrap();
        let cleanup = if engine == "native" {
            json!({"awaited":true,"listener_observation":"connection_refused","router_close_returned":true,"native_join_result_available":false})
        } else {
            json!({"awaited":true,"listener_observation":"connection_refused","reference_closed":true,"reference_protocol_valid":true,"reference_process_exit_code":0,"reference_process_timed_out":false,"reference_process_output_limit_exceeded":false})
        };
        let mut item = json!({"case":name,"checks":checks,"routes":routes,"request_observations":observations,"upstream_models":models,"upstream_statuses":[200],"proxy_errors":[],"classifier_succeeded":true,"model_changed":true,"provider_usage":[{"input_tokens":100,"output_tokens":10}],"api_equivalent_savings":[],"status_lines":[],"result_count":scenario["prompts"].as_array().unwrap().len(),"assistant_models":models,"tools_used":tools,"permission_denials":0,"parse_errors":0,"failures":[],"tool_outcomes":outcomes,"provider_usage_shapes":[],"successful_tools":tools,"denied_tool_details":[],"client_model_usage_estimate":{},"exit_code":0,"exit_signal":null,"duration_ms":1,"stderr_bytes":0,"timed_out":false,"output_limit_exceeded":false,"cancelled":false,"gateway_cleanup":cleanup,"fixture_removed":true});

        item.as_object_mut()
            .unwrap()
            .extend(accepted.as_object().unwrap().clone());
        cases.push(item);
    }
    let policy = create_evaluation_policy(Some(&json!({"profile":profile}))).unwrap();
    let accepted = evaluate_live_report(&json!(cases), &json!({"policy":policy})).unwrap();
    let provenance: Value =
        serde_json::from_str(include_str!("../fixtures/live-cases-v2.json")).unwrap();
    let projection:Vec<_>=fixtures.as_object().unwrap().iter().map(|(name,s)|json!({"name":name,"prompts":s["prompts"],"expected":s.get("expectedClassifiedTiers").or_else(||s.get("expectedClassifiedTier")).or_else(||s.get("expectedTier")).cloned().unwrap_or(Value::Null)})).collect();
    let mut report = json!({"type":"live_validation","engine":engine,"auth_mode":"subscription","evaluator":"jev","coverage_scope":"full_profile","client_profile":profile,"client_permission_mode":declaration["configuration"][profile]["permission_mode"],"client_model":declaration["configuration"][profile]["client_model"],"cli_version":declaration["metadata"]["claude_version"],"classifier_timeout_ms":declaration["configuration"][profile]["config"]["jevTimeoutMs"],"max_turns_supported":declaration["metadata"]["max_turns_supported"],"thinking_disabled":profile=="compatible","simulated_classifier_outage":false,"fixture_version":2,"fixture_sha256":digest(json!(projection).to_string().as_bytes()),"historical_fixture_version":provenance["historical_fixture_version"],"historical_fixture_sha256":provenance["historical_fixture_sha256"],"fixture_adaptation":provenance["adaptation"],"native_repair_source_sha256":declaration["identities"]["files"]["rust/xtask/fixtures/merge-intervals.rs"]["sha256"],"native_repair_test_sha256":declaration["identities"]["files"]["rust/xtask/fixtures/merge-intervals.test.rs"]["sha256"],"native_repair_manifest_sha256":declaration["identities"]["files"]["rust/xtask/fixtures/repair-cargo.toml"]["sha256"],"quality_policy":policy,"cases":cases,"passed":accepted["passed"],"gates":accepted["gates"],"cost_basis":"CLI list-price estimate, not subscription charges","classifier_succeeded":true,"model_changed":true});
    if engine == "node-reference" {
        report["reference"] = json!({"baseline_commit":"ea930c247626ce2af5ccdad721b5121417bf4ad8","verified_source_files":111,"temporary_reference_only":true,"adapter_sha256":declaration["identities"]["files"]["rust/parity/live-gateway.mjs"]["sha256"]});
        report["reference_runtime"] =
            json!({"node_version":"v22.14.0","address":"127.0.0.1:12345","adapter_version":1});
    }
    report
}
fn wire(report: &Value) -> Vec<u8> {
    let mut out = Vec::new();
    for case in report["cases"].as_array().unwrap() {
        let mut progress = case.clone();
        progress["type"] = json!("case_result");
        out.extend(progress.to_string().as_bytes());
        out.push(b'\n');
    }
    out.extend(report.to_string().as_bytes());
    out.push(b'\n');
    out
}
fn status(report: &Value) -> Value {
    json!({"complete":true,"exit_code":if report["passed"]==true{0}else{1}})
}
fn admit(report: &Value, declared: &Value, profile: &str, engine: &str) -> Result<Value, String> {
    schema::extract(
        &wire(report),
        &status(report),
        declared,
        profile,
        engine,
        &[],
    )
}
fn recompute(report: &mut Value) {
    use autorouter_core::evaluation_report::{evaluate_live_case, evaluate_live_report};
    let fixtures = crate::live::cases();
    for item in report["cases"].as_array_mut().unwrap() {
        let name = item["case"].as_str().unwrap();
        let mut input = json!({"checks":item["checks"],"routes":item["routes"],"evaluator":"jev","expectOutage":false});
        if let Some(expected) = fixtures[name].get("expectedClassifiedTier") {
            input["expectedClassifiedTier"] = expected.clone();
        }
        let accepted = evaluate_live_case(&input).unwrap();
        item["passed"] = accepted["passed"].clone();
        item["gates"] = accepted["gates"].clone();
    }
    let accepted = evaluate_live_report(
        &report["cases"],
        &json!({"policy":report["quality_policy"],"expectOutage":false}),
    )
    .unwrap();
    report["passed"] = accepted["passed"].clone();
    report["gates"] = accepted["gates"].clone();
}
#[test]
fn fixed_thinking_labels_accept_retries_retain_unknown_failure_and_reject_relabeling() {
    let declared = declaration_stub();
    let original = synthetic(&declared, "compatible", "native");
    let index = original["cases"]
        .as_array()
        .unwrap()
        .iter()
        .position(|c| c["case"] == "thinking_continuation")
        .unwrap();
    let mut relabeled = original.clone();
    for row in relabeled["cases"][index]["routes"].as_array_mut().unwrap() {
        row["expected_classified_tier"] = json!("sonnet");
        row["classified_tier"] = json!("sonnet");
    }
    recompute(&mut relabeled);
    assert!(admit(&relabeled, &declared, "compatible", "native").is_err());
    let mut unknown = original.clone();
    unknown["cases"][index]["routes"][1]["scenario_prompt_index"] = Value::Null;
    unknown["cases"][index]["routes"][1]
        .as_object_mut()
        .unwrap()
        .remove("expected_classified_tier");
    recompute(&mut unknown);
    assert_eq!(unknown["cases"][index]["gates"]["rubric"]["passed"], false);
    assert!(admit(&unknown, &declared, "compatible", "native").is_ok());
    assert_eq!(
        schema::compare(
            &json!({"node-reference":synthetic(&declared,"compatible","node-reference"),"native":unknown}),
            "compatible"
        )["fixture_mapping_complete"],
        false
    );
    let mut extra_unknown = original.clone();
    let mut unknown_row = extra_unknown["cases"][index]["routes"][0].clone();
    unknown_row["scenario_prompt_index"] = Value::Null;
    unknown_row
        .as_object_mut()
        .unwrap()
        .remove("expected_classified_tier");
    extra_unknown["cases"][index]["routes"]
        .as_array_mut()
        .unwrap()
        .push(unknown_row);
    recompute(&mut extra_unknown);
    assert!(admit(&extra_unknown, &declared, "compatible", "native").is_ok());
    let mut unknown_baseline = synthetic(&declared, "compatible", "node-reference");
    unknown_baseline["cases"][index]["routes"] = extra_unknown["cases"][index]["routes"].clone();
    recompute(&mut unknown_baseline);
    let comparison = schema::compare(
        &json!({"node-reference":unknown_baseline,"native":extra_unknown}),
        "compatible",
    );
    assert_eq!(comparison["fixture_mapping_complete"], false);
    assert_eq!(comparison["relative_gates_passed"], false);
    let mut repeated = original.clone();
    let first = repeated["cases"][index]["routes"][0].clone();
    repeated["cases"][index]["routes"]
        .as_array_mut()
        .unwrap()
        .insert(1, first);
    recompute(&mut repeated);
    assert!(admit(&repeated, &declared, "compatible", "native").is_ok());
    let mut duplicate = original.clone();
    duplicate["cases"][index]["routes"][1]["scenario_prompt_index"] = json!(0);
    recompute(&mut duplicate);
    assert!(admit(&duplicate, &declared, "compatible", "native").is_err());
}
#[test]
fn incomplete_or_nested_extra_original_fields_and_false_public_checks_cannot_qualify() {
    let declared = declaration_stub();
    let original = synthetic(&declared, "auto", "native");
    for variant in 0..13 {
        let mut report = original.clone();
        match variant {
            0 => {
                report["cases"][0]
                    .as_object_mut()
                    .unwrap()
                    .remove("result_count");
            }
            1 => {
                report["cases"][0]["request_observations"][0]["private_extension"] =
                    json!({"secret":true})
            }
            2 => {
                report["cases"][0]["provider_usage"][0]["private_extension"] =
                    json!({"secret":true})
            }
            3 => {
                report["cases"][0]["checks"]["no_upstream_api_errors"] = json!(false);
                recompute(&mut report);
            }
            4 => report["cases"].as_array_mut().unwrap().reverse(),
            5 => report["cases"][0]["checks"]["unknown_check"] = json!(true),
            6 => report["cases"][0]["gateway_cleanup"]
                .as_object_mut()
                .unwrap()
                .remove("router_close_returned")
                .map(|_| ())
                .unwrap(),
            7 => report["cases"][0]["provider_usage_shapes"] = json!([{"iterations":[]}]),
            8 => {
                report["cases"][0]["routes"][0]
                    .as_object_mut()
                    .unwrap()
                    .remove("latency_ms");
            }
            9 => {
                report["cases"][0]["routes"][0]
                    .as_object_mut()
                    .unwrap()
                    .remove("evaluation_latency_ms");
            }
            10 => {
                report["cases"][0]["routes"][0]
                    .as_object_mut()
                    .unwrap()
                    .remove("scenario_prompt_index");
            }
            11 => report["cases"][0]["routes"][0]["latency_ms"] = Value::Null,
            _ => report["cases"][0]["routes"][0]["evaluation_latency_ms"] = json!(-0.1),
        }
        assert!(admit(&report, &declared, "auto", "native").is_err());
    }
}

#[test]
fn full_profiles_have_original_nonempty_coverage_and_fresh_progress() {
    let declared = declaration_stub();
    for profile in PROFILES {
        for engine in ["node-reference", "native"] {
            let report = synthetic(&declared, profile, engine);
            assert!(admit(&report, &declared, profile, engine).is_ok());
            assert_eq!(
                report["quality_policy"]["requiredTiers"]
                    .as_array()
                    .unwrap()
                    .len(),
                if *profile == "auto" { 2 } else { 3 }
            );
        }
    }
}
#[test]
fn report_admission_rejects_vacuous_dropped_inconsistent_or_private_output() {
    let declared = declaration_stub();
    let original = synthetic(&declared, "auto", "native");
    for mutate in [0, 1, 2, 3, 4] {
        let mut report = original.clone();
        match mutate {
            0 => report["quality_policy"]["requiredTiers"] = json!([]),
            1 => {
                report["cases"].as_array_mut().unwrap().pop();
            }
            2 => report["cases"][0]["checks"]
                .as_object_mut()
                .unwrap()
                .remove("expected_result")
                .map(|_| ())
                .unwrap(),
            3 => report["provider_secret_extension"] = json!("private-credential-marker"),
            _ => report["cli_version"] = json!("private-credential-marker"),
        }
        let error = schema::extract(
            &wire(&report),
            &status(&report),
            &declared,
            "auto",
            "native",
            &["private-credential-marker".into()],
        )
        .unwrap_err();
        assert!(!error.contains("private-credential-marker"));
    }
    let mut progress = wire(&original);
    progress.extend(b"{\"type\":\"unexpected\"}\n");
    assert!(
        schema::extract(
            &progress,
            &status(&original),
            &declared,
            "auto",
            "native",
            &[]
        )
        .is_err()
    );
}
#[test]
fn relative_pair_never_hides_native_only_regressions_or_inherited_absolute_failure() {
    let declared = declaration_stub();
    let baseline = synthetic(&declared, "compatible", "node-reference");
    let mut native = synthetic(&declared, "compatible", "native");
    assert_eq!(
        schema::compare(
            &json!({"node-reference":baseline,"native":native}),
            "compatible"
        )["relative_gates_passed"],
        true
    );
    native["cases"][0]["gates"]["task"]["passed"] = json!(false);
    assert_eq!(
        schema::compare(
            &json!({"node-reference":baseline,"native":native}),
            "compatible"
        )["relative_gates_passed"],
        false
    );
    let mut baseline = baseline;
    baseline["passed"] = json!(false);
    native = synthetic(&declared, "compatible", "native");
    native["passed"] = json!(false);
    let same = schema::compare(
        &json!({"node-reference":baseline,"native":native}),
        "compatible",
    );
    assert_eq!(same["relative_gates_passed"], true);
    assert_eq!(same["absolute_original_quality_passed"], false);
    native["cases"][0]["gateway_cleanup"]["listener_observation"] = json!("unverified_timeout");
    assert_eq!(
        schema::compare(
            &json!({"node-reference":baseline,"native":native}),
            "compatible"
        )["relative_gates_passed"],
        false
    );
}
#[test]
fn arguments_and_credential_projection_are_explicit_and_value_safe() {
    assert!(
        parse_args(
            &["run", "--declaration", "prepared", "--output", "new"].map(str::to_owned),
            Path::new("/synthetic")
        )
        .is_err()
    );
    let base = Environment::from([
        (
            "TYPESAFE_API_KEY".into(),
            "private-credential-marker".into(),
        ),
        ("AUTOROUTER_EVALUATOR".into(), "jev".into()),
        ("AUTOROUTER_AUTH_MODE".into(), "subscription".into()),
        (
            "ANTHROPIC_CUSTOM_HEADERS".into(),
            "Authorization: private-header-marker".into(),
        ),
    ]);
    let projected = configuration(Path::new("/synthetic"), &base).unwrap();
    assert!(!projected.to_string().contains("private-credential"));
    assert!(!projected.to_string().contains("private-header"));
    assert!(private_material(
        &json!({"nested":["private-header-marker"]}),
        &secrets(&base)
    ));
    assert!(!private_material(&projected, &secrets(&base)));
}

#[test]
fn prepared_packet_rejects_duplicate_unknown_and_stale_checksum_before_admission() {
    let scratch = crate::tool_process::Scratch::new("live-packet").unwrap();
    let value = json!({"schema_version":1,"kind":"prepared_paired_live_language_port_comparison","prepared_at":"synthetic","source_commit":"b".repeat(40),"identities":{},"programs":{},"configuration":{},"metadata":{"node_version":"v22.14.0","max_turns_supported":true},"operator_declared":{},"observed":{},"credentials_persisted":false,"authentication_verified":false,"inference_performed":false,"source_to_binary_provenance_verified":false});
    let original = serde_json::to_vec(&value).unwrap();
    for (bytes, checksum) in [
        (original.clone(), digest(b"stale")),
        (
            format!("{{\"schema_version\":2,{}", &value.to_string()[1..]).into_bytes(),
            String::new(),
        ),
        (
            format!("{{\"private_extension\":true,{}", &value.to_string()[1..]).into_bytes(),
            String::new(),
        ),
    ] {
        fs::write(scratch.0.join("declaration.json"), &bytes).unwrap();
        fs::write(
            scratch.0.join("declaration-checksum.json"),
            json!({"sha256":if checksum.is_empty(){digest(&bytes)}else{checksum}}).to_string(),
        )
        .unwrap();
        assert!(declaration(&scratch.0).is_err());
    }
}

fn script(path: &Path, body: &str) {
    fs::write(path, body).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}
async fn fixture(
    name: &str,
) -> (
    PathBuf,
    crate::tool_process::Scratch,
    PathBuf,
    Environment,
    PathBuf,
) {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    let scratch = crate::tool_process::Scratch::new(name).unwrap();
    let claude = scratch.0.join("claude");
    let cargo = scratch.0.join("cargo");
    script(
        &claude,
        "#!/bin/sh\nif [ -n \"$TYPESAFE_API_KEY\" ]; then exit 9; fi\ncase \"$1\" in --version) printf '%s\\n' '2.1.292 (Claude Code)' ;; --help) printf '%s\\n' '--safe-mode --restricted --permission-mode --no-session-persistence --strict-mcp-config --max-turns' ;; *) exit 8 ;; esac\n",
    );
    script(
        &cargo,
        "#!/bin/sh\n[ \"$1\" = --version ] || exit 8\nprintf '%s\\n' 'cargo 1.94.0 (synthetic)'\n",
    );
    let node = PathBuf::from(
        std::env::var_os("AUTOROUTER_TEST_PINNED_NODE")
            .expect("Explicit pinned Node for synthetic metadata only"),
    );
    let mut base = crate::tool_process::environment();
    base.insert("AUTOROUTER_EVALUATOR".into(), "jev".into());
    base.insert(
        "TYPESAFE_API_KEY".into(),
        "private-synthetic-evaluator-credential".into(),
    );
    base.remove(&OsString::from("MAX_THINKING_TOKENS"));
    let declaration_path = scratch.0.join("prepared");
    assert!(
        prepare(
            Action::Prepare {
                output: declaration_path.clone(),
                node: node.clone(),
                claude,
                cargo,
                commit: "b".repeat(40)
            },
            &root,
            &CancellationToken::new(),
            &base
        )
        .await
        .unwrap()
    );
    (root, scratch, node, base, declaration_path)
}
fn read_report(path: &Path) -> Value {
    serde_json::from_slice(&regular(path, MAX).unwrap()).unwrap()
}
#[tokio::test]
#[ignore = "Explicit synthetic subprocess prepare/run control; no Claude/Jev calls"]
async fn prepared_driver_runs_twenty_owned_stub_commands_with_both_permission_profiles() {
    let (root, scratch, _, base, declaration_path) = fixture("live-driver-stub").await;
    let declared = declaration(&declaration_path).unwrap();
    assert!(
        !String::from_utf8(regular(&declaration_path.join("declaration.json"), MAX).unwrap())
            .unwrap()
            .contains("private-synthetic-evaluator-credential")
    );
    for profile in PROFILES {
        for engine in ["node-reference", "native"] {
            fs::write(
                scratch.0.join(format!("{profile}-{engine}.jsonl")),
                wire(&synthetic(&declared, profile, engine)),
            )
            .unwrap();
        }
    }
    let launched = std::sync::Mutex::new(Vec::new());
    let output = scratch.0.join("run");
    assert!(
        execute(
            &root,
            &declaration_path,
            &output,
            &CancellationToken::new(),
            &base,
            |command| {
                let args: Vec<_> = command
                    .get_args()
                    .map(|s| s.to_string_lossy().into_owned())
                    .collect();
                assert_eq!(args[0], "live-validation");
                assert!(!args.iter().any(|a| a == "--case"));
                let engine = args[2].clone();
                let mode = args[4].clone();
                let profile = if mode == "auto" { "auto" } else { "compatible" };
                launched
                    .lock()
                    .unwrap()
                    .push((profile.to_owned(), engine.clone(), mode));
                assert!(
                    command
                        .get_envs()
                        .any(|(k, v)| k == "TYPESAFE_API_KEY" && v.is_some())
                );
                let mut fake = Command::new("/bin/cat");
                fake.arg(scratch.0.join(format!("{profile}-{engine}.jsonl")));
                fake.env_clear();
                fake
            }
        )
        .await
        .unwrap()
    );
    let launched = launched.into_inner().unwrap();
    assert_eq!(launched.len(), 20);
    for (profile_index, profile) in PROFILES.iter().enumerate() {
        for pair in 0..5 {
            let start = profile_index * 10 + pair * 2;
            let order = if pair % 2 == 0 {
                ["node-reference", "native"]
            } else {
                ["native", "node-reference"]
            };
            assert_eq!(launched[start].0, *profile);
            assert_eq!(launched[start].1, order[0]);
            assert_eq!(launched[start + 1].1, order[1]);
        }
    }
    let report: Value = read_report(&output.join("comparison.json"));
    assert_eq!(report["complete"], true);
    assert_eq!(report["pairs"].as_array().unwrap().len(), 10);
    assert_eq!(report["relative_gates_passed"], true);
    let invalid = scratch.0.join("invalid-output");
    fs::write(&invalid, b"not-a-sanitized-report\n").unwrap();
    let invalid_calls = std::sync::atomic::AtomicUsize::new(0);
    let invalid_run = scratch.0.join("invalid-run");
    assert!(
        !execute(
            &root,
            &declaration_path,
            &invalid_run,
            &CancellationToken::new(),
            &base,
            |_| {
                invalid_calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let mut command = Command::new("/bin/cat");
                command.arg(&invalid);
                command.env_clear();
                command
            }
        )
        .await
        .unwrap()
    );
    assert_eq!(invalid_calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    let failed_worker_calls = std::sync::atomic::AtomicUsize::new(0);
    let failed_worker_run = scratch.0.join("worker-error-run");
    assert!(
        !execute(
            &root,
            &declaration_path,
            &failed_worker_run,
            &CancellationToken::new(),
            &base,
            |_| {
                failed_worker_calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Command::new(scratch.0.join("nonexistent-helper"))
            }
        )
        .await
        .unwrap()
    );
    assert_eq!(
        failed_worker_calls.load(std::sync::atomic::Ordering::SeqCst),
        1
    );
    let failed_summary: Value = read_report(&failed_worker_run.join("comparison.json"));
    assert_eq!(
        failed_summary["failure"],
        "invalid_or_incomplete_engine_report"
    );
    assert_eq!(failed_summary["complete"], false);

    let invalid_summary: Value = read_report(&invalid_run.join("comparison.json"));
    assert_eq!(
        invalid_summary["failure"],
        "invalid_or_incomplete_engine_report"
    );
    assert_eq!(invalid_summary["complete"], false);
    assert!(
        invalid_run
            .join("compatible-1-node-reference-rejected.json")
            .exists()
    );
    assert!(
        !invalid_run
            .join("compatible-1-native-execution.json")
            .exists()
    );
    assert!(
        prepare(
            Action::Prepare {
                output: declaration_path.clone(),
                node: PathBuf::from(declared["programs"]["node"]["path"].as_str().unwrap()),
                claude: PathBuf::from(declared["programs"]["claude"]["path"].as_str().unwrap()),
                cargo: PathBuf::from(declared["programs"]["cargo"]["path"].as_str().unwrap()),
                commit: "b".repeat(40)
            },
            &root,
            &CancellationToken::new(),
            &base
        )
        .await
        .is_err()
    );
    assert!(
        execute(
            &root,
            &declaration_path,
            &output,
            &CancellationToken::new(),
            &base,
            |_| panic!("Existing output cannot admit a process")
        )
        .await
        .is_err()
    );
    fs::write(
        declared["programs"]["claude"]["path"].as_str().unwrap(),
        "#!/bin/sh\nexit 8\n",
    )
    .unwrap();
    let drift = scratch.0.join("drift");
    assert!(
        execute(
            &root,
            &declaration_path,
            &drift,
            &CancellationToken::new(),
            &base,
            |_| panic!("Drift cannot admit a process")
        )
        .await
        .is_err()
    );
    assert!(!drift.exists());
}

#[tokio::test]
#[ignore = "Explicit synthetic TERM/nested-group observation; no Claude/Jev calls"]
async fn cancellation_joins_outer_stub_and_observes_its_owned_nested_term_cleanup() {
    let (root, scratch, node, base, prepared) = fixture("live-driver-cancel").await;
    let stub = scratch.0.join("nested.cjs");
    fs::write(&stub,r#"const fs=require('node:fs');const {spawn}=require('node:child_process');
const child=spawn(process.execPath,['-e',`const fs=require('node:fs');process.on('SIGTERM',()=>{fs.writeFileSync(process.env.DONE,'nested TERM observed');process.exit(0)});fs.writeFileSync(process.env.READY,'ready');setInterval(()=>{},1000)`],{detached:true,stdio:'ignore',env:{READY:process.env.READY,DONE:process.env.DONE}});
process.on('SIGTERM',()=>{child.kill('SIGTERM')});child.once('exit',()=>process.exit(0));setInterval(()=>{},1000);
"#).unwrap();
    let ready = scratch.0.join("ready");
    let done = scratch.0.join("done");
    let cancel = CancellationToken::new();
    let calls = std::sync::atomic::AtomicUsize::new(0);
    let output = scratch.0.join("cancelled");
    let execution = execute(&root, &prepared, &output, &cancel, &base, |command| {
        assert_eq!(command.get_args().next().unwrap(), "live-validation");
        calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let mut fake = Command::new(&node);
        fake.arg(&stub)
            .env_clear()
            .env("READY", &ready)
            .env("DONE", &done);
        fake
    });
    let stop = async {
        tokio::time::timeout(std::time::Duration::from_secs(15), async {
            while !ready.exists() {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("Nested stub must announce readiness before cancellation");
        cancel.cancel();
    };
    let (result, ()) = tokio::join!(execution, stop);
    assert!(!result.unwrap());
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(fs::read_to_string(done).unwrap(), "nested TERM observed");
    let report: Value = read_report(&output.join("comparison.json"));
    assert_eq!(report["complete"], false);
    assert_eq!(report["failure"], "cancelled");
    assert_eq!(report["nested_abnormal_group_cleanup_verified"], false);
    let status: Value = serde_json::from_slice(
        &regular(
            &output.join("compatible-1-node-reference-execution.json"),
            MAX,
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(status["cancelled"], true);
    assert_eq!(status["forced_kill"], false);
    assert_eq!(status["complete"], false);
}

#[test]
fn actual_ready_projection_keeps_version_identity_without_rejecting_float_one() {
    use autorouter_core::js_json::JsDocument;
    let ready = JsDocument::parse(
        br#"{"address":"127.0.0.1:12345","node_version":"v22.14.0","adapter_version":1}"#,
    )
    .unwrap()
    .to_serde_observation_lossy();
    // This is the exact production conversion in live_reference::Gateway::start.
    assert_eq!(ready["adapter_version"], json!(1.0));
    assert_ne!(ready["adapter_version"], json!(1));
    assert!(ready.to_string().contains("\"adapter_version\":1.0"));
    let declared = declaration_stub();
    for profile in PROFILES {
        let original = synthetic(&declared, profile, "node-reference");
        let mut projected = original.clone();
        projected["reference_runtime"] = ready.clone();
        assert!(admit(&original, &declared, profile, "node-reference").is_ok());
        assert!(admit(&projected, &declared, profile, "node-reference").is_ok());
        for version in [
            json!(0),
            json!(-1),
            json!(2),
            json!(1.5),
            json!(1.0000000000000002),
            json!(9007199254740993u64),
            json!(u64::MAX),
            Value::Null,
            json!("1"),
            json!(true),
        ] {
            let mut wrong = projected.clone();
            wrong["reference_runtime"]["adapter_version"] = version;
            assert_eq!(
                admit(&wrong, &declared, profile, "node-reference").unwrap_err(),
                "Actual frozen Node gateway identity differs"
            );
        }
        let mut missing = projected.clone();
        missing["reference_runtime"]
            .as_object_mut()
            .unwrap()
            .remove("adapter_version");
        assert_eq!(
            admit(&missing, &declared, profile, "node-reference").unwrap_err(),
            "Actual frozen Node gateway identity differs"
        );
        for field in [
            "verified_source_files",
            "baseline_commit",
            "adapter_sha256",
            "temporary_reference_only",
        ] {
            let mut wrong = projected.clone();
            wrong["reference"][field] = match field {
                "verified_source_files" => json!(110),
                "temporary_reference_only" => json!(false),
                "adapter_sha256" => json!("b".repeat(64)),
                _ => json!("b".repeat(40)),
            };
            assert_eq!(
                admit(&wrong, &declared, profile, "node-reference").unwrap_err(),
                "Actual frozen Node gateway identity differs"
            );
        }
        for (field, value) in [
            ("node_version", json!("v22.15.0")),
            ("address", json!("192.0.2.1:12345")),
            ("address", json!("127.0.0.1:0")),
        ] {
            let mut wrong = projected.clone();
            wrong["reference_runtime"][field] = value;
            assert_eq!(
                admit(&wrong, &declared, profile, "node-reference").unwrap_err(),
                "Actual frozen Node gateway identity differs"
            );
        }
        let mut extra = projected.clone();
        extra["reference_runtime"]["private_extension"] = json!("synthetic-private");
        assert_eq!(
            admit(&extra, &declared, profile, "node-reference").unwrap_err(),
            "Unknown nested reference metadata"
        );
        assert!(admit(&projected, &declared, profile, "native").is_err());
    }
}

// Optional process diagnostics never turn task completion into cleanup proof.
fn postexit_group_failure() -> Value {
    json!({"observation_error":false,"wait_error":false,"ownership_lost":false,"read_error":false,
        "signals":[{"target":"group","signal":"SIGKILL","outcome":"error","errno":1,"kind":null}]})
}
fn add_process_failure(item: &mut Value, gateway: bool) {
    let (process, prefix) = if gateway {
        (&mut item["gateway_cleanup"], "reference_process_")
    } else {
        (item, "")
    };
    process[format!("{prefix}supervision")] = postexit_group_failure();
    process[format!("{prefix}cleanup_confirmed")] = json!(false);
}
#[test]
fn optional_supervision_preserves_tasks_but_cannot_qualify_cleanup() {
    let declared = declaration_stub();
    for profile in PROFILES {
        let baseline = synthetic(&declared, profile, "node-reference");
        let native = synthetic(&declared, profile, "native");
        // Older reports with no optional diagnostics remain readable.
        assert!(admit(&baseline, &declared, profile, "node-reference").is_ok());
        assert!(admit(&native, &declared, profile, "native").is_ok());
        let mut native_error = native.clone();
        add_process_failure(&mut native_error["cases"][0], false);
        assert!(admit(&native_error, &declared, profile, "native").is_ok());
        assert_eq!(native_error["cases"][0]["exit_code"], 0);
        assert_eq!(native_error["cases"][0]["checks"]["claude_success"], true);
        let result = schema::compare(
            &json!({"node-reference":baseline,"native":native_error}),
            profile,
        );
        assert_eq!(result["absolute_original_quality_passed"], true);
        assert_eq!(result["native_only_transport_protocol_cleanup_failures"], 1);
        assert_eq!(result["observed_normal_cleanup_complete"], false);
        assert_eq!(result["relative_gates_passed"], false);
        assert_eq!(result["nested_abnormal_group_cleanup_verified"], false);

        let mut baseline_error = baseline.clone();
        add_process_failure(&mut baseline_error["cases"][0], true);
        assert!(admit(&baseline_error, &declared, profile, "node-reference").is_ok());
        for candidate in [native, native_error] {
            let result = schema::compare(
                &json!({"node-reference":baseline_error,"native":candidate}),
                profile,
            );
            // Correlated cleanup failures are never cancelled out as a pass.
            assert_eq!(result["native_only_transport_protocol_cleanup_failures"], 0);
            assert_eq!(result["absolute_original_quality_passed"], true);
            assert_eq!(result["observed_normal_cleanup_complete"], false);
            assert_eq!(result["relative_gates_passed"], false);
        }
    }
}
#[test]
fn optional_supervision_rejects_unknown_unbounded_and_inconsistent_metadata() {
    let declared = declaration_stub();
    for gateway in [false, true] {
        let engine = if gateway { "node-reference" } else { "native" };
        let original = synthetic(&declared, "auto", engine);
        for variant in 0..26 {
            let mut report = original.clone();
            add_process_failure(&mut report["cases"][0], gateway);
            let (process, prefix) = if gateway {
                (
                    &mut report["cases"][0]["gateway_cleanup"],
                    "reference_process_",
                )
            } else {
                (&mut report["cases"][0], "")
            };
            let supervision = format!("{prefix}supervision");
            match variant {
                0 => process[&supervision]["private_payload"] = json!("synthetic-private-marker"),
                1 => {
                    process[&supervision]
                        .as_object_mut()
                        .unwrap()
                        .remove("read_error");
                }
                2 => process[&supervision]["wait_error"] = json!("false"),
                3 => process[&supervision]["signals"] = json!({}),
                4 => {
                    let row = process[&supervision]["signals"][0].clone();
                    process[&supervision]["signals"] = json!([row, row, row, row]);
                }
                5 => process[&supervision]["signals"][0]["target"] = json!("unowned"),
                6 => {
                    process[&supervision]["signals"][0]["signal"] =
                        json!("synthetic-private-marker")
                }
                7 => process[&supervision]["signals"][0]["outcome"] = json!("success"),
                8 => {
                    process[&supervision]["signals"][0]["errno"] = json!("synthetic-private-marker")
                }
                9 => process[&supervision]["signals"][0]["errno"] = json!(1.5),
                10 => process[&supervision]["signals"][0]["errno"] = json!(2147483648i64),
                11 => {
                    process[&supervision]["signals"][0]["kind"] =
                        json!("synthetic-private-marker".repeat(100))
                }
                12 => process[&supervision]["signals"][0]["argv"] = json!(["private"]),
                13 => {
                    process[&supervision]["signals"][0]["target"] = json!("child");
                    process[&supervision]["signals"][0]["signal"] = json!("SIGTERM");
                    process[&supervision]["signals"][0]["kind"] = json!("direct_kill_error");
                }
                14 => process[&supervision]["signals"][0]["outcome"] = json!("absent"),
                15 => process[format!("{prefix}cleanup_confirmed")] = json!(true),
                16 => process[format!("{prefix}cleanup_confirmed")] = Value::Null,
                17 => process[format!("{prefix}observed_exit_code")] = json!("private"),
                18 => process[format!("{prefix}observed_exit_signal")] = json!("private"),
                19 => process[format!("{prefix}observed_exit_code")] = Value::Null,
                20 => {
                    process
                        .as_object_mut()
                        .unwrap()
                        .remove(&format!("{prefix}cleanup_confirmed"));
                }
                21 => {
                    process[format!("{prefix}observed_exit_code")] = json!(0);
                    process[format!("{prefix}observed_exit_signal")] = Value::Null;
                }
                22 => process[&supervision]["signals"][0]["kind"] = json!("PermissionDenied"),
                23 => process[&supervision]["observation_error"] = json!(true),
                24 => process[format!("{prefix}timed_out")] = json!(true),
                _ => {
                    process[&supervision]["signals"][0]["target"] = json!("child");
                    process[&supervision]["signals"][0]["kind"] = json!("direct_kill_error");
                }
            }
            let error = admit(&report, &declared, "auto", engine).unwrap_err();
            assert!(
                !error.contains("synthetic-private-marker"),
                "variant {variant}"
            );
        }
    }
}
#[test]
fn optional_failure_facts_and_bounded_absence_observations_remain_readable() {
    let declared = declaration_stub();
    let mut failed = synthetic(&declared, "auto", "native");
    let item = &mut failed["cases"][0];
    add_process_failure(item, false);
    item["exit_code"] = Value::Null;
    item["exit_signal"] = json!("SIGTERM");
    item["observed_exit_code"] = Value::Null;
    item["observed_exit_signal"] = json!("SIGTERM");
    item["checks"]["claude_success"] = json!(false);
    recompute(&mut failed);
    assert!(admit(&failed, &declared, "auto", "native").is_ok());
    assert_eq!(failed["cases"][0]["passed"], false);

    for gateway in [false, true] {
        let engine = if gateway { "node-reference" } else { "native" };
        let mut absent = synthetic(&declared, "auto", engine);
        let (process, prefix) = if gateway {
            (
                &mut absent["cases"][0]["gateway_cleanup"],
                "reference_process_",
            )
        } else {
            (&mut absent["cases"][0], "")
        };
        let errno = nix::libc::ESRCH;
        process[format!("{prefix}supervision")] = json!({
        "observation_error":false,"wait_error":false,"ownership_lost":false,"read_error":false,
        "signals":[
            {"target":"group","signal":"SIGTERM","outcome":"absent","errno":errno,"kind":null},
            {"target":"group","signal":"SIGKILL","outcome":"absent","errno":errno,"kind":null},
            {"target":"child","signal":"SIGKILL","outcome":"absent","errno":errno,"kind":"direct_kill_error"}
        ]});
        assert!(admit(&absent, &declared, "auto", engine).is_ok());
        let supervision = format!("{prefix}supervision");
        let process = if gateway {
            &mut absent["cases"][0]["gateway_cleanup"]
        } else {
            &mut absent["cases"][0]
        };
        process[&supervision]["signals"][2]["kind"] = json!("synthetic-private-marker");
        let error = schema::extract(
            &wire(&absent),
            &status(&absent),
            &declared,
            "auto",
            engine,
            &["synthetic-private-marker".into()],
        )
        .unwrap_err();
        assert!(!error.contains("synthetic-private-marker"));
    }
}
