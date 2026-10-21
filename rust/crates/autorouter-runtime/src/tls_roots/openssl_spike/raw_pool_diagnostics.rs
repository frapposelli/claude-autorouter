//! Passive scalar history for the original complete replay inventories. Domains
//! have separate locks: their indices are not one global clock or causal order.
use super::super::{buffered_body, gateway_terminal, idle_close};
use super::*;

const REQUESTS: usize = 16;
const RAW_ROWS: usize = 256;
const TERMINAL_ROWS: usize = 512;
const ENCODED_BYTES: usize = 16_384;

#[derive(Clone, Copy)]
pub(super) struct AssignmentObservation {
    pub ordinal: usize,
    pub label: usize,
    pub connection: usize,
    pub terminal: Option<gateway_terminal::Identity>,
    pub idle_request: Option<usize>,
    sequence: usize,
}
impl AssignmentObservation {
    pub(super) fn new(
        ordinal: usize,
        label: usize,
        connection: usize,
        terminal: Option<gateway_terminal::Identity>,
        idle_request: Option<usize>,
    ) -> Self {
        Self {
            ordinal,
            label,
            connection,
            terminal,
            idle_request,
            sequence: 0,
        }
    }
}
// This reader owns only existing scalar rows and scalar identity correlations.
// It never owns Shared, an Entry, a gate, assignment capture or an IO handle.
pub(super) struct RawReader(Arc<Mutex<ProbeState>>);
impl Probe {
    pub(super) fn observe_lifecycle(&self) {
        let mut state = self.state.lock().unwrap();
        assert!(
            state.rows.is_empty() && state.assignments.is_none(),
            "raw lifecycle enabled before requests once"
        );
        state.assignments = Some(Vec::new());
    }
    pub(super) fn lifecycle_reader(&self) -> RawReader {
        RawReader(self.state.clone())
    }
    pub(super) fn record_assignment(&self, mut observation: AssignmentObservation, reused: bool) {
        let row = serde_json::json!({"event":"assigned","request":observation.label,
            "connection":observation.connection,"reused":reused,
            "ms":u64::try_from(self.start.elapsed().as_millis()).unwrap()});
        let mut state = self.state.lock().unwrap();
        assert!(state.rows.len() < 1024, "raw pool trace bound");
        let sequence = state.rows.len();
        let mut failed = false;
        if let Some(assignments) = &mut state.assignments {
            if assignments.len() >= REQUESTS
                || observation.ordinal >= REQUESTS
                || observation.label >= REQUESTS
                || !(1..=8).contains(&observation.connection)
                || assignments
                    .iter()
                    .any(|old| old.ordinal == observation.ordinal)
            {
                // Diagnostic limits never replace the real request operation.
                // Retain the failure for final rejection without growing history.
                failed = true;
            } else {
                observation.sequence = sequence;
                assignments.push(observation);
            }
        }
        state.lifecycle_failed |= failed;
        // Exactly the original assigned shape, with no new fields in pool_trace.
        state.rows.push(row);
    }
}
fn identity(value: gateway_terminal::Identity) -> Trace {
    serde_json::json!({"downstream_connection":value.connection,"downstream_request":value.request})
}
impl RawReader {
    fn snapshot(&self) -> Trace {
        let (rows, assignments, failed) = {
            let state = self.0.lock().unwrap();
            assert!(state.rows.len() <= RAW_ROWS, "raw lifecycle row bound");
            let assignments = state
                .assignments
                .as_ref()
                .expect("raw lifecycle enabled before requests");
            assert!(
                assignments.len() <= REQUESTS,
                "raw lifecycle assignment bound"
            );
            (
                state.rows.clone(),
                assignments.clone(),
                state.lifecycle_failed,
            )
        };
        assert!(!failed, "raw lifecycle correlation failure");
        let ordered: Vec<_> = rows
            .iter()
            .enumerate()
            .map(|(sequence, row)| {
                let event = row["event"].as_str().expect("raw lifecycle event");
                let expected: &[&str] = match event {
                    "assigned" => &["connection", "event", "ms", "request", "reused"],
                    "consumer_complete" => &[
                        "clean",
                        "connection",
                        "event",
                        "ms",
                        "sender_closed",
                        "sender_ready",
                        "unpoisoned",
                    ],
                    "free" => &["connection", "event", "kept", "ms", "timeout"],
                    "close" => &["connection", "event", "ms"],
                    "driver_finished" => &["connection", "event", "ms", "ok"],
                    "timeout" if row.get("destroyed").is_some() => {
                        &["connection", "destroyed", "event", "free", "ms"]
                    }
                    "timeout" => &["connection", "event", "free", "ms"],
                    _ => panic!("raw lifecycle event"),
                };
                let fields = row.as_object().expect("raw lifecycle object");
                assert_eq!(
                    fields.len(),
                    expected.len(),
                    "raw lifecycle closed field set"
                );
                assert!(
                    expected.iter().all(|key| fields.contains_key(*key)),
                    "raw lifecycle closed field set"
                );
                assert!(
                    row["connection"]
                        .as_u64()
                        .is_some_and(|id| (1..=8).contains(&id)),
                    "raw lifecycle connection bound"
                );
                for (key, value) in fields {
                    match key.as_str() {
                        "event" => {}
                        "connection" | "ms" | "request" | "timeout" => {
                            assert!(value.as_u64().is_some(), "raw lifecycle numeric field");
                        }
                        _ => assert!(value.as_bool().is_some(), "raw lifecycle boolean field"),
                    }
                }
                let mut value = row.clone();
                value["sequence"] = sequence.into();
                value
            })
            .collect();
        let assignment_rows: Vec<_> = assignments
            .iter()
            .map(|item| {
                let row = &rows[item.sequence];
                assert_eq!(
                    row["event"], "assigned",
                    "raw lifecycle assignment sequence"
                );
                assert_eq!(row["request"], item.label, "raw lifecycle assignment label");
                assert_eq!(
                    row["connection"], item.connection,
                    "raw lifecycle assignment connection"
                );
                serde_json::json!({"raw_sequence":item.sequence,"raw_request":item.ordinal,
                "raw_label":item.label,"raw_connection":item.connection,
                "terminal":item.terminal.map(identity),"idle_request":item.idle_request})
            })
            .collect();
        assert_eq!(
            rows.iter().filter(|row| row["event"] == "assigned").count(),
            assignments.len(),
            "raw lifecycle complete assignment correlation"
        );
        serde_json::json!({"available":true,"complete":true,"domain":"raw_pool_probe_mutex",
            "sequence":"zero_based_insertion_order_in_existing_raw_rows",
            "rows":ordered,"assignments":assignment_rows,
            "raw_request_numbering":"zero_based_raw_request_ordinal",
            "raw_label_numbering":"existing_assigned_request_label",
            "raw_connection_numbering":"successful_raw_http1_connection",
            "consumer_complete":"local_body_completion_not_downstream_delivery",
            "close":"connection_driver_RAII_lease_release_not_normal_return_proof",
            "close_coverage":"only_recorded_when_weak_pool_owner_upgrades_missing_close_during_pool_destruction_is_not_a_live_driver_proof"})
    }
}

