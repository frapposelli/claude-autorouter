//! Full preimages captured from the unchanged frozen Node turn helpers.
use super::*;
const CASES: &str = include_str!("../../../parity/cases/turn-fingerprint-preimages.jsonl");
const CASES_SHA256: &str = "e6e5682c9ab894f3d6032c2bdad31c1ea4ed4f871c84b6be8d62ee012d2a50b6";

#[test]
fn all_frozen_turn_preimages_and_keys_match_through_normalization_and_prefixes() {
    assert!(CASES.len() <= 64 * 1024);
    assert_eq!(
        format!("{:x}", Sha256::digest(CASES.as_bytes())),
        CASES_SHA256
    );
    let mut ids = std::collections::HashSet::new();
    for line in CASES.lines() {
        let row: Value = serde_json::from_str(line).unwrap();
        let id = row["id"].as_str().unwrap();
        assert!(ids.insert(id.to_owned()));
        let doc = JsDocument::parse(row["raw_body"].as_str().unwrap().as_bytes()).unwrap();
        let scope = row["scope"].as_str().unwrap();
        let prompt = row["prompt_id"].as_str().unwrap();
        let end = row["prefix_end"]
            .as_u64()
            .map(|value| usize::try_from(value).unwrap());
        let actual = turn_info_prefix(&doc, scope, prompt, end);
        let expected = &row["expected"];
        let index = expected["index"].as_i64().unwrap();
        assert_eq!(actual.index, usize::try_from(index).ok(), "{id}: index");
        assert_eq!(actual.key, expected["key"], "{id}: primary key");
        assert_eq!(
            actual.content_key, expected["contentKey"],
            "{id}: content key"
        );
        assert_eq!(
            actual.goal_feedback, expected["goalFeedback"],
            "{id}: goal feedback"
        );
        assert_eq!(
            actual.continuation, expected["continuation"],
            "{id}: continuation"
        );

        let all = array(&doc, property(&doc, doc.root(), "messages")).unwrap_or_default();
        let messages = &all[..end.unwrap_or(all.len()).min(all.len())];
        let mut full = String::new();
        write_turn_preimage(&doc, scope, messages, actual.index, &mut full);
        assert!(
            full == row["content_preimage"].as_str().unwrap(),
            "{id}: full content preimage"
        );
        assert_eq!(
            format!("{:x}", Sha256::digest(full.as_bytes())),
            actual.content_key,
            "{id}: streamed digest"
        );
        if let Some(expected) = row["prompt_preimage"].as_str() {
            let mut full = String::new();
            write_prompt_preimage(scope, prompt, &mut full);
            assert!(full == expected, "{id}: full prompt preimage");
            assert_eq!(
                format!("{:x}", Sha256::digest(full.as_bytes())),
                actual.key,
                "{id}: streamed prompt digest"
            );
        } else {
            assert!(prompt.is_empty());
            assert_eq!(actual.key, actual.content_key, "{id}: content alias");
        }
    }
    assert_eq!(ids.len(), 26);
}

#[test]
fn buffered_turn_digest_preserves_every_fragment_across_fixed_batch_boundaries() {
    let document = JsDocument::parse(b"null").unwrap();
    for prefix in [0, 1, 63, 64, 65, 4095, 4096, 4097] {
        for payload in [0, 1, 4095, 4096, 4097, 8192, 16385] {
            let mut expected = "p".repeat(prefix);
            let text = "é水🦀".repeat(payload);
            let mut actual = TurnDigest::new();
            actual.push_str(&expected);
            actual.push('"');
            expected.push('"');
            actual.push_str(&text);
            expected.push_str(&text);
            for fragment in ["", ",", "null", "\\ud800", "\n", "tail"] {
                actual.push_str(fragment);
                expected.push_str(fragment);
                let (used, capacity) = actual.buffered_state();
                assert!(used < capacity);
            }
            assert_eq!(
                actual.finish(),
                format!("{:x}", Sha256::digest(expected.as_bytes()))
            );
            let legacy: [u8; 32] = Sha256::digest(format!("{expected}null|tail").as_bytes()).into();
            assert_eq!(
                document.sha256_with_fragments(&["", &expected], &["|", "", "tail"]),
                legacy
            );
        }
    }
}
