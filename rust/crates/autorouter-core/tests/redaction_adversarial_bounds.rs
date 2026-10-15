//! Complete frozen adversarial inputs, with opt-in original per-input bounds.
//! Ordinary CI checks exact corpus/output identities without a timing gate.
use autorouter_core::redaction::redact_sensitive;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::hint::black_box;
use std::sync::mpsc;
use std::time::{Duration, Instant};

const IDENTITIES: &str =
    include_str!("../../../parity/cases/redaction-adversarial.identities.json");
const IDENTITIES_SHA256: &str = "5c5937073063f719ea1296a57e8c43dc089924dbd3724bc4e13116467a93dde9";
const BASELINE: &str = "ea930c247626ce2af5ccdad721b5121417bf4ad8";
const LIMIT_NS: u64 = 2_000_000_000;
const MAX_INPUT: usize = 512 * 1024;
const MAX_OUTPUT: usize = 2 * 1024 * 1024;

fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn identities() -> Value {
    assert!(IDENTITIES.len() <= 32 * 1024);
    assert_eq!(hash(IDENTITIES.as_bytes()), IDENTITIES_SHA256);
    let value: Value = serde_json::from_str(IDENTITIES).unwrap();
    assert_eq!(value["schema_version"], 1);
    assert_eq!(value["kind"], "frozen_redaction_adversarial_identities");
    assert_eq!(value["baseline_commit"], BASELINE);
    assert_eq!(
        value["baseline_file_sha256"],
        "2c39564dea427488ffa04caeb78281a77c753d6a9deb61c067e8d1d505dab3a6"
    );
    let definitions = value["definitions"].as_array().unwrap();
    assert_eq!(definitions.len(), 2);
    for (row, (number, expected)) in definitions.iter().zip([
        (
            5,
            "0d9fec8f69d65dc344ee7c64b015bd1eafb8295aa5250189ab79be1ea97f945a",
        ),
        (
            16,
            "8dc19f60fc16fb9053096d247311fe8444b4f895448d6ed266bae6886baf8f3a",
        ),
    ]) {
        assert_eq!(row["id"], format!("test/redaction.test.mjs#{number}"));
        assert_eq!(row["definition_sha256"], expected);
    }
    assert_eq!(value["cases"].as_array().unwrap().len(), 28);
    value
}

fn warm_original_prefix(number: usize) {
    let input = match number {
        5 => format!("password={} and jane@example.com", "x".repeat(20)),
        16 => [
            "REDIS_URL=redis://:pw1234@h",
            "DB_PASS=abcd1234",
            "PW_SECRET='a b c d'",
            "mysql -pabcd1234",
            "curl -u a:bcd",
            "tool --api-key abcd1234",
            "Cookie: sid=abcdef123456",
            "+39 333 1234567",
            "SSN 123-45-6789",
        ]
        .join("\n"),
        _ => panic!("unknown frozen definition"),
    };
    let once = redact_sensitive(&input);
    assert!(
        redact_sensitive(&once) == once,
        "frozen redaction prefix {number} is not idempotent"
    );
}

fn inputs(number: usize) -> Vec<String> {
    match number {
        5 => vec![
            "a-".repeat(200000),
            "a@".repeat(200000),
            "4 ".repeat(200000),
            "AB12 ".repeat(100000),
            format!(
                "-----BEGIN PRIVATE KEY-----{}",
                "-----BEGIN PRIVATE KEY-----".repeat(5000)
            ),
            "Q".repeat(400000),
            format!("{}\n-----END PRIVATE KEY-----", "Q".repeat(400000)),
            format!("{}\n", "A".repeat(20)).repeat(20000),
        ],
        16 => vec![
            "mysql ".repeat(60000),
            "mysql -x ".repeat(40000),
            "password=\"".repeat(40000),
            "secret='".repeat(40000),
            "://:".repeat(100000),
            "a://:".repeat(60000),
            format!("http://{}", ":".repeat(300000)),
            format!("http://{}", "a:".repeat(150000)),
            "+1 ".repeat(100000),
            "+".repeat(300000),
            "1-".repeat(200000),
            " -".repeat(150000) + "token",
            "--a".repeat(100000),
            "DB_KEY".repeat(60000),
            "A_KEY=".repeat(60000),
            "Cookie:".repeat(60000),
            "sshpass -p ".repeat(30000),
            format!("pass{}", "a".repeat(300000)),
            format!("password={}", "a;".repeat(150000)),
            format!("password={}", "a,".repeat(150000)),
        ],
        _ => panic!("unknown frozen definition"),
    }
}