#[derive(Default)]
struct BufferState {
    enabled: bool,
    failed: bool,
    pools: Vec<(Option<gateway_terminal::Identity>, buffered_body::Probe)>,
    ends: Vec<(Option<gateway_terminal::Identity>, buffered_body::End)>,
}
// buffered_body::Probe retains scalar metrics and a Weak<Shared>. No producer,
// body, block, queue, RequestTerminal, cancellation token or admission is owned.
#[derive(Clone, Default)]
pub(super) struct BufferReader(Arc<Mutex<BufferState>>);
impl BufferReader {
    pub(super) fn observe_lifecycle(&self) {
        let mut state = self.0.lock().unwrap();
        assert!(
            !state.enabled && state.pools.is_empty() && state.ends.is_empty(),
            "buffer lifecycle enabled before requests once"
        );
        state.enabled = true;
    }
    pub(super) fn register(
        &self,
        owner: Option<gateway_terminal::Identity>,
        probe: buffered_body::Probe,
    ) {
        let mut state = self.0.lock().unwrap();
        if !state.enabled {
            return;
        }
        if state.pools.len() >= REQUESTS
            || owner.is_some_and(|id| state.pools.iter().any(|(old, _)| *old == Some(id)))
        {
            state.failed = true;
            return;
        }
        state.pools.push((owner, probe));
    }
    pub(super) fn source_end(
        &self,
        owner: Option<gateway_terminal::Identity>,
        end: buffered_body::End,
    ) {
        let mut state = self.0.lock().unwrap();
        if !state.enabled {
            return;
        }
        if state.ends.len() >= REQUESTS
            || owner.is_some_and(|id| state.ends.iter().any(|(old, _)| *old == Some(id)))
        {
            // The original producer failure callback still runs. Fail closed
            // only when the fixture serializes its separate diagnostic field.
            state.failed = true;
            return;
        }
        state.ends.push((owner, end));
    }
    fn snapshot(&self) -> Trace {
        let (pools, ends, failed) = {
            let state = self.0.lock().unwrap();
            assert!(state.enabled, "buffer lifecycle enabled before requests");
            (state.pools.clone(), state.ends.clone(), state.failed)
        };
        // Incomplete diagnostic identity/history never changes an original
        // request. The owner rejects this separate field as evidence instead.
        let mut failure = failed.then_some("observer_invariant");
        if pools.len() != ends.len() {
            failure.get_or_insert("callback_history_incomplete");
        }
        if pools.iter().any(|(owner, _)| owner.is_none())
            || ends.iter().any(|(owner, _)| owner.is_none())
        {
            failure.get_or_insert("unowned_buffer_identity_unavailable");
        }
        for (owner, _) in &pools {
            if owner.is_some() && ends.iter().filter(|row| row.0 == *owner).count() != 1 {
                failure.get_or_insert("callback_identity_mismatch");
            }
        }
        let buffers: Vec<_> = pools.iter().enumerate().map(|(index, (owner, probe))| {
            let current = probe.snapshot();
            if current.producer_tasks != 0 {
                failure.get_or_insert("producer_callback_not_settled");
            }
            if current.callback_panics != 0 {
                failure.get_or_insert("producer_callback_panicked");
            }
            serde_json::json!({"registration":index,"terminal":owner.map(identity),
                "live_pools":current.live_pools,"queued_bytes":current.queued_bytes,
                "queued_blocks":current.queued_blocks,"allocated_blocks":current.allocated_blocks,
                "outstanding_blocks":current.outstanding_blocks,
                "peak_outstanding_blocks":current.peak_outstanding_blocks,
                "peak_queued_bytes":current.peak_queued_bytes,"producer_tasks":current.producer_tasks,
                "consumer_polls":current.consumer_polls,"callback_panics":current.callback_panics,
                "waker_panics":current.waker_panics,
                "current_end":current.end.map(end_name)})
        }).collect();
        let rows: Vec<_> = ends
            .iter()
            .enumerate()
            .map(|(sequence, (owner, end))| {
                serde_json::json!({"sequence":sequence,"event":"producer_terminal_callback",
                "terminal":owner.map(identity),"end":end_name(*end)})
            })
            .collect();
        serde_json::json!({"available":true,"complete":failure.is_none(),"failure":failure,
            "domain":"buffer_diagnostic_mutex","registrations":pools.len(),"callbacks":ends.len(),
            "identity_correlation":"exact_terminal_identity_only_unowned_callbacks_are_not_guessed",
            "sequence":"zero_based_callback_observation_order_only",
            "rows":rows,"buffers":buffers,
            "buffer_sampling":"cleanup_snapshot_not_callback_time_or_atomic_across_metrics",
            "registration_order":"separate_from_callback_order_producer_may_finish_before_registration",
            "producer_terminal":"after_source_end_publication_and_original_terminal_callback_stage_not_initial_clean_queue_publication",
            "current_end_after_pool_release":"unavailable_weak_source_expired_retained_callback_end_is_separate"})
    }
}
fn end_name(end: buffered_body::End) -> &'static str {
    match end {
        buffered_body::End::Clean => "clean",
        buffered_body::End::SourceError => "source_error",
        buffered_body::End::InvalidMetadata => "invalid_metadata",
        buffered_body::End::Cancelled => "cancelled",
    }
}
fn terminal_rows(events: &[gateway_terminal::Event]) -> Vec<Trace> {
    use gateway_terminal::Event;
    // The old probe retains a 1024-row suffix. A diagnostic rejects even the
    // pre-suffix range above 512; overwritten history can never pass this bound.
    assert!(
        events.len() <= TERMINAL_ROWS,
        "terminal lifecycle complete-history bound"
    );
    events
        .iter()
        .enumerate()
        .map(|(sequence, event)| {
            let (name, owner, detail) = match event {
                Event::Registered(id) => ("registered", Some(*id), None),
                Event::Attached(id) => ("attached", Some(*id), None),
                Event::Failed(id, cause) => ("failed", Some(*id), Some(format!("{cause:?}"))),
                Event::DeliveryClaimed(id, delivery) => {
                    ("delivery_claimed", Some(*id), Some(format!("{delivery:?}")))
                }
                Event::CallbackFinished(id) => ("callback_finished", Some(*id), None),
                Event::CallbackAbandoned(id) => ("callback_abandoned", Some(*id), None),
                Event::Retired(id) => ("retired", Some(*id), None),
                Event::ConnectionClaimed(id) => ("connection_claimed", Some(*id), None),
                Event::Closed(_) => ("closed", None, None),
                Event::WriteHeld(_) => ("write_held", None, None),
            };
            let connection = match event {
                Event::Closed(id) | Event::WriteHeld(id) => *id,
                _ => owner.unwrap().connection,
            };
            serde_json::json!({"sequence":sequence,"event":name,"terminal":owner.map(identity),
            "downstream_connection":connection,"detail":detail})
        })
        .collect()
}
fn terminal_snapshot(probe: &gateway_terminal::Probe) -> Trace {
    assert!(
        !probe.observer_failed(),
        "terminal lifecycle observer failed"
    );
    let rows = terminal_rows(&probe.events());
    let actions: Vec<_> = probe
        .transport_events()
        .iter()
        .enumerate()
        .map(|(sequence, action)| {
            serde_json::json!({"sequence":sequence,"terminal":identity(action.identity),
            "cause":format!("{:?}",action.cause),"outcome":format!("{:?}",action.outcome)})
        })
        .collect();
    assert!(actions.len() <= 128, "terminal lifecycle transport bound");
    serde_json::json!({"available":true,"complete":true,"domain":"terminal_probe_mutex",
        "sequence":"zero_based_existing_event_insertion_order",
        "rows":rows,"transport_actions":actions,
        "transport_action_sequence":"separate_vector_order_not_interleaved_with_terminal_events",
        "request_numbering":"zero_based_per_downstream_connection",
        "connection_numbering":"process_unique_terminal_generation_not_upstream_physical_id"})
}
fn idle_snapshot(probe: Option<&idle_close::Probe>) -> Trace {
    let Some(probe) = probe else {
        return serde_json::json!({"available":false,
            "reason":"session_constructor_has_no_idle_transport_probe"});
    };
    let rows: Vec<_> = probe.rows().iter().enumerate().map(|(sequence, event)| {
        serde_json::json!({"sequence":sequence,"event":format!("{event:?}")})
    }).collect();
    serde_json::json!({"available":true,"complete":true,"domain":"idle_probe_mutex",
        "sequence":"zero_based_existing_idle_event_insertion_order",
        "rows":rows,"assignment_history":probe.assignment_history(),
        "request_numbering":"one_based_idle_capture_registration",
        "connection_numbering":"one_based_connected_transport_ordinal",
        "eof_semantics":"ReadEof_is_positive_capacity_zero_read_delivery_boundary;_held_EOF_controls_add_UnderlyingReadEofObserved_and_ReadEofDelivered;_no_TCP_FIN_or_TLS_close_notify_inference",
        "release_semantics":"Released_is_transport_observer_RAII_drop_not_raw_driver_return",
        "assignment_sampling":"capture_history_at_cleanup_not_an_event_order"})
}
pub(super) fn snapshot(
    raw: &RawReader,
    buffers: &BufferReader,
    terminal: &gateway_terminal::Probe,
    idle: Option<&idle_close::Probe>,
) -> Trace {
    let producer = buffers.snapshot();
    let complete = producer["complete"].as_bool().unwrap();
    let result = serde_json::json!({"schema_version":1,"available":true,"complete":complete,
        "cross_domain_order":"unavailable_do_not_sort_or_merge_elapsed_clocks",
        "raw_pool":raw.snapshot(),"producer":producer,
        "terminal":terminal_snapshot(terminal),"idle":idle_snapshot(idle)});
    assert!(
        serde_json::to_vec(&result).unwrap().len() <= ENCODED_BYTES,
        "lifecycle diagnostic encoded byte bound"
    );
    result
}

#[cfg(test)]
#[path = "raw_pool_diagnostics_tests.rs"]
mod tests;
