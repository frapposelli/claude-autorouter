//! Pure AutoRouter policy and state transitions. No I/O or provider calls.

pub mod auth;
pub mod auto_routing;
pub mod config;
pub mod contracts;
pub mod evaluation_report;
pub mod fixture;
pub mod js_json;
pub mod model_catalog;
pub mod model_request;
pub mod policy;
pub mod prompt_state;
pub mod redaction;
pub mod request_validation;
pub mod router;
pub mod savings;
pub mod session_history;
pub mod status_state;
pub mod statusline;
pub mod telemetry_event;
pub mod turn_state;

#[cfg(test)]
pub(crate) mod test_support {
    include!("json_compare_macros.rs");
    contract_numeric_equal!(numeric_equal);
    contract_json_equal!(pub(crate) equal_js_json, a, b => numeric_equal(a, b));

    contract_json_equal!(source_equal, a, b => a.as_f64() == b.as_f64());
    contract_json_equal!(finite_equal, a, b => a.as_f64().zip(b.as_f64()).is_some_and(|(a, b)| a.is_finite() && b.is_finite() && a == b));

    #[test]
    fn shared_numeric_policies_keep_their_distinct_large_integer_contracts() {
        for (a, b, source, guarded) in [
            ("200", "200.0", true, true),
            ("9007199254740992", "9007199254740993", true, false),
            ("9007199254740993", "9007199254740992.0", true, false),
            (
                "18446744073709551615",
                "18446744073709551616.0",
                true,
                false,
            ),
            ("-9223372036854775808", "-9223372036854775808.0", true, true),
            ("9223372036854775807", "9223372036854775808.0", true, false),
            ("200", "201", false, false),
        ] {
            let a: serde_json::Value = serde_json::from_str(a).unwrap();
            let b: serde_json::Value = serde_json::from_str(b).unwrap();
            for (left, right) in [(&a, &b), (&b, &a)] {
                assert_eq!(source_equal(left, right), source);
                assert_eq!(finite_equal(left, right), source);
                assert_eq!(equal_js_json(left, right), guarded);
            }
        }
    }

    #[test]
    fn shared_walkers_preserve_keys_array_positions_and_primitive_kinds() {
        for compare in [source_equal, finite_equal, equal_js_json] {
            let original = serde_json::json!({"a":[1, {"b":false}], "c":null});
            assert!(compare(&original, &original.clone()));
            for different in [
                serde_json::json!({"a":[1, {"b":false}]}),
                serde_json::json!({"a":[{"b":false},1], "c":null}),
                serde_json::json!({"a":[1, {"b":0}], "c":null}),
                serde_json::json!({"a":[1, {"b":false}], "c":false}),
            ] {
                assert!(!compare(&original, &different));
            }
        }
    }
}
