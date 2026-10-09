#[path = "support/evaluation_contract.rs"]
mod support;
use autorouter_core::fixture;
use serde_json::json;
use support::*;

#[test]
fn frozen_pure_report_calls_preserve_every_gate_and_exact_threshold() {
    let mut replayed = 0;
    let mut tagged = 0;
    for row in cases() {
        let op = row["op"].as_str().unwrap();
        if ![
            "evaluation_policy",
            "routing_report",
            "live_case",
            "live_report",
        ]
        .contains(&op)
        {
            continue;
        }
        if row.get("native_boundary").is_some() {
            assert_eq!(op, "evaluation_policy");
            assert_eq!(
                row["source_tests"],
                json!(["test/evaluation-report.test.mjs#12"])
            );
            tagged += 1;
            continue;
        }
        let actual = outcome(fixture::execute(&row, std::path::Path::new("/synthetic")));
        assert!(
            equal(&actual, &row["node_expected"]),
            "{}: actual={actual} expected={}",
            row["id"],
            row["node_expected"]
        );
        replayed += 1;
    }
    assert_eq!((replayed, tagged), (44, 2));
}

#[test]
fn report_comparison_rejects_unmeasured_or_failed_quality_laundering() {
    let expected = json!({"gates":{"transport":{"passed":null},"rubric":{"passed":false,"agreement":0.75,"under_route_rate":0.25}},"passed":false});
    for path in ["/gates/transport/passed", "/gates/rubric/passed", "/passed"] {
        let mut actual = expected.clone();
        *actual.pointer_mut(path).unwrap() = json!(true);
        assert!(!equal(&actual, &expected), "{path}");
    }
    let mut actual = expected.clone();
    actual["gates"]["rubric"]["agreement"] = json!(1);
    assert!(!equal(&actual, &expected));
    actual = expected.clone();
    actual["gates"].as_object_mut().unwrap().remove("transport");
    assert!(!equal(&actual, &expected));
    assert!(!equal(
        &json!({"ok":false,"error":"wrong"}),
        &json!({"ok":false,"error":"expected"})
    ));
}
