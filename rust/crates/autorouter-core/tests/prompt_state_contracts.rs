//! Baseline prompt-state assertions, including task ownership and privacy.
//! JavaScript accessor side effects are a separate library API migration issue.
use autorouter_core::js_json::JsDocument;
use autorouter_core::prompt_state::{
    build_state, build_state_document, goal_feedback_indexes, prompt_excerpt,
    prompt_excerpt_document,
};
use serde_json::{Value, json};
use std::collections::BTreeSet;

fn text(value: impl Into<String>) -> Value {
    json!({"type":"text","text":value.into()})
}
fn user(content: Value) -> Value {
    json!({"role":"user","content":content})
}
fn assistant() -> Value {
    json!({"role":"assistant","content":"Work is partly complete."})
}
fn request(messages: Vec<Value>) -> Value {
    json!({"model":"claude-haiku-4-5-20251001","messages":messages})
}
fn goal(condition: &str) -> Value {
    user(json!([text(format!(
        "<command-name>/goal</command-name>\n<command-message>goal</command-message>\n<command-args>{condition}</command-args>"
    ))]))
}
fn feedback(condition: &str) -> Value {
    user(json!([text(format!(
        "Stop hook feedback:\n[{condition}]: Verification is still missing."
    ))]))
}
fn indexes(messages: &[Value]) -> BTreeSet<usize> {
    goal_feedback_indexes(&json!(messages))
}

#[test]
fn excerpts_select_latest_direct_text_and_omit_all_opaque_payloads() {
    let mut blocks = vec![
        text(format!(
            "<system-reminder>{}</system-reminder>",
            "Synthetic setup instructions ".repeat(1500)
        )),
        text("<available-deferred-tools>mcp__synthetic__lookup</available-deferred-tools>"),
        text("Describe the attached diagram."),
    ];
    for kind in [
        "image",
        "document",
        "thinking",
        "redacted_thinking",
        "tool_use",
        "future_block",
    ] {
        blocks.push(json!({"type":kind,"source":"PRIVATE_CANARY","title":"PRIVATE_CANARY","context":"PRIVATE_CANARY","thinking":"PRIVATE_CANARY","signature":"PRIVATE_CANARY","data":"PRIVATE_CANARY","name":"PRIVATE_CANARY","input":"PRIVATE_CANARY","text":"PRIVATE_CANARY","content":"PRIVATE_CANARY"}));
    }
    blocks.push(text("Keep the answer brief."));
    let mut body = request(vec![
        user(json!("Original task")),
        assistant(),
        user(json!(blocks)),
        assistant(),
        user(
            json!([text("Tool-result metadata is not a human task."), {"type":"tool_result","content":"PRIVATE_CANARY"}]),
        ),
        user(json!(
            "<system-reminder>Background work finished.</system-reminder>"
        )),
    ]);
    body["system"] = json!("PRIVATE_CANARY");
    body["tools"] = json!([{"name":"PRIVATE_CANARY"}]);
    let before = body.clone();
    assert_eq!(
        prompt_excerpt(&body, 500),
        "Describe the attached diagram.\nKeep the answer brief."
    );
    assert_eq!(body, before);
}

#[test]
fn excerpts_follow_goal_then_human_steering_and_keep_unrecognized_feedback() {
    let condition = "Implement a fixture and verify its acceptance test passes.";
    let mut messages = vec![
        goal(condition),
        assistant(),
        feedback(condition),
        assistant(),
        user(
            json!([{"type":"tool_result","tool_use_id":"test","content":"Synthetic test output."}]),
        ),
        assistant(),
        feedback(condition),
    ];
    let body = request(messages.clone());
    let before = body.clone();
    assert_eq!(
        prompt_excerpt(&body, 500),
        build_state(&body, 12000)["current_task"]
    );
    assert!(prompt_excerpt(&body, 500).contains(condition));
    assert!(!prompt_excerpt(&body, 500).contains("Stop hook feedback:"));
    assert_eq!(body, before);
    messages.extend([
        user(json!("Use the alternate fixture directory.")),
        assistant(),
        feedback(condition),
    ]);
    assert_eq!(
        prompt_excerpt(&request(messages.clone()), 500),
        "Use the alternate fixture directory."
    );
    messages.extend([assistant(), feedback("An unmatched condition")]);
    assert_eq!(
        prompt_excerpt(&request(messages.clone()), 500),
        messages.last().unwrap()["content"][0]["text"]
    );
}

