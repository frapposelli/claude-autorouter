//! Request-owned producer tickets. Observations never grant delivery or pool authority.
use super::*;
use hyper::ext::{NodeHttpBodyHandoff, NodeHttpBodyHandoffEvent as Observation, NodeHttpFlushPoll};
use std::future::{Future, poll_fn};
use tokio::sync::oneshot;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PauseKind {
    Body,
    Response,
}
impl PauseKind {
    fn index(self) -> usize {
        match self {
            Self::Body => 0,
            Self::Response => 1,
        }
    }
}
#[derive(Clone, Copy, Default, Debug, Eq, PartialEq)]
enum Phase {
    #[default]
    Awaiting,
    Queued,
    Active,
    WriterPending,
}
#[derive(Clone, Copy, Debug)]
enum Evidence {
    Submitted,
    Gate { index: usize, epoch: u64 },
    Phase { phase: Phase, epoch: u64 },
}
#[derive(Clone, Copy, Debug)]
pub(super) struct Acknowledgment {
    epoch: u64,
    sequence: u64,
    target: u64,
    evidence: Evidence,
}
pub(super) type Sender = oneshot::Sender<Result<Acknowledgment, ()>>;
type Ready = (Sender, Acknowledgment);
struct Pending {
    epoch: u64,
    sequence: u64,
    target: u64,
    sender: Sender,
}
struct ProducerState {
    epoch: u64,
    stop: CancellationToken,
    sealed: bool,
    published: u64,
    next_sequence: u64,
    pending: Option<Pending>,
}
#[derive(Default)]
pub(super) struct HandoffState {
    producer: Option<ProducerState>,
    next_producer: u64,
    next_pause: u64,
    pauses: [Option<u64>; 2],
    phase: Phase,
    phase_epoch: u64,
    submitted: u64,
    attempted: u64,
}
#[derive(Clone, Debug, serde::Serialize)]
pub(crate) struct HandoffSnapshot {
    pub producer_epoch: Option<u64>,
    pub published: u64,
    pub submitted: u64,
    pub attempted: u64,
    pub phase: &'static str,
    pub phase_epoch: u64,
}
impl HandoffState {
    pub(super) fn snapshot(&self) -> HandoffSnapshot {
        HandoffSnapshot {
            producer_epoch: self.producer.as_ref().map(|p| p.epoch),
            published: self.producer.as_ref().map_or(0, |p| p.published),
            submitted: self.submitted,
            attempted: self.attempted,
            phase: match self.phase {
                Phase::Awaiting => "awaiting",
                Phase::Queued => "queued",
                Phase::Active => "active",
                Phase::WriterPending => "writer_pending",
            },
            phase_epoch: self.phase_epoch,
        }
    }
    pub(super) fn live_producer(&self, epoch: u64) -> bool {
        self.producer
            .as_ref()
            .is_some_and(|p| p.epoch == epoch && !p.sealed && !p.stop.is_cancelled())
    }
    pub(super) fn take_sender(&mut self) -> Option<Sender> {
        self.producer.as_mut()?.pending.take().map(|p| p.sender)
    }
    pub(super) fn stop(&self) -> Option<CancellationToken> {
        self.producer.as_ref().map(|p| p.stop.clone())
    }
    fn covered(&self, target: u64) -> bool {
        target <= self.submitted && target <= self.attempted
    }
    fn evidence(&self, target: u64) -> Option<Evidence> {
        if self.covered(target) {
            return Some(Evidence::Submitted);
        }
        if let Some((index, epoch)) = self
            .pauses
            .iter()
            .enumerate()
            .find_map(|(index, epoch)| epoch.map(|epoch| (index, epoch)))
        {
            return Some(Evidence::Gate { index, epoch });
        }
        matches!(self.phase, Phase::Queued | Phase::WriterPending).then_some(Evidence::Phase {
            phase: self.phase,
            epoch: self.phase_epoch,
        })
    }
    fn current(&self, acknowledgment: Acknowledgment) -> bool {
        self.covered(acknowledgment.target)
            || match acknowledgment.evidence {
                Evidence::Submitted => false,
                Evidence::Gate { index, epoch } => self.pauses[index] == Some(epoch),
                Evidence::Phase { phase, epoch } => {
                    self.phase == phase && self.phase_epoch == epoch
                }
            }
    }
    fn phase(&mut self, phase: Phase) -> Result<(), ()> {
        if self.phase != phase {
            self.phase_epoch = self.phase_epoch.checked_add(1).ok_or(())?;
            self.phase = phase;
        }
        Ok(())
    }
    fn ready_sender(&mut self) -> Option<Ready> {
        let producer = self.producer.as_ref()?;
        let pending = producer.pending.as_ref()?;
        if producer.sealed || producer.stop.is_cancelled() || pending.epoch != producer.epoch {
            return None;
        }
        let acknowledgment = Acknowledgment {
            epoch: pending.epoch,
            sequence: pending.sequence,
            target: pending.target,
            evidence: self.evidence(pending.target)?,
        };
        Some((self.take_sender()?, acknowledgment))
    }
}
pub(super) fn complete_sender(
    sender: Option<Sender>,
    result: Result<Acknowledgment, ()>,
    terminal: Option<&RequestTerminal>,
) {
    if let Some(sender) = sender
        && std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = sender.send(result);
        }))
        .is_err()
        && let Some(terminal) = terminal
    {
        terminal.fail(FailureCause::Cancelled);
    }
}
fn complete_ready(ready: Option<Ready>, terminal: &RequestTerminal) {
    if let Some((sender, acknowledgment)) = ready {
        complete_sender(Some(sender), Ok(acknowledgment), Some(terminal));
    }
}
pub(super) fn retire_record(mut record: Record) {
    let pending = record.handoff.take_sender();
    let stop = record.handoff.stop();
    if let Some(stop) = stop {
        safe_cancel(&stop);
    }
    complete_sender(pending, Err(()), None);
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(record)));
}
fn live(state: &mut State, identity: Identity, generation: u64) -> Option<&mut Record> {
    if identity.connection != generation || state.closed || state.close_latch.is_some() {
        return None;
    }
    state
        .records
        .get_mut(&identity.request)
        .filter(|r| r.failure.is_none())
}
impl RequestTerminal {
    pub(crate) fn producer(&self, stop: CancellationToken) -> Result<DataProducerOwner, ()> {
        let shared = self.shared.upgrade().ok_or(())?;
        let epoch = {
            let mut state = shared.state.lock().unwrap();
            let record = live(&mut state, self.identity, shared.generation).ok_or(())?;
            if record.handoff.producer.is_some()
                || stop.is_cancelled()
                || shared.stop.is_cancelled()
            {
                return Err(());
            }
            let epoch = record.handoff.next_producer.checked_add(1).ok_or(())?;
            record.handoff.next_producer = epoch;
            record.handoff.producer = Some(ProducerState {
                epoch,
                stop: stop.clone(),
                sealed: false,
                published: 0,
                next_sequence: 0,
                pending: None,
            });
            epoch
        };
        Ok(DataProducerOwner {
            terminal: self.clone(),
            epoch,
            stop,
            connection_stop: shared.stop.clone(),
            sealed: false,
        })
    }
    pub(crate) fn pause(&self, kind: PauseKind) -> Result<PauseLease, ()> {
        let (lease, sender) = self.pause_silent(kind)?;
        complete_ready(sender, self);
        Ok(lease)
    }
    fn pause_silent(&self, kind: PauseKind) -> Result<(PauseLease, Option<Ready>), ()> {
        let shared = self.shared.upgrade().ok_or(())?;
        let (epoch, sender) = {
            let mut state = shared.state.lock().unwrap();
            let record = live(&mut state, self.identity, shared.generation).ok_or(())?;
            let h = &mut record.handoff;
            if h.pauses[kind.index()].is_some() {
                return Err(());
            }
            let epoch = h.next_pause.checked_add(1).ok_or(())?;
            h.next_pause = epoch;
            h.pauses[kind.index()] = Some(epoch);
            (epoch, h.ready_sender())
        };
        Ok((
            PauseLease {
                terminal: self.clone(),
                kind,
                epoch,
            },
            sender,
        ))
    }
    pub(crate) fn handoff_observer(&self) -> NodeHttpBodyHandoff {
        let terminal = self.clone();
        // The epoch is captured once for this response. Repeated old observers
        // cannot attach themselves to a replacement publication lifetime.
        let epoch = self.shared.upgrade().and_then(|shared| {
            shared
                .state
                .lock()
                .unwrap()
                .records
                .get(&self.identity.request)
                .and_then(|r| r.handoff.producer.as_ref().map(|p| p.epoch))
        });
        NodeHttpBodyHandoff::new(move |event| {
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                terminal.observe_handoff(epoch, event)
            }))
            .is_err()
            {
                terminal.fail_handoff(epoch);
            }
        })
    }
    fn fail_handoff(&self, epoch: Option<u64>) {
        if let (Some(shared), Some(epoch)) = (self.shared.upgrade(), epoch) {
            shared.fail_owned(self.identity, FailureCause::Cancelled, Some(epoch));
        }
    }
    fn observe_handoff(&self, epoch: Option<u64>, event: Observation) {
        if matches!(
            event,
            Observation::FlushPolled {
                outcome: NodeHttpFlushPoll::Failed,
                ..
            }
        ) {
            // Physical TerminalIo failure, if present, already won first cause.
            // This fallback must never pass through the positive ack path.
            self.fail_handoff(epoch);
            return;
        }
        let Some(shared) = self.shared.upgrade() else {
            return;
        };
        let result = (|| {
            let mut state = shared.state.lock().unwrap();
            let Some(record) = live(&mut state, self.identity, shared.generation) else {
                return Ok(None);
            };
            if shared.stop.is_cancelled() {
                return Ok(None);
            }
            let h = &mut record.handoff;
            let Some(producer) = &h.producer else {
                return Ok(None);
            };
            if Some(producer.epoch) != epoch || producer.sealed || producer.stop.is_cancelled() {
                return Ok(None);
            }
            match event {
                Observation::Queued => {
                    h.phase(Phase::Queued)?;
                }
                Observation::Active => {
                    h.phase(Phase::Active)?;
                }
                Observation::DataSubmitted {
                    input_bytes,
                    total_input_bytes,
                } => {
                    if input_bytes == 0
                        || h.submitted
                            .checked_add(u64::try_from(input_bytes).map_err(|_| ())?)
                            != Some(total_input_bytes)
                    {
                        return Err(());
                    }
                    h.submitted = total_input_bytes;
                }
                Observation::FlushPolled {
                    total_input_bytes,
                    outcome,
                } => {
                    if total_input_bytes > h.submitted || total_input_bytes < h.attempted {
                        return Err(());
                    }
                    h.attempted = total_input_bytes;
                    h.phase(match outcome {
                        NodeHttpFlushPoll::Ready => Phase::Active,
                        NodeHttpFlushPoll::Pending => Phase::WriterPending,
                        NodeHttpFlushPoll::Failed => unreachable!(),
                    })?;
                }
            }
            Ok(h.ready_sender())
        })();
        match result {
            Ok(sender) => complete_ready(sender, self),
            Err(()) => {
                self.fail_handoff(epoch);
            }
        }
    }
}
pub(crate) struct PauseLease {
    terminal: RequestTerminal,
    kind: PauseKind,
    epoch: u64,
}
impl Drop for PauseLease {
    fn drop(&mut self) {
        let Some(shared) = self.terminal.shared.upgrade() else {
            return;
        };
        let mut state = shared.state.lock().unwrap();
        if self.terminal.identity.connection != shared.generation {
            return;
        }
        if let Some(record) = state.records.get_mut(&self.terminal.identity.request) {
            let slot = &mut record.handoff.pauses[self.kind.index()];
            if *slot == Some(self.epoch) {
                *slot = None;
            }
        }
    }
}
pub(crate) struct DataProducerOwner {
    terminal: RequestTerminal,
    epoch: u64,
    stop: CancellationToken,
    connection_stop: CancellationToken,
    sealed: bool,
}
impl DataProducerOwner {
    pub(crate) fn ticket(&self, bytes: usize) -> Result<HandoffTicket, ()> {
        let (sender, receiver) = oneshot::channel();
        let mut sender = Some(sender);
        let shared = self.terminal.shared.upgrade().ok_or(())?;
        let registration = (|| {
            let mut state = shared.state.lock().unwrap();
            let record = live(&mut state, self.terminal.identity, shared.generation).ok_or(())?;
            let h = &mut record.handoff;
            let producer = h.producer.as_mut().ok_or(())?;
            if self.sealed
                || producer.sealed
                || producer.epoch != self.epoch
                || producer.stop.is_cancelled()
                || shared.stop.is_cancelled()
                || producer.pending.is_some()
                || bytes == 0
            {
                return Err(());
            }
            let target = producer
                .published
                .checked_add(u64::try_from(bytes).map_err(|_| ())?)
                .ok_or(())?;
            let sequence = producer.next_sequence.checked_add(1).ok_or(())?;
            producer.published = target;
            producer.next_sequence = sequence;
            producer.pending = Some(Pending {
                epoch: self.epoch,
                sequence,
                target,
                sender: sender.take().unwrap(),
            });
            Ok((sequence, target, h.ready_sender()))
        })();
        let (sequence, target, ready) = registration?;
        complete_ready(ready, &self.terminal);
        Ok(HandoffTicket {
            terminal: self.terminal.clone(),
            epoch: self.epoch,
            sequence,
            target,
            acknowledgment: None,
            receiver,
            stop_wait: Box::pin(self.stop.clone().cancelled_owned()),
            connection_wait: Box::pin(self.connection_stop.clone().cancelled_owned()),
            stop: self.stop.clone(),
            connection_stop: self.connection_stop.clone(),
        })
    }
    pub(crate) fn valid(&self) -> bool {
        !self.sealed
            && valid(
                &self.terminal,
                self.epoch,
                &self.stop,
                &self.connection_stop,
            )
    }
    pub(crate) fn seal(&mut self) {
        if self.sealed {
            return;
        }
        self.sealed = true;
        let sender = self.terminal.shared.upgrade().and_then(|shared| {
            let mut state = shared.state.lock().unwrap();
            let record = state.records.get_mut(&self.terminal.identity.request)?;
            let p = record.handoff.producer.as_mut()?;
            if p.epoch != self.epoch {
                return None;
            }
            p.sealed = true;
            record.handoff.take_sender()
        });
        complete_sender(sender, Err(()), Some(&self.terminal));
    }
}
impl Drop for DataProducerOwner {
    fn drop(&mut self) {
        self.seal();
    }
}
fn valid(
    terminal: &RequestTerminal,
    epoch: u64,
    stop: &CancellationToken,
    connection_stop: &CancellationToken,
) -> bool {
    if stop.is_cancelled() || connection_stop.is_cancelled() {
        return false;
    }
    terminal.shared.upgrade().is_some_and(|shared| {
        let mut state = shared.state.lock().unwrap();
        live(&mut state, terminal.identity, shared.generation).is_some_and(|r| {
            r.handoff
                .producer
                .as_ref()
                .is_some_and(|p| p.epoch == epoch && !p.sealed && !p.stop.is_cancelled())
        })
    })
}
pub(crate) struct HandoffTicket {
    terminal: RequestTerminal,
    epoch: u64,
    sequence: u64,
    target: u64,
    acknowledgment: Option<Acknowledgment>,
    receiver: oneshot::Receiver<Result<Acknowledgment, ()>>,
    stop_wait: Pin<Box<dyn Future<Output = ()> + Send>>,
    connection_wait: Pin<Box<dyn Future<Output = ()> + Send>>,
    stop: CancellationToken,
    connection_stop: CancellationToken,
}
impl HandoffTicket {
    // A producer retains this ticket across a Pending source poll. Every later
    // decoder poll revalidates the exact pause epoch; a historical pause never
    // becomes permanent permission after the gate or writer has resumed.
    pub(crate) fn poll_permission(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), ()>> {
        if self.stop_wait.as_mut().poll(cx).is_ready()
            || self.connection_wait.as_mut().poll(cx).is_ready()
            || !valid(
                &self.terminal,
                self.epoch,
                &self.stop,
                &self.connection_stop,
            )
        {
            return Poll::Ready(Err(()));
        }
        // At most two channel inspections per poll; concurrent phase churn
        // cannot create an unbounded synchronous loop.
        for _ in 0..2 {
            if self.acknowledgment.is_none() {
                self.acknowledgment = match Pin::new(&mut self.receiver).poll(cx) {
                    Poll::Pending => return Poll::Pending,
                    Poll::Ready(Ok(Ok(acknowledgment))) => Some(acknowledgment),
                    Poll::Ready(_) => return Poll::Ready(Err(())),
                };
            }
            let acknowledgment = self.acknowledgment.unwrap();
            if acknowledgment.epoch != self.epoch
                || acknowledgment.sequence != self.sequence
                || acknowledgment.target != self.target
            {
                return Poll::Ready(Err(()));
            }
            // Build the replacement channel outside the outcome lock. It is
            // used only if the old pause expired before decoder admission.
            let (sender, receiver) = oneshot::channel();
            let mut sender = Some(sender);
            let Some(shared) = self.terminal.shared.upgrade() else {
                return Poll::Ready(Err(()));
            };
            let ready = {
                let mut state = shared.state.lock().unwrap();
                let Some(record) = live(&mut state, self.terminal.identity, shared.generation)
                else {
                    return Poll::Ready(Err(()));
                };
                let h = &mut record.handoff;
                if shared.stop.is_cancelled() || !h.live_producer(self.epoch) {
                    return Poll::Ready(Err(()));
                }
                if h.current(acknowledgment) {
                    return Poll::Ready(Ok(()));
                }
                let producer = h.producer.as_mut().unwrap();
                if producer.next_sequence != self.sequence || producer.pending.is_some() {
                    return Poll::Ready(Err(()));
                }
                producer.pending = Some(Pending {
                    epoch: self.epoch,
                    sequence: self.sequence,
                    target: self.target,
                    sender: sender.take().unwrap(),
                });
                h.ready_sender()
            };
            self.receiver = receiver;
            self.acknowledgment = None;
            complete_ready(ready, &self.terminal);
        }
        cx.waker().wake_by_ref();
        Poll::Pending
    }
    pub(crate) async fn wait(mut self) -> Result<(), ()> {
        poll_fn(|cx| self.poll_permission(cx)).await
    }
}
impl Drop for HandoffTicket {
    fn drop(&mut self) {
        let sender = self.terminal.shared.upgrade().and_then(|shared| {
            let mut state = shared.state.lock().unwrap();
            let record = state.records.get_mut(&self.terminal.identity.request)?;
            let producer = record.handoff.producer.as_mut()?;
            if producer
                .pending
                .as_ref()
                .is_some_and(|p| p.epoch == self.epoch && p.sequence == self.sequence)
            {
                record.handoff.take_sender()
            } else {
                None
            }
        });
        complete_sender(sender, Err(()), Some(&self.terminal));
    }
}