fn exercise(measure: bool) {
    let manifest = identities();
    let expected = manifest["cases"].as_array().unwrap();
    let mut index = 0;
    let mut total_input = 0;
    let mut total_output = 0;
    let mut failures = Vec::new();
    for number in [5, 16] {
        // Match the original callback's two idempotence calls before its array.
        warm_original_prefix(number);
        let cases = inputs(number);
        assert_eq!(cases.len(), if number == 5 { 8 } else { 20 });
        for (offset, input) in cases.iter().enumerate() {
            let id = format!("redaction-adversarial-{number}-{}", offset + 1);
            let row = &expected[index];
            assert_eq!(row["id"], id);
            assert_eq!(
                row["source_test"],
                format!("test/redaction.test.mjs#{number}")
            );
            assert!(input.len() <= MAX_INPUT);
            total_input += input.len();
            assert!(total_input <= 16 * 1024 * 1024);
            assert!(row["output_bytes"].as_u64().unwrap() <= MAX_OUTPUT as u64);

            let started = measure.then(Instant::now);
            let output = redact_sensitive(black_box(input.as_str()));
            let elapsed_ns = started.map(|time| u64::try_from(time.elapsed().as_nanos()).unwrap());
            // No input hashing, output checking or reporting occurs inside the timer.
            total_output += output.len();
            assert!(total_output <= 16 * 1024 * 1024);
            let input_hash = hash(input.as_bytes());
            let output_hash = hash(output.as_bytes());
            let identity_ok = row["input_bytes"] == input.len()
                && row["input_sha256"] == input_hash
                && output.len() <= MAX_OUTPUT
                && row["output_bytes"] == output.len()
                && row["output_sha256"] == output_hash;
            let time_ok = elapsed_ns.is_none_or(|elapsed| elapsed < LIMIT_NS);
            if measure {
                println!(
                    "AUTOROUTER_REDACTION_BOUND {}",
                    json!({
                        "schema_version":1, "kind":"native_original_redaction_bound",
                        "id":id, "source_test":row["source_test"],
                        "input_bytes":input.len(), "input_sha256":input_hash,
                        "output_bytes":output.len(), "output_sha256":output_hash,
                        "elapsed_ns":elapsed_ns, "limit_ns":LIMIT_NS,
                        "identity_ok":identity_ok, "passed":identity_ok && time_ok,
                        "profile":"release", "clock":"std::time::Instant",
                    })
                );
            }
            if !identity_ok || !time_ok {
                failures.push(id);
            }
            index += 1;
        }
    }
    assert_eq!(index, 28);
    assert!(
        failures.is_empty(),
        "frozen adversarial failures: {failures:?}"
    );
}

#[test]
fn all_frozen_adversarial_input_and_output_identities_match() {
    exercise(false);
}

#[test]
#[ignore = "opt-in original 2s per-input measurement; run in release after other builds stop"]
fn original_adversarial_each_input_finishes_below_two_seconds() -> Result<(), &'static str> {
    if cfg!(debug_assertions) {
        return Err("measurement requires release profile");
    }
    // Bound a non-yielding redactor too, without relying on an async timer.
    let (sender, receiver) = mpsc::channel();
    let watchdog = std::thread::spawn(move || {
        if matches!(
            receiver.recv_timeout(Duration::from_secs(90)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ) {
            eprintln!("original redaction bound fixture exceeded 90 seconds");
            std::process::exit(124);
        }
    });
    struct Watchdog(mpsc::Sender<()>, Option<std::thread::JoinHandle<()>>);
    impl Drop for Watchdog {
        fn drop(&mut self) {
            let _ = self.0.send(());
            if let Some(handle) = self.1.take() {
                let _ = handle.join();
            }
        }
    }
    let _watchdog = Watchdog(sender, Some(watchdog));
    exercise(true);
    Ok(())
}