#[test]
fn new_non_text_tasks_and_empty_inputs_never_reuse_earlier_excerpts() {
    for block in [
        json!({"type":"image","source":{"data":"synthetic-image"}}),
        json!({"type":"document","source":{"data":"synthetic-document"}}),
        json!({"type":"thinking","thinking":"synthetic-reasoning"}),
        json!({"type":"tool_use","name":"Synthetic","input":{"text":"synthetic-input"}}),
        json!({"type":"future_block","text":"Unknown blocks are not direct human text."}),
        json!({"type":"text","text":{"nested":"Malformed text must not be coerced."}}),
    ] {
        assert_eq!(
            prompt_excerpt(
                &request(vec![
                    user(json!("An earlier unrelated task.")),
                    assistant(),
                    user(json!([
                        text("<system-reminder>New context</system-reminder>"),
                        block
                    ]))
                ]),
                500
            ),
            ""
        );
    }
    for body in [
        json!({}),
        json!({"messages":null}),
        request(vec![]),
        request(vec![assistant()]),
        request(vec![user(json!("   "))]),
        request(vec![user(json!([text(
            "<available-deferred-tools>Names only</available-deferred-tools>"
        )]))]),
    ] {
        assert_eq!(prompt_excerpt(&body, 500), "");
    }
}

