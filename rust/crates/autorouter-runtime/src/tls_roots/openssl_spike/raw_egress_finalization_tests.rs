//! Separate operation-origin controls; inherits the existing scripted fixture.
//! These are lower-adapter faults, not fabricated SSL errors or real TLS proof.
use super::*;

fn application_fence(io: &mut HandshakeIo<Script>, state: &Arc<Mutex<Observed>>) {
    write(io, b"application");
    assert!(matches!(
        io.poll_egress_flush(&mut cx()),
        Poll::Ready(Ok(()))
    ));
    assert_eq!(state.lock().unwrap().output, b"application");
    assert_eq!(io.egress_queued_bytes(), 0);
    assert!(!io.tls_finalization_started());
    io.begin_tls_finalization();
    assert!(io.tls_finalization_started());
    assert!(!io.finalization_write_failed());
    assert!(!io.shutdown_entered());
}

#[test]
fn finalization_cipher_write_error_is_tagged_and_discarded_before_tcp() {
    let (mut io, state) = fixture();
    application_fence(&mut io, &state);
    write(&mut io, b"notify");
    let payload = Arc::new(());
    state.lock().unwrap().writes =
        [WriteStep::Error(io::Error::other(Payload(payload.clone())))].into();
    let Poll::Ready(Err(error)) = Pin::new(&mut io).poll_shutdown(&mut cx()) else {
        panic!("missing actual scripted ciphertext write error");
    };
    same_payload(&error, &payload);
    assert!(io.tls_finalization_started() && io.finalization_write_failed());
    assert!(io.lower_shutdown_started());
    assert!(!io.shutdown_entered());
    assert_eq!(io.egress_queued_bytes(), b"notify".len());
    assert_eq!(state.lock().unwrap().output, b"application");

    io.discard_failed_finalization();
    assert!(io.tls_finalization_started());
    assert!(
        !io.finalization_write_failed(),
        "discard consumes only this failure provenance"
    );
    assert_eq!(io.egress_queued_bytes(), 0);
    assert_eq!(io.written, 0);
    assert!(matches!(
        Pin::new(&mut io).poll_shutdown(&mut cx()),
        Poll::Ready(Ok(()))
    ));
    assert!(
        io.shutdown_entered(),
        "real raw shutdown follows explicit finalization-only discard"
    );
    assert_eq!(state.lock().unwrap().output, b"application");
    assert_eq!(
        state
            .lock()
            .unwrap()
            .events
            .iter()
            .filter(|event| **event == "shutdown")
            .count(),
        1
    );
}

#[test]
fn application_cipher_error_is_never_finalization_and_cannot_be_discarded() {
    let (mut io, state) = fixture();
    write(&mut io, b"application");
    let payload = Arc::new(());
    state.lock().unwrap().writes = [
        WriteStep::Accept(2),
        WriteStep::Error(io::Error::other(Payload(payload.clone()))),
    ]
    .into();
    let Poll::Ready(Err(error)) = io.poll_egress_drain(&mut cx()) else {
        panic!("missing actual scripted application drain error");
    };
    same_payload(&error, &payload);
    assert!(!io.tls_finalization_started() && !io.finalization_write_failed());
    assert!(!io.shutdown_entered());
    let before = io.egress_queued_bytes();
    assert_eq!(before, b"application".len() - 2);
    let rejected = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        io.discard_failed_finalization();
    }));
    assert!(
        rejected.is_err(),
        "application output cannot use finalization discard"
    );
    assert_eq!(io.egress_queued_bytes(), before);
    assert_eq!(state.lock().unwrap().output, b"ap");
}

