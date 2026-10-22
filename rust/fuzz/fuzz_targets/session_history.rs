#![no_main]

use autorouter_core::session_history::{HistoryLimits, parse_session, summary_lines};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if data.len() > 65_536 || data.len() < 3 {
        return;
    }
    let limits = HistoryLimits {
        max_line_bytes: 16 + usize::from(data[0]) * 64,
        max_records: 1 + usize::from(data[1]),
        max_lines: 1 + usize::from(data[2]),
        ..HistoryLimits::default()
    };
    let bytes = &data[3..];
    let result = parse_session(bytes, bytes.len() as u64, "synthetic-fuzz", &limits);
    let records = result["records"].as_array().unwrap();
    assert!(records.len() <= limits.max_records);
    if let Some(first) = records.first() {
        for record in records {
            assert_eq!(record["session_id"], first["session_id"]);
            assert!(matches!(record["schema_version"].as_f64(), Some(1.0 | 2.0)));
            assert!(matches!(
                record["event"].as_str(),
                Some("decision" | "outcome")
            ));
        }
    }
    let coverage = &result["summary"]["coverage"];
    assert!(coverage["lines_read"].as_u64().unwrap() <= limits.max_lines as u64);
    assert_eq!(coverage["bytes_read"].as_u64(), Some(bytes.len() as u64));
    let _ = summary_lines(&result["summary"]);
});