#[test]
fn excerpt_limits_count_scalars_and_repair_lone_surrogates() {
    let body = request(vec![user(json!("😀".repeat(600)))]);
    assert_eq!(prompt_excerpt(&body, 500), "😀".repeat(500));
    for limit in [0, 1, 2, 3, 500, 501] {
        assert_eq!(prompt_excerpt(&body, limit), "😀".repeat(limit));
    }
    let mixed = request(vec![user(json!([text("A😀"), text("𐐷B")]))]);
    assert_eq!(prompt_excerpt(&mixed, 4), "A😀\n𐐷");
    let lone =
        JsDocument::parse(br#"{"messages":[{"role":"user","content":"\ud800A\udc00"}]}"#).unwrap();
    assert_eq!(prompt_excerpt_document(&lone, 500), "�A�");
}

#[test]
fn bounded_excerpts_preserve_literal_and_malformed_wrapper_text() {
    let body = request(vec![
        user(json!("Earlier unrelated task")),
        user(json!([
            text("Current task ".repeat(10000)),
            text("PRIVATE_AFTER_LIMIT")
        ])),
    ]);
    assert_eq!(prompt_excerpt(&body, 12), "Current task");
    assert_eq!(prompt_excerpt(&body, 0), "");
    for task in [
        "Explain <system-reminder> in this syntax.",
        "<system-reminder>Context</system-reminder> Implement the parser.",
        "<available-deferred-tools>Explain the missing closing tag.",
    ] {
        assert_eq!(
            prompt_excerpt(&request(vec![user(json!([text(task)]))]), 500),
            task
        );
    }
}

#[test]
fn exact_feedback_retains_human_task_and_unmodified_history() {
    let condition = "Implement the feature and verify all acceptance tests pass.";
    let mut messages = vec![
        goal(condition),
        assistant(),
        feedback(condition),
        assistant(),
        user(json!(format!(
            "Stop hook feedback:\n[{condition}]: One test still fails."
        ))),
    ];
    messages[2]["content"][0]["cache_control"] = json!({"type":"ephemeral"});
    let before = messages.clone();
    assert_eq!(indexes(&messages), BTreeSet::from([2, 4]));
    let state = build_state(&request(messages.clone()), 12000);
    assert_eq!(state["current_task"], messages[0]["content"][0]["text"]);
    assert_eq!(state["original_task"], state["current_task"]);
    for expected in ["Verification is still missing.", "One test still fails."] {
        assert!(
            state["recent_messages"]
                .as_array()
                .unwrap()
                .iter()
                .any(|message| message["content"].as_str().unwrap().contains(expected))
        );
    }
    assert_eq!(messages, before);
}

#[test]
fn malformed_quoted_mixed_and_unestablished_feedback_remains_human() {
    let condition = "Verify the fixture.";
    let valid = format!("Stop hook feedback:\n[{condition}]: Work remains.");
    for content in [
        json!("Stop hook feedback: Work remains."),
        json!("Stop hook feedback:\n[Another goal]: Work remains."),
        json!(format!("Explain this quote: {valid}")),
        json!(format!("\n{valid}")),
        json!(format!("Stop hook feedback:\n[{condition}]:   ")),
        json!(format!("Stop hook feedback:\n[{condition}] Work remains.")),
        json!([text(&valid), text("This is a new human request.")]),
        json!([text(&valid), {"type":"tool_result","tool_use_id":"a","content":"Result"}]),
        json!([
            text(&valid),
            text("<system-reminder>Extra context</system-reminder>")
        ]),
    ] {
        let messages = vec![goal(condition), assistant(), user(content.clone())];
        assert!(indexes(&messages).is_empty());
        if content.is_string() {
            assert_eq!(
                build_state(&request(messages), 12000)["current_task"],
                content
            );
        }
    }
    for messages in [
        vec![assistant(), feedback(condition)],
        vec![goal(condition), feedback(condition)],
        vec![
            user(json!(format!(
                "Explain <command-name>/goal</command-name> and <command-args>{condition}</command-args>."
            ))),
            assistant(),
            feedback(condition),
        ],
    ] {
        assert!(indexes(&messages).is_empty());
    }
}

#[test]
fn human_steering_remains_current_while_feedback_refers_to_active_goal() {
    let condition = "Verify the fixture.";
    let messages = vec![
        goal(condition),
        assistant(),
        feedback(condition),
        assistant(),
        user(json!("Use the alternate test directory instead.")),
        assistant(),
        feedback(condition),
    ];
    assert_eq!(indexes(&messages), BTreeSet::from([2, 6]));
    assert_eq!(
        build_state(&request(messages), 12000)["current_task"],
        "Use the alternate test directory instead."
    );
}

#[test]
fn goal_replacement_clear_commands_and_empty_status_reset_or_retain_recognition() {
    let condition = "Verify the original fixture.";
    for replacement in [
        "clear",
        "stop",
        "off",
        "reset",
        "none",
        "cancel",
        "Verify the new fixture.",
    ] {
        let mut messages = vec![
            goal(condition),
            assistant(),
            feedback(condition),
            goal(replacement),
            assistant(),
            feedback(condition),
        ];
        assert_eq!(indexes(&messages), BTreeSet::from([2]));
        if replacement.starts_with("Verify") {
            messages.extend([assistant(), feedback(replacement)]);
            assert_eq!(indexes(&messages), BTreeSet::from([2, 7]));
        }
    }
    assert_eq!(
        indexes(&[goal(condition), goal(""), assistant(), feedback(condition)]),
        BTreeSet::from([3])
    );
}

#[test]
fn short_goal_labels_require_full_feedback_and_exact_utf16_truncation() {
    for (condition, short, invalid) in [
        (
            "x".repeat(510),
            format!("{}… [+10 chars]", "x".repeat(500)),
            format!("{}… [+999 chars]", "x".repeat(500)),
        ),
        (
            format!("{}😀tail", "x".repeat(499)),
            format!("{}… [+6 chars]", "x".repeat(499)),
            format!("{}… [+999 chars]", "x".repeat(499)),
        ),
    ] {
        assert!(indexes(&[goal(&condition), assistant(), feedback(&short)]).is_empty());
        let mut messages = vec![
            goal(&condition),
            assistant(),
            feedback(&condition),
            assistant(),
            feedback(&short),
        ];
        assert_eq!(indexes(&messages), BTreeSet::from([2, 4]));
        messages.extend([assistant(), feedback(&invalid)]);
        assert_eq!(indexes(&messages), BTreeSet::from([2, 4]));
        messages.extend([goal(&condition), assistant(), feedback(&short)]);
        assert_eq!(indexes(&messages), BTreeSet::from([2, 4]));
    }
}

#[test]
fn large_client_prefixes_do_not_displace_the_human_task() {
    let task = "Design a distributed lease with fencing tokens that remains safe under process pauses, duplicate delivery, and split brain. Explain the invariants and race conditions.";
    let body = request(vec![user(json!([
        text(format!(
            "<system-reminder>{}</system-reminder>",
            "Repository setup instructions ".repeat(1400)
        )),
        text(format!(
            "<available-deferred-tools>{}</available-deferred-tools>",
            "mcp__example__lookup\n".repeat(2000)
        )),
        text(task)
    ]))]);
    let state = build_state(&body, 12000);
    assert_eq!(state["current_task"], task);
    assert_eq!(state["original_task"], task);
    assert!(!state.to_string().contains("Repository setup instructions"));
    assert!(state.to_string().encode_utf16().count() <= 12000);
}

#[test]
fn new_task_precedes_old_task_and_trailing_tool_results() {
    let body = request(vec![
        user(json!("Implement a distributed lease.")),
        assistant(),
        user(json!([
            text("<system-reminder>New date</system-reminder>"),
            text("What does [].length return in JavaScript?")
        ])),
        json!({"role":"assistant","content":[{"type":"tool_use","id":"read","name":"Read","input":{"private":"PRIVATE_TOOL_INPUT"}}]}),
        user(
            json!([{"type":"tool_result","tool_use_id":"read","is_error":true,"content":[text("File lookup failed.")]}]),
        ),
        user(json!([text(
            "<system-reminder>Background task completed</system-reminder>"
        )])),
    ]);
    let state = build_state(&body, 12000);
    assert_eq!(state["original_task"], "Implement a distributed lease.");
    assert_eq!(
        state["current_task"],
        "What does [].length return in JavaScript?"
    );
    assert!(
        state
            .to_string()
            .contains("[tool result ERROR] File lookup failed.")
    );
    assert!(!state.to_string().contains("PRIVATE_TOOL_INPUT"));
}

#[test]
fn wrapper_mentions_and_plain_string_reminders_remain_human_instructions() {
    for task in [
        "Explain what <system-reminder> means in this XML format.",
        "<system-reminder>Context</system-reminder> Now implement a parser.",
        "<available-deferred-tools>Missing closing tag: explain this syntax error.",
        "<system-reminder>Keep this exact literal string</system-reminder>",
    ] {
        assert_eq!(
            build_state(&request(vec![user(json!(task))]), 12000)["current_task"],
            task
        );
        if !task.ends_with("</system-reminder>") {
            assert_eq!(
                build_state(&request(vec![user(json!([text(task)]))]), 12000)["current_task"],
                task
            );
        }
    }
}

#[test]
fn long_current_tasks_keep_both_ends_and_most_of_the_budget() {
    let mut body = request(vec![
        user(json!("Old task ".repeat(5000))),
        json!({"role":"assistant","content":"Old answer ".repeat(5000)}),
        user(json!(format!(
            "FIRST_INSTRUCTION {} FINAL_QUESTION",
            "Detailed task constraints ".repeat(3000)
        ))),
    ]);
    body["system"] = json!("Background system instructions ".repeat(1000));
    let doc = JsDocument::parse(body.to_string().as_bytes()).unwrap();
    let exact = build_state_document(&doc, 12000);
    let state = exact.observation();
    let current = state["current_task"].as_str().unwrap();
    assert!(current.starts_with("FIRST_INSTRUCTION"));
    assert!(current.ends_with("FINAL_QUESTION"));
    assert!(current.encode_utf16().count() >= 6000);
    assert!(!state["original_task"].as_str().unwrap().is_empty());
    assert!(!state["system"].as_str().unwrap().is_empty());
    assert!(exact.stringify().encode_utf16().count() <= 12000);
}

#[test]
fn escaped_state_fits_small_and_normal_budgets_without_opaque_payloads() {
    let mut body = request(vec![
        user(json!("original\0".repeat(1000))),
        json!({"role":"assistant","content":[{"type":"thinking","thinking":"PRIVATE_THINKING","signature":"PRIVATE_SIGNATURE"},{"type":"redacted_thinking","data":"PRIVATE_REDACTED"},{"type":"tool_use","name":"Read","input":{"value":"PRIVATE_INPUT"}}]}),
        user(
            json!([{"type":"tool_result","content":[text(format!("Result starts {} result ends", "\0".repeat(8000))),{"type":"image","source":{"data":"PRIVATE_IMAGE_BYTES"}}]}]),
        ),
        user(json!(format!(
            "CURRENT_START {} CURRENT_END",
            "\0\"\\".repeat(1000)
        ))),
    ]);
    body["system"] = json!("\0".repeat(1000));
    let doc = JsDocument::parse(body.to_string().as_bytes()).unwrap();
    for limit in [12000, 2000] {
        let state = build_state_document(&doc, limit);
        let serialized = state.stringify();
        assert!(serialized.encode_utf16().count() <= limit);
        assert!(
            state
                .current_task
                .to_well_formed()
                .starts_with("CURRENT_START")
        );
        assert!(state.current_task.to_well_formed().ends_with("CURRENT_END"));
        assert!(!state.original_task.units().is_empty());
        assert!(!state.system.units().is_empty());
        assert!(!serialized.contains("PRIVATE_"));
    }
}

#[test]
fn balanced_tool_history_preserves_errors_at_the_end_of_long_results() {
    let body = request(vec![
        user(json!("Fix the failed test.")),
        json!({"role":"assistant","content":[{"type":"tool_use","id":"test","name":"Bash","input":{"command":"secret command"}}]}),
        user(
            json!([{"type":"tool_result","tool_use_id":"test","is_error":true,"content":format!("START_OF_OUTPUT {} FAILURE_AT_END: race detected", "ordinary test output\n".repeat(2000))}]),
        ),
    ]);
    let state = build_state(&body, 12000);
    let result = state["recent_messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|message| message["content"].as_str())
        .find(|content| content.starts_with("[tool result ERROR]"))
        .unwrap();
    assert!(result.contains("START_OF_OUTPUT"));
    assert!(result.ends_with("FAILURE_AT_END: race detected"));
    assert!(!state.to_string().contains("secret command"));
}
