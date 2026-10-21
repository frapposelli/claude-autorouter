use super::*;

fn assignment(ordinal: usize) -> AssignmentObservation {
    AssignmentObservation::new(
        ordinal,
        ordinal,
        1,
        Some(gateway_terminal::Identity {
            connection: 41,
            request: ordinal as u64,
        }),
        Some(ordinal + 1),
    )
}
#[test]
fn retained_raw_lifecycle_preserves_original_projection_and_exact_correlations() {
    let probe = Probe::default();
    probe.observe_lifecycle();
    let legacy = probe.trace_reader();
    let reader = probe.lifecycle_reader();
    probe.record_assignment(assignment(0), false);
    probe.record(
        "consumer_complete",
        serde_json::json!({"connection":1,"clean":true,
        "unpoisoned":true,"sender_ready":false,"sender_closed":false}),
    );
    probe.record(
        "free",
        serde_json::json!({"connection":1,"kept":true,"timeout":5000}),
    );
    probe.record(
        "driver_finished",
        serde_json::json!({"connection":1,"ok":true}),
    );
    probe.record("close", serde_json::json!({"connection":1}));
    let before = legacy.snapshot();
    assert_eq!(before["rows"].as_array().unwrap().len(), 3);
    assert_eq!(before["assignment_history"], serde_json::json!([[0, 1]]));
    assert_eq!(
        before["rows"][0].as_object().unwrap().len(),
        5,
        "assigned schema remains event/request/connection/reused/ms"
    );
    drop(probe);
    let after = reader.snapshot();
    assert_eq!(
        legacy.snapshot(),
        before,
        "reader retains scalar rows after owner drop"
    );
    let rows = after["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 5);
    assert_eq!(
        rows.iter()
            .map(|row| row["sequence"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        vec![0, 1, 2, 3, 4]
    );
    assert_eq!(rows[1]["event"], "consumer_complete");
    assert_eq!(rows[3]["event"], "driver_finished");
    assert_eq!(
        rows[4]["event"], "close",
        "normal return and RAII release remain distinct"
    );
    assert_eq!(
        after["assignments"],
        serde_json::json!([{
            "raw_sequence":0,"raw_request":0,"raw_label":0,"raw_connection":1,
            "terminal":{"downstream_connection":41,"downstream_request":0},"idle_request":1
        }])
    );
}
#[test]
fn passive_diagnostics_do_not_apply_replay_limits_to_other_raw_constructors() {
    let probe = Probe::default();
    for ordinal in 0..20 {
        probe.record_assignment(
            AssignmentObservation::new(ordinal, 500 + ordinal, 9 + ordinal, None, None),
            false,
        );
    }
    assert_eq!(probe.snapshot().0.len(), 20);
    assert!(probe.state.lock().unwrap().assignments.is_none());
    let buffers = BufferReader::default();
    for _ in 0..20 {
        buffers.source_end(None, buffered_body::End::Clean);
    }
    let state = buffers.0.lock().unwrap();
    assert!(!state.enabled && state.ends.is_empty());
}
#[test]
#[should_panic(expected = "raw lifecycle correlation failure")]
fn raw_lifecycle_seventeenth_request_is_a_failure_not_truncated_history() {
    let probe = Probe::default();
    probe.observe_lifecycle();
    for ordinal in 0..17 {
        probe.record_assignment(assignment(ordinal), false);
    }
    assert_eq!(
        probe.snapshot().0.len(),
        17,
        "original assigned recording continued"
    );
    probe.lifecycle_reader().snapshot();
}
#[test]
#[should_panic(expected = "raw lifecycle closed field set")]
fn raw_lifecycle_rejects_unexpected_fields_instead_of_leaking_payloads() {
    let probe = Probe::default();
    probe.observe_lifecycle();
    probe.record(
        "close",
        serde_json::json!({"connection":1,"payload":"synthetic"}),
    );
    probe.lifecycle_reader().snapshot();
}
#[test]
#[should_panic(expected = "terminal lifecycle complete-history bound")]
fn terminal_lifecycle_cannot_claim_a_retained_suffix_is_complete() {
    let events = vec![gateway_terminal::Event::Closed(1); 1024];
    terminal_rows(&events);
}
#[test]
fn producer_lifecycle_seventeenth_completion_fails_closed() {
    let reader = BufferReader::default();
    reader.observe_lifecycle();
    for _ in 0..17 {
        reader.source_end(None, buffered_body::End::Clean);
    }
    assert_eq!(
        reader.0.lock().unwrap().ends.len(),
        16,
        "retained data remains bounded"
    );
    let value = reader.snapshot();
    assert_eq!(value["complete"], false);
    assert_eq!(value["failure"], "observer_invariant");
}
#[tokio::test]
async fn retained_clean_producer_does_not_report_consumer_or_delivery_completion() {
    let reader = BufferReader::default();
    reader.observe_lifecycle();
    let observed = reader.clone();
    let id = gateway_terminal::Identity {
        connection: 42,
        request: 0,
    };
    let (body, producer, probe) =
        buffered_body::start(Full::new(Bytes::from_static(b"synthetic")), move |end| {
            observed.source_end(Some(id), end)
        });
    reader.register(Some(id), probe);
    producer.join().await.unwrap();
    let live = reader.snapshot();
    assert_eq!(live["rows"].as_array().unwrap().len(), 1);
    assert_eq!(live["rows"][0]["end"], "clean");
    assert_eq!(live["rows"][0]["terminal"], identity(id));
    assert_eq!(live["buffers"][0]["queued_bytes"], 9);
    assert_eq!(
        live["buffers"][0]["consumer_polls"], 0,
        "complete source has not polled or forwarded the queued body"
    );
    assert_eq!(live["buffers"][0]["producer_tasks"], 0);
    drop(body);
    let retired = reader.snapshot();
    assert_eq!(
        retired["rows"], live["rows"],
        "terminal source observation survives weak pool expiry"
    );
    for key in [
        "live_pools",
        "allocated_blocks",
        "outstanding_blocks",
        "queued_bytes",
        "producer_tasks",
    ] {
        assert_eq!(
            retired["buffers"][0][key], 0,
            "retained reader owns no buffer resources"
        );
    }
    assert!(
        retired["buffers"][0]["current_end"].is_null(),
        "expired current source is unavailable, not invented from a completion row"
    );
}
#[tokio::test]
async fn terminal_domain_uses_actual_identity_and_keeps_delivery_callbacks_separate() {
    use crate::transport_completion::Delivery;
    let probe = gateway_terminal::Probe::default();
    let connection = gateway_terminal::ConnectionTerminal::new(probe.clone(), 1);
    let owner = connection.handle().register().unwrap();
    let id = owner.publisher().diagnostic_identity();
    owner.attach();
    let mut claim = owner.claim_delivery(Delivery::Flushed);
    claim.callback_finished();
    drop(claim);
    let abandoned = connection.handle().register().unwrap();
    let retired_id = abandoned.publisher().diagnostic_identity();
    drop(abandoned);
    connection.close().await;
    let value = terminal_snapshot(&probe);
    let rows = value["rows"].as_array().unwrap();
    for event in [
        "registered",
        "attached",
        "delivery_claimed",
        "callback_finished",
    ] {
        let row = rows.iter().find(|row| row["event"] == event).expect(event);
        assert_eq!(row["terminal"], identity(id));
    }
    let claimed = rows
        .iter()
        .position(|row| row["event"] == "delivery_claimed")
        .unwrap();
    let callback = rows
        .iter()
        .position(|row| row["event"] == "callback_finished")
        .unwrap();
    assert!(claimed < callback);
    let retired = rows.iter().find(|row| row["event"] == "retired").unwrap();
    assert_eq!(
        retired["terminal"],
        identity(retired_id),
        "an unclaimed owner retirement is not invented for a delivered callback"
    );
    assert_ne!(retired_id, id);
    assert_eq!(
        rows.iter().filter(|row| row["event"] == "closed").count(),
        1
    );
    assert_eq!(probe.tasks(), 0);
}
#[test]
fn absent_idle_probe_is_explicitly_unavailable_not_a_guessed_eof_history() {
    assert_eq!(
        idle_snapshot(None),
        serde_json::json!({"available":false,
        "reason":"session_constructor_has_no_idle_transport_probe"})
    );
    let probe = idle_close::Probe::default();
    probe.record(idle_close::Event::ReadEof(1));
    probe.record(idle_close::Event::Released(1));
    let value = idle_snapshot(Some(&probe));
    assert_eq!(
        value["rows"],
        serde_json::json!([
            {"sequence":0,"event":"ReadEof(1)"},
            {"sequence":1,"event":"Released(1)"}
        ])
    );
    assert!(
        !value.to_string().contains("ReadEofDelivered(1)"),
        "unheld EOF does not fabricate a separately instrumented delivery row"
    );
}

#[test]
fn independent_domains_are_declared_without_a_fabricated_global_sequence() {
    let probe = Probe::default();
    probe.observe_lifecycle();
    let raw = probe.lifecycle_reader();
    let buffers = BufferReader::default();
    buffers.observe_lifecycle();
    let terminal = gateway_terminal::Probe::default();
    let value = snapshot(&raw, &buffers, &terminal, None);
    assert_eq!(
        value["cross_domain_order"],
        "unavailable_do_not_sort_or_merge_elapsed_clocks"
    );
    assert_eq!(value["raw_pool"]["domain"], "raw_pool_probe_mutex");
    assert_eq!(value["producer"]["domain"], "buffer_diagnostic_mutex");
    assert_eq!(value["terminal"]["domain"], "terminal_probe_mutex");
    assert!(value.get("sequence").is_none());
    assert!(serde_json::to_vec(&value).unwrap().len() <= ENCODED_BYTES);
}

#[tokio::test]
async fn diagnostic_overflow_cannot_suppress_original_producer_callback_work() {
    let reader = BufferReader::default();
    reader.observe_lifecycle();
    for _ in 0..16 {
        reader.source_end(None, buffered_body::End::Clean);
    }
    let observed = reader.clone();
    let callback_calls = Arc::new(AtomicUsize::new(0));
    let original = callback_calls.clone();
    let (body, producer, probe) = buffered_body::start(Full::new(Bytes::new()), move |end| {
        observed.source_end(None, end);
        original.fetch_add(1, Ordering::SeqCst);
    });
    producer.join().await.unwrap();
    assert_eq!(
        callback_calls.load(Ordering::SeqCst),
        1,
        "diagnostic overflow cannot short-circuit original callback effects"
    );
    assert_eq!(probe.snapshot().callback_panics, 0);
    drop(body);
    assert_eq!(probe.snapshot().live_pools, 0);
    let value = reader.snapshot();
    assert_eq!(
        value["complete"], false,
        "incomplete history cannot be executable evidence"
    );
    assert_eq!(value["failure"], "observer_invariant");
}

#[tokio::test]
async fn absent_producer_callback_history_cannot_serialize_as_complete() {
    let reader = BufferReader::default();
    reader.observe_lifecycle();
    let (body, producer, probe) = buffered_body::start(Full::new(Bytes::new()), |_| {
        panic!("synthetic original producer callback panic");
    });
    reader.register(None, probe.clone());
    producer.join().await.unwrap();
    assert_eq!(probe.snapshot().callback_panics, 1);
    drop(body);
    assert_eq!(probe.snapshot().live_pools, 0);
    let value = reader.snapshot();
    assert_eq!(
        value["complete"], false,
        "a contained original callback panic cannot turn missing diagnostic history into complete:true"
    );
    assert_eq!(value["failure"], "callback_history_incomplete");
}

#[tokio::test]
async fn equal_history_counts_with_wrong_callback_identity_are_explicitly_incomplete() {
    let reader = BufferReader::default();
    reader.observe_lifecycle();
    let registered = gateway_terminal::Identity {
        connection: 43,
        request: 0,
    };
    let wrong = gateway_terminal::Identity {
        connection: 43,
        request: 1,
    };
    let observed = reader.clone();
    let (body, producer, probe) = buffered_body::start(Full::new(Bytes::new()), move |end| {
        observed.source_end(Some(wrong), end);
    });
    reader.register(Some(registered), probe.clone());
    producer.join().await.unwrap();
    drop(body);
    assert_eq!(probe.snapshot().live_pools, 0);
    assert_eq!(probe.snapshot().callback_panics, 0);
    let value = reader.snapshot();
    assert_eq!(value["registrations"], value["callbacks"]);
    assert_eq!(
        value["complete"], false,
        "equal counts do not prove identity correlation"
    );
    assert_eq!(value["failure"], "callback_identity_mismatch");
}

#[tokio::test]
async fn unowned_callback_identity_reports_incomplete_without_failing_original_cleanup() {
    let raw = Probe::default();
    raw.observe_lifecycle();
    let reader = BufferReader::default();
    reader.observe_lifecycle();
    let observed = reader.clone();
    let (body, producer, probe) = buffered_body::start(Full::new(Bytes::new()), move |end| {
        observed.source_end(None, end);
    });
    reader.register(None, probe.clone());
    producer.join().await.unwrap();
    drop(body);
    let metrics = probe.snapshot();
    assert_eq!(
        (
            metrics.live_pools,
            metrics.producer_tasks,
            metrics.callback_panics
        ),
        (0, 0, 0)
    );
    let value = snapshot(
        &raw.lifecycle_reader(),
        &reader,
        &gateway_terminal::Probe::default(),
        None,
    );
    assert_eq!(value["complete"], false);
    assert_eq!(
        value["producer"]["failure"],
        "unowned_buffer_identity_unavailable"
    );
    assert_eq!(value["producer"]["registrations"], 1);
    assert_eq!(value["producer"]["callbacks"], 1);
}