/// A real fixture gate owns every pause lease and clears it before publishing
/// its open state. Registrations hold weak references and no body payload.
#[derive(Clone, Default)]
pub(crate) struct PauseGate(Arc<Mutex<GateState>>);
#[derive(Default)]
struct GateState {
    held: bool,
    next: u64,
    entries: BTreeMap<u64, GateEntry>,
}
struct GateEntry {
    terminal: RequestTerminal,
    kind: PauseKind,
    lease: Option<PauseLease>,
}
pub(crate) struct PauseRegistration {
    gate: Weak<Mutex<GateState>>,
    id: u64,
}
impl PauseGate {
    pub(crate) fn held(&self) -> bool {
        self.0.lock().unwrap().held
    }
    pub(crate) fn hold(&self) {
        let mut sends = Vec::new();
        let mut failures = Vec::new();
        {
            let mut state = self.0.lock().unwrap();
            if state.held {
                return;
            }
            for entry in state.entries.values_mut() {
                match entry.terminal.pause_silent(entry.kind) {
                    Ok((lease, sender)) => {
                        entry.lease = Some(lease);
                        sends.push((entry.terminal.clone(), sender));
                    }
                    Err(()) => failures.push(entry.terminal.clone()),
                }
            }
            state.held = true;
        }
        for (terminal, sender) in sends {
            complete_ready(sender, &terminal);
        }
        for terminal in failures {
            terminal.fail(FailureCause::Cancelled);
        }
    }
    pub(crate) fn release(&self) {
        let mut state = self.0.lock().unwrap();
        // PauseLease has only internal lock work, no wake or user destructor.
        // The only lock order is gate -> outcome; callbacks run after outcome.
        for entry in state.entries.values_mut() {
            drop(entry.lease.take());
        }
        state.held = false;
    }
    pub(crate) fn bind(
        &self,
        terminal: RequestTerminal,
        kind: PauseKind,
    ) -> Result<PauseRegistration, ()> {
        let (id, sender) = {
            let mut state = self.0.lock().unwrap();
            if state.entries.len() >= 128 {
                return Err(());
            }
            let id = state.next.checked_add(1).ok_or(())?;
            let (lease, sender) = if state.held {
                let (lease, sender) = terminal.pause_silent(kind)?;
                (Some(lease), sender)
            } else {
                (None, None)
            };
            state.next = id;
            state.entries.insert(
                id,
                GateEntry {
                    terminal: terminal.clone(),
                    kind,
                    lease,
                },
            );
            (id, sender)
        };
        complete_ready(sender, &terminal);
        Ok(PauseRegistration {
            gate: Arc::downgrade(&self.0),
            id,
        })
    }
}
impl Drop for PauseRegistration {
    fn drop(&mut self) {
        let entry = self
            .gate
            .upgrade()
            .and_then(|gate| gate.lock().unwrap().entries.remove(&self.id));
        drop(entry);
    }
}

#[path = "gateway_terminal_handoff_tests.rs"]
mod tests;
