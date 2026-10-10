#![no_main]

use autorouter_core::redaction::redact_sensitive;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if data.len() > 65_536 {
        return;
    }
    let text = String::from_utf8_lossy(data);
    let _ = redact_sensitive(&text);
    // Arbitrary surrounding syntax must not expose this recognized synthetic
    // Authorization token. No real credentials are accepted by the harness.
    const CANARY: &str = "synthetic_fuzz_private_token_0987654321";
    if text.contains(CANARY) {
        // An unlabeled occurrence supplied by the fuzzer is not a recognized
        // credential and must not make this field-specific oracle fail.
        return;
    }
    let wrapped = format!("{text}\nAuthorization: Bearer {CANARY}\n");
    assert!(!redact_sensitive(&wrapped).contains(CANARY));
});