#[test]
fn finalization_flush_tcp_and_invalid_count_failures_stay_untagged() {
    for stage in ["flush", "shutdown"] {
        let (mut io, state) = fixture();
        application_fence(&mut io, &state);
        write(&mut io, b"notify");
        let payload = Arc::new(());
        let error = io::Error::other(Payload(payload.clone()));
        match stage {
            "flush" => state.lock().unwrap().flushes = [Step::Error(error)].into(),
            "shutdown" => state.lock().unwrap().shutdowns = [Step::Error(error)].into(),
            _ => unreachable!(),
        }
        let Poll::Ready(Err(error)) = Pin::new(&mut io).poll_shutdown(&mut cx()) else {
            panic!("missing lower {stage} error");
        };
        same_payload(&error, &payload);
        assert!(io.tls_finalization_started() && io.lower_shutdown_started());
        assert!(
            !io.finalization_write_failed(),
            "only actual cipher-write Err may carry the tag"
        );
        assert_eq!(io.shutdown_entered(), stage == "shutdown");
        assert_eq!(io.egress_queued_bytes(), 0);
        assert_eq!(state.lock().unwrap().output, b"applicationnotify");
        let before = state.lock().unwrap().events.clone();
        drop(io);
        assert_eq!(
            state.lock().unwrap().events,
            before,
            "Drop cannot retry the lower error"
        );
    }
    for count in [0, 7] {
        let (mut io, state) = fixture();
        application_fence(&mut io, &state);
        write(&mut io, b"notify");
        state.lock().unwrap().writes = if count == 0 {
            [WriteStep::Accept(0)].into()
        } else {
            [WriteStep::Oversized(count)].into()
        };
        let Poll::Ready(Err(error)) = Pin::new(&mut io).poll_shutdown(&mut cx()) else {
            panic!("invalid count became finalization success");
        };
        assert_eq!(
            error.kind(),
            if count == 0 {
                io::ErrorKind::WriteZero
            } else {
                io::ErrorKind::InvalidData
            }
        );
        assert!(
            !io.finalization_write_failed(),
            "synthetic count failure is not actual inner write Err"
        );
        assert!(!io.shutdown_entered());
        assert_eq!(io.egress_queued_bytes(), b"notify".len());
        assert_eq!(state.lock().unwrap().output, b"application");
    }
}

#[test]
fn finalization_partial_pending_keeps_fence_then_discards_only_unsent_suffix() {
    let (mut io, state) = fixture();
    application_fence(&mut io, &state);
    write(&mut io, b"notify");
    state.lock().unwrap().writes = [WriteStep::Accept(2), WriteStep::Pending].into();
    assert!(Pin::new(&mut io).poll_shutdown(&mut cx()).is_pending());
    assert!(io.tls_finalization_started() && io.lower_shutdown_started());
    assert!(!io.finalization_write_failed() && !io.shutdown_entered());
    assert_eq!(io.egress_queued_bytes(), 4);
    assert_eq!(state.lock().unwrap().output, b"applicationno");

    // Continue the lower phase. Do not call application flush or begin again:
    // that would misclassify finalization bytes on the next outer poll.
    let payload = Arc::new(());
    state.lock().unwrap().writes =
        [WriteStep::Error(io::Error::other(Payload(payload.clone())))].into();
    let Poll::Ready(Err(error)) = Pin::new(&mut io).poll_shutdown(&mut cx()) else {
        panic!("missing finalization continuation error");
    };
    same_payload(&error, &payload);
    assert!(io.finalization_write_failed() && !io.shutdown_entered());
    io.discard_failed_finalization();
    assert_eq!(io.egress_queued_bytes(), 0);
    assert_eq!(io.written, 0);
    assert!(!io.finalization_write_failed());
    assert!(matches!(
        Pin::new(&mut io).poll_shutdown(&mut cx()),
        Poll::Ready(Ok(()))
    ));
    assert!(io.shutdown_entered());
    assert_eq!(
        state.lock().unwrap().output,
        b"applicationno",
        "no duplicate prefix or replayed discarded suffix"
    );
}

#[test]
fn consumed_write_tag_does_not_absorb_a_later_physical_flush_error() {
    let (mut io, state) = fixture();
    application_fence(&mut io, &state);
    write(&mut io, b"notify");
    let first = Arc::new(());
    state.lock().unwrap().writes =
        [WriteStep::Error(io::Error::other(Payload(first.clone())))].into();
    let Poll::Ready(Err(error)) = Pin::new(&mut io).poll_shutdown(&mut cx()) else {
        panic!("missing first write error");
    };
    same_payload(&error, &first);
    assert!(io.finalization_write_failed());
    io.discard_failed_finalization();
    let later = Arc::new(());
    state.lock().unwrap().flushes = [Step::Error(io::Error::other(Payload(later.clone())))].into();
    let Poll::Ready(Err(error)) = Pin::new(&mut io).poll_shutdown(&mut cx()) else {
        panic!("missing actual later flush error");
    };
    same_payload(&error, &later);
    assert!(
        !io.finalization_write_failed(),
        "consumed write provenance cannot label a flush error"
    );
    assert!(
        !io.shutdown_entered(),
        "flush failure still precedes TCP entry but remains fatal"
    );
    assert_eq!(state.lock().unwrap().output, b"application");
}
