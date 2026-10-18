use serde_json::{Value, json};
use sha2::{Digest, Sha256};

pub const CASES: &str = include_str!("../../../../parity/cases/evaluation-report-contracts.jsonl");
pub fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
pub fn cases() -> Vec<Value> {
    assert_eq!(
        digest(CASES.as_bytes()),
        "2728c4cec322ae2ae49604b81d72e050a1d2840aaed6eea9701518c1b681269d"
    );
    let values: Vec<Value> = CASES
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(values.len(), 104);
    values
}
// JSON numbers are JavaScript doubles; spelling1 versus1.0 is not a policy
// difference. No epsilon, missing-key relaxation or null/false coercion applies.
include!("../../src/json_compare_macros.rs");
contract_json_equal!(pub equal, a, b => a.as_f64() == b.as_f64());
pub fn outcome(value: Result<Value, String>) -> Value {
    match value {
        Ok(result) => json!({"ok":true,"result":result}),
        Err(error) => json!({"ok":false,"error":error}),
    }
}
