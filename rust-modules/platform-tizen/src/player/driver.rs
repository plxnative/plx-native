//! The Tizen URI driver: a Sans-IO [`UriDriver`] over the host's native media player.
//!
//! **What it is.** The state machine that turns the contract's addressed commands ([`DriverSet::dispatch`])
//! into the host's player requests ([`HostPlayer::open`], [`HostPlayer::verb`]) and the host's player events
//! ([`PlayerEvent`], drained by [`DriverSet::poll`]) into the contract's observations. It starts no worker,
//! reads no global and has no `cfg(test)` fork: its clock and its host are the [`HostPlayer`] it was built over.
//!
//! **Epochs.** The driver mints one `epoch` per open and keeps at most one attempt active. Every request carries
//! it and every event is checked against it: an event whose epoch is not the active one (a completion that lands
//! after a stop, a position of the player a restart replaced) is dropped and counted, never mapped. The hosted
//! library drops the same events one layer earlier (`hosted::player`, an epoch is live from the open until its
//! `PLAYER_CLOSED`); this is the second layer.
//!
//! **Release.** A `Stop` asks the host to close and the attempt is released only by the host's `PLAYER_CLOSED` for its
//! epoch. A close the host never confirms (or the library's gate refused after the app's stop, which never delivers
//! one) is `Release::Quarantined` at the deadline, never a clean release, and [`DriverSet::exit_disposition`] stays
//! `PreserveNativeOwnership` until a close is confirmed.
//!
//! **What it does not claim.** It never emits `Observation::Presented`: that needs the window's alpha and the video
//! hole (`ecore.rs`, `Shell::opaque_route`), which are not part of this driver, so startup metrics stay Unknown (the
//! contract's stated behaviour for a path that cannot say). Its capabilities are what the host's player is asked to
//! do, not a codec verdict ([`CAPS`]); which streams the platform player decodes is the qualifier's concern
//! ([`super::qualifier`]).
use super::port::{HostPlayer, Verb, FULL_WINDOW};
use crate::host_abi::{PlayerEvent, PlayerKind, PLAYER_FAIL_BASE};
use plx_base::task::MainThread;
use plx_playback_contract::uri::{native_index, DisplayEvidence, NativeTrackIndex, UriDriver, UriNotReady, UriSource};
use plx_playback_contract::{AttemptId, BackendEvent, BackendKind, Capabilities, CommandId, ContentTime, DispatchResult,
    DriverEvent, DriverSet, EvidenceSource, ExitDisposition, Failure, Observation, Observed, Operation,
    Playback, ReleaseStatus, Requirements, SourceKey, SourceKind, TrackChoice, TrackKind, TrackSelection};
use std::collections::VecDeque;

/// The driver's own name for its display path (the boot log's `uri: display path=...` line).
pub const DISPLAY_TAG: &str = "ecore-overlay";

/// What the driver can call: a progressive source, pause and seek. No HLS, no track disable, no packet feed, no
/// subtitle selection, and no variable speed (that has no flag: the planner refuses any rate but normal for every
/// URI driver). The flags say what is CALLABLE, never a codec veto.
///
/// `audio_track` is `true` for one reason and one narrow meaning: the contract makes a play whose PMS-selected audio
/// stream has an id a `Selected` track requirement, and the start plan then settles it with one `SelectTrack`. The
/// host player has no track verb, so the driver settles that request only for the source's FIRST audio stream
/// (the stream the native player starts on, [`UriDriver::maps_track`]), with no host call, and refuses any other
/// with `Unsupported`; the qualifier ([`super::qualifier`]) never certifies a play that selects another one. Which
/// stream the retail player really starts on is unmeasured.
pub const CAPS: Capabilities = Capabilities { progressive_uri: true, hls_uri: false, packet_feed: false, pause: true,
    seek: true, audio_track: true, subtitle_track: false, audio_off: false, subtitle_off: false };

/// The deadlines the driver enforces, in milliseconds on the port's clock.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timing {
    /// From the open to the player's `PREPARED`.
    pub startup_timeout_ms: u64,
    /// From a close request to the host's `PLAYER_CLOSED`.
    pub release_allowance_ms: u64,
    /// From a play, pause or seek to its confirmation.
    pub control_timeout_ms: u64,
    /// How long after a `PLAYER_STALL` (a position read hung) the player may stay silent before it is failed.
    pub stall_grace_ms: u64,
    /// How long a staged source waits for its Start.
    pub source_ttl_ms: u64,
}

impl Timing {
    /// The shipped values. The host's own seek expiry is 10 s and its stall report 3 s (`plx_host.h`), so these sit above.
    pub const PRODUCT: Timing = Timing { startup_timeout_ms: 20_000, release_allowance_ms: 10_000,
        control_timeout_ms: 15_000, stall_grace_ms: 7_000, source_ttl_ms: 30_000 };
}

struct Staged {
    attempt: AttemptId,
    key: SourceKey,
    source: UriSource,
    deadline_ms: u64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Opened; the host has not reported `PREPARED`.
    Opening,
    Open,
    /// A close was requested at (and by) `deadline_ms`; `PLAYER_CLOSED` is awaited.
    Closing { deadline_ms: u64 },
    /// The close was not confirmed in time. Only a `PLAYER_CLOSED` ends it now.
    Quarantined,
}

/// A play, pause or seek the host was asked for and has not confirmed.
struct Pending {
    command: CommandId,
    operation: Operation,
    deadline_ms: u64,
}

struct Active {
    attempt: AttemptId,
    epoch: u32,
    phase: Phase,
    tracks: Vec<NativeTrackIndex>,
    start_deadline_ms: u64,
    prepared: bool,
    started_once: bool,
    ended: bool,
    rebuffering: bool,
    stalled_since: Option<u64>,
    controls: Vec<Pending>,
}

/// What reducing one host event asks of the driver beyond the events it pushed.
enum Next {
    Nothing,
    /// The attempt failed (an `Observation::Failed` was pushed): ask the host to close it.
    Close,
    /// The host confirmed the close.
    Released,
}

fn note(out: &mut VecDeque<DriverEvent>, attempt: AttemptId, observation: Observation) {
    out.push_back(DriverEvent::Observation { attempt, observation });
}

fn observed<T>(value: T) -> Observed<T> {
    Observed { value, from: EvidenceSource::BackendReport }
}

pub struct TizenUriDriver<P: HostPlayer> {
    port: P,
    timing: Timing,
    staged: Option<Staged>,
    active: Option<Active>,
    last_started: Option<AttemptId>,
    next_epoch: u32,
    pending: VecDeque<DriverEvent>,
    inbox: Vec<PlayerEvent>,
    /// Host events dropped for an epoch that is not the active one, since the driver was built.
    stale_dropped: usize,
}

impl<P: HostPlayer> TizenUriDriver<P> {
    pub fn new(port: P, timing: Timing) -> Self {
        Self { port, timing, staged: None, active: None, last_started: None, next_epoch: 0,
            pending: VecDeque::new(), inbox: Vec::new(), stale_dropped: 0 }
    }

    /// How many host events were dropped because their epoch was not the active one.
    #[cfg(test)]
    pub fn stale_dropped(&self) -> usize {
        self.stale_dropped
    }

    fn mint_epoch(&mut self) -> u32 {
        self.next_epoch = self.next_epoch.wrapping_add(1).max(1);
        self.next_epoch
    }

    /// An admission refusal before anything was opened. The common reducer wants a resource-owner verdict even then,
    /// so a driver that holds nothing says `Released` for the attempt; one that holds another attempt says nothing.
    fn refuse_before_open(&mut self, attempt: AttemptId, failure: Failure) -> DispatchResult {
        if self.active.is_none() {
            self.pending.push_back(DriverEvent::Release { attempt, status: ReleaseStatus::Released });
        }
        DispatchResult::Refused(failure)
    }

    fn start(&mut self, attempt: AttemptId, key: SourceKey, requirements: Requirements) -> DispatchResult {
        // A Start consumes its own attempt's staged source even when it is then refused; another attempt's is untouched.
        let staged = if self.staged.as_ref().is_some_and(|s| s.attempt == attempt) { self.staged.take() } else { None };
        if self.active.is_some() {
            return self.refuse_before_open(attempt, Failure::Busy);
        }
        if self.last_started.is_some_and(|last| attempt <= last) {
            return self.refuse_before_open(attempt, Failure::Stale);
        }
        if requirements.source != SourceKind::ProgressiveUri || !CAPS.supports(requirements) {
            return self.refuse_before_open(attempt, Failure::Unsupported);
        }
        let Some(staged) = staged else { return self.refuse_before_open(attempt, Failure::Refused) };
        let now = self.port.now_ms();
        if staged.key != key || now >= staged.deadline_ms {
            return self.refuse_before_open(attempt, Failure::Stale);
        }
        if !staged.source.can_supply(requirements) {
            return self.refuse_before_open(attempt, Failure::Unsupported);
        }
        if self.port.offered().is_err() {
            return self.refuse_before_open(attempt, Failure::Unavailable);
        }
        let epoch = self.mint_epoch();
        let (url, tracks) = staged.source.into_parts();
        // The URL is lent to the host for this call and dropped with `url`: it carries the Plex token.
        if let Err(refusal) = self.port.open(epoch, &url, FULL_WINDOW) {
            return self.refuse_before_open(attempt, refusal.failure());
        }
        self.last_started = Some(attempt);
        self.active = Some(Active { attempt, epoch, phase: Phase::Opening, tracks,
            start_deadline_ms: now.saturating_add(self.timing.startup_timeout_ms), prepared: false, started_once: false,
            ended: false, rebuffering: false, stalled_since: None, controls: Vec::new() });
        DispatchResult::Queued
    }

    /// Ask the host to close the active attempt and wait for its confirmation. A request the host refuses leaves
    /// the attempt closing: only the deadline (a quarantine) or a late `PLAYER_CLOSED` ends it.
    fn begin_close(&mut self) {
        let now = self.port.now_ms();
        let Some(active) = self.active.as_mut() else { return };
        if matches!(active.phase, Phase::Closing { .. } | Phase::Quarantined) {
            return;
        }
        active.phase = Phase::Closing { deadline_ms: now.saturating_add(self.timing.release_allowance_ms) };
        if let Err(refusal) = self.port.verb(active.epoch, Verb::Close) {
            plx_base::eventlog::log(&format!("tizen-uri: the host refused the close ({refusal:?}); the release is not confirmed"));
        }
    }

    /// A transport request (play, pause, seek) for the active attempt.
    fn control(&mut self, attempt: AttemptId, command: CommandId, operation: Operation) -> DispatchResult {
        let now = self.port.now_ms();
        let Some(active) = self.active.as_mut().filter(|a| a.attempt == attempt) else {
            return DispatchResult::Refused(Failure::Stale);
        };
        if active.phase != Phase::Open || !active.prepared {
            return DispatchResult::Refused(Failure::Busy);
        }
        let verb = match operation {
            Operation::Play => Verb::Play,
            Operation::Pause => Verb::Pause,
            Operation::Seek(time) => {
                let Ok(ms) = i64::try_from(time.as_millis()) else { return DispatchResult::Refused(Failure::Unsupported) };
                let Ok(command) = i64::try_from(command.get()) else { return DispatchResult::Refused(Failure::Unsupported) };
                Verb::Seek { ms, command }
            }
            _ => return DispatchResult::Refused(Failure::Unsupported),
        };
        if let Err(refusal) = self.port.verb(active.epoch, verb) {
            return DispatchResult::Refused(refusal.failure());
        }
        active.controls.push(Pending { command, operation, deadline_ms: now.saturating_add(self.timing.control_timeout_ms) });
        DispatchResult::Queued
    }

    /// The one audio request the host player can honour: the stream it starts on, the source's first.
    fn select_audio(&mut self, attempt: AttemptId, command: CommandId, choice: TrackChoice) -> DispatchResult {
        let Some(active) = self.active.as_ref().filter(|a| a.attempt == attempt) else {
            return DispatchResult::Refused(Failure::Stale);
        };
        let TrackChoice::Selected(id) = choice else { return DispatchResult::Refused(Failure::Unsupported) };
        let track = TrackSelection { kind: TrackKind::Audio, id };
        if active.phase != Phase::Open || !active.prepared {
            return DispatchResult::Refused(Failure::Busy);
        }
        if native_index(&active.tracks, track) != Some(0) {
            return DispatchResult::Refused(Failure::Unsupported);
        }
        self.pending.push_back(DriverEvent::Observation { attempt, observation: Observation::TrackSelected(observed(track)) });
        self.pending.push_back(DriverEvent::TrackSettled { attempt, command, kind: TrackKind::Audio, choice });
        DispatchResult::Queued
    }

    /// Fold one host event into `active`, pushing what it means; the reducer is pure over (`active`, event, now).
    fn reduce(active: &mut Active, out: &mut VecDeque<DriverEvent>, event: PlayerEvent, now: u64) -> Next {
        let attempt = active.attempt;
        if event.kind == PlayerKind::Closed {
            return Next::Released;
        }
        // After a close request nothing but the release matters, but a seek still in flight is answered.
        let live = matches!(active.phase, Phase::Opening | Phase::Open);
        match event.kind {
            PlayerKind::Prepared if live && !active.prepared => {
                active.prepared = true;
                active.phase = Phase::Open;
                note(out, attempt, Observation::Playback(observed(Playback::ReadyAwaitPlay)));
            }
            PlayerKind::Started if live && active.prepared => {
                active.started_once = true;
                if let Some(at) = active.controls.iter().position(|c| c.operation == Operation::Play) {
                    let control = active.controls.remove(at);
                    out.push_back(DriverEvent::ControlAccepted { attempt, command: control.command,
                        operation: control.operation, native_acknowledged: true });
                }
                out.push_back(DriverEvent::Observation { attempt, observation: Observation::Playback(observed(Playback::Playing)) });
            }
            PlayerKind::Paused if live && active.prepared => {
                if let Some(at) = active.controls.iter().position(|c| c.operation == Operation::Pause) {
                    let control = active.controls.remove(at);
                    out.push_back(DriverEvent::ControlAccepted { attempt, command: control.command,
                        operation: control.operation, native_acknowledged: true });
                }
                out.push_back(DriverEvent::Observation { attempt, observation: Observation::Playback(observed(Playback::Paused)) });
            }
            PlayerKind::Position if live && active.prepared => {
                active.stalled_since = None;
                if let Ok(ms) = u64::try_from(event.a) {
                    out.push_back(DriverEvent::Observation { attempt,
                        observation: Observation::Position(observed(ContentTime::from_millis(ms))) });
                }
            }
            PlayerKind::SeekDone => {
                let at = active.controls.iter().position(|c| matches!(c.operation, Operation::Seek(_))
                    && i64::try_from(c.command.get()) == Ok(event.a));
                if let Some(at) = at {
                    let control = active.controls.remove(at);
                    out.push_back(match event.b {
                        ms if ms >= 0 => DriverEvent::SeekLanded { attempt, command: control.command,
                            position: u64::try_from(ms).ok().map(|ms| observed(ContentTime::from_millis(ms))) },
                        // Expired: the seek may still land, so a position is the authority; it is not a refusal of it.
                        -2 => DriverEvent::ControlRefused { attempt, command: control.command,
                            operation: control.operation, failure: Failure::TimedOut },
                        _ => DriverEvent::ControlRefused { attempt, command: control.command,
                            operation: control.operation, failure: Failure::Refused },
                    });
                }
            }
            PlayerKind::Buffering if live && active.started_once => {
                let rebuffering = event.a < 100;
                if rebuffering != active.rebuffering {
                    active.rebuffering = rebuffering;
                    note(out, attempt, Observation::Rebuffering(observed(rebuffering)));
                }
            }
            // A completion before the player was prepared is no end of anything.
            PlayerKind::Completed if live && active.prepared && !active.ended => {
                active.ended = true;
                note(out, attempt, Observation::Playback(observed(Playback::Ended)));
            }
            PlayerKind::Error | PlayerKind::Interrupted if live => {
                note(out, attempt, Observation::Failed(Failure::Backend));
                return Next::Close;
            }
            PlayerKind::Stall if live => {
                active.stalled_since.get_or_insert(now);
            }
            _ => {}
        }
        Next::Nothing
    }

    /// The deadlines: startup, a hung player, an unconfirmed control, an unconfirmed close.
    fn deadlines(&mut self) {
        let now = self.port.now_ms();
        let Some(active) = self.active.as_mut() else { return };
        let attempt = active.attempt;
        let mut fail = false;
        match active.phase {
            Phase::Opening | Phase::Open => {
                if !active.prepared && now >= active.start_deadline_ms {
                    fail = true;
                } else if active.stalled_since.is_some_and(|since| now.saturating_sub(since) >= self.timing.stall_grace_ms) {
                    fail = true;
                }
                let (expired, kept): (Vec<_>, Vec<_>) = active.controls.drain(..).partition(|c| now >= c.deadline_ms);
                active.controls = kept;
                for control in expired {
                    plx_base::eventlog::log(&format!("tizen-uri: control {:?} unconfirmed; refused (TimedOut)", control.operation));
                    self.pending.push_back(DriverEvent::ControlRefused { attempt, command: control.command,
                        operation: control.operation, failure: Failure::TimedOut });
                }
                if fail {
                    self.pending.push_back(DriverEvent::Observation { attempt, observation: Observation::Failed(Failure::TimedOut) });
                }
            }
            Phase::Closing { deadline_ms } if now >= deadline_ms => {
                active.phase = Phase::Quarantined;
                self.pending.push_back(DriverEvent::Release { attempt, status: ReleaseStatus::Quarantined });
            }
            Phase::Closing { .. } | Phase::Quarantined => {}
        }
        if fail {
            self.begin_close();
        }
    }

    fn poll_events(&mut self) {
        let mut inbox = std::mem::take(&mut self.inbox);
        let drained = self.port.drain(&mut inbox);
        let mut close = false;
        if drained.overflowed {
            // The host's lossless queue lost an event: part of the player's story is gone, so the attempt cannot be trusted.
            plx_base::eventlog::log("tizen-uri: the host's player queue overflowed; the attempt is failed");
            if let Some(active) = self.active.as_ref().filter(|a| matches!(a.phase, Phase::Opening | Phase::Open)) {
                self.pending.push_back(DriverEvent::Observation { attempt: active.attempt, observation: Observation::Failed(Failure::Backend) });
                close = true;
            }
        }
        let now = self.port.now_ms();
        for event in inbox.drain(..) {
            if event.kind == PlayerKind::Error {
                let who = if event.a >= PLAYER_FAIL_BASE { "the host" } else { "the native player" };
                plx_base::eventlog::log(&format!("tizen-uri: {who} reported an error (code {})", event.a));
            }
            let Some(active) = self.active.as_mut().filter(|a| a.epoch == event.epoch) else {
                self.stale_dropped += 1;
                if self.stale_dropped == 1 {
                    plx_base::eventlog::log(&format!("tizen-uri: a host event ({:?}) of epoch {} is not the active player's; dropped", event.kind, event.epoch));
                }
                continue;
            };
            match Self::reduce(active, &mut self.pending, event, now) {
                Next::Nothing => {}
                Next::Close => close = true,
                Next::Released => {
                    let attempt = active.attempt;
                    // Every seek still held is answered before the release: the host sends its SEEK_DONE first, but a
                    // driver must not depend on it.
                    for control in std::mem::take(&mut active.controls) {
                        self.pending.push_back(DriverEvent::ControlRefused { attempt, command: control.command,
                            operation: control.operation, failure: Failure::Refused });
                    }
                    self.pending.push_back(DriverEvent::Release { attempt, status: ReleaseStatus::Released });
                    self.active = None;
                    close = false;
                }
            }
        }
        self.inbox = inbox;
        if close {
            self.begin_close();
        }
    }
}

impl<P: HostPlayer> DriverSet for TizenUriDriver<P> {
    type Context = MainThread;

    fn capabilities(&self, backend: BackendKind) -> Option<Capabilities> {
        (backend == BackendKind::Uri).then_some(CAPS)
    }

    fn dispatch(&mut self, backend: BackendKind, _: &MainThread, attempt: AttemptId, command: CommandId,
        operation: Operation) -> DispatchResult {
        if backend != BackendKind::Uri {
            return DispatchResult::Refused(Failure::Unavailable);
        }
        match operation {
            Operation::Start { source, requirements } => self.start(attempt, source, requirements),
            Operation::Play | Operation::Pause | Operation::Seek(_) => self.control(attempt, command, operation),
            Operation::SelectTrack { kind: TrackKind::Audio, choice } => self.select_audio(attempt, command, choice),
            Operation::SelectTrack { kind: TrackKind::Subtitle, .. } => {
                if self.active.as_ref().is_some_and(|a| a.attempt == attempt) {
                    DispatchResult::Refused(Failure::Unsupported)
                } else {
                    DispatchResult::Refused(Failure::Stale)
                }
            }
            Operation::Stop(_) => {
                let Some(active) = self.active.as_ref().filter(|a| a.attempt == attempt) else {
                    return DispatchResult::Refused(Failure::Stale);
                };
                if active.phase == Phase::Quarantined {
                    return DispatchResult::Refused(Failure::ReleaseUncertain);
                }
                self.begin_close();
                DispatchResult::Queued
            }
        }
    }

    fn poll(&mut self, _: &MainThread) -> Vec<BackendEvent> {
        let now = self.port.now_ms();
        if self.staged.as_ref().is_some_and(|s| now >= s.deadline_ms) {
            self.staged = None;
        }
        self.poll_events();
        self.deadlines();
        self.pending.drain(..).map(|event| BackendEvent { backend: BackendKind::Uri, event }).collect()
    }

    fn discard_staged(&mut self, backend: BackendKind, attempt: AttemptId, key: SourceKey) {
        if backend == BackendKind::Uri && self.staged.as_ref().is_some_and(|s| s.attempt == attempt && s.key == key) {
            self.staged = None;
        }
    }

    /// The player is held from the open until the host confirmed its close, and a quarantine holds it still.
    fn exit_disposition(&self) -> ExitDisposition {
        if self.active.is_some() { ExitDisposition::PreserveNativeOwnership } else { ExitDisposition::Clean }
    }
}

impl<P: HostPlayer> UriDriver for TizenUriDriver<P> {
    /// Ready when the host offers a player. That is ALL it rests on: the window's alpha and the video hole are not this
    /// driver's, so it says nothing of whether a picture shows (see the module note, and `docs/tizen.md`).
    fn display(&self) -> DisplayEvidence {
        if self.port.offered().is_ok() { DisplayEvidence::ready(DISPLAY_TAG) } else { DisplayEvidence::unavailable(DISPLAY_TAG) }
    }

    fn stage_source(&mut self, attempt: AttemptId, key: SourceKey, source: UriSource) -> Result<(), Failure> {
        if !source.has_url() || self.last_started.is_some_and(|last| attempt <= last) {
            return Err(Failure::Stale);
        }
        if !source.valid_track_map() {
            return Err(Failure::Unsupported);
        }
        let deadline_ms = self.port.now_ms().saturating_add(self.timing.source_ttl_ms);
        self.staged = Some(Staged { attempt, key, source, deadline_ms });
        Ok(())
    }

    fn cancel_staged(&mut self, attempt: AttemptId) {
        if self.staged.as_ref().is_some_and(|s| s.attempt == attempt) {
            self.staged = None;
        }
    }

    /// Owned from the Start until the close is confirmed: an ownership fact, not proof that a picture shows.
    fn plane_owned(&self) -> bool {
        self.active.is_some()
    }

    /// Only the source's first audio stream: the one the host player starts on (see [`CAPS`]).
    fn maps_track(&self, selection: TrackSelection) -> bool {
        selection.kind == TrackKind::Audio
            && self.active.as_ref().is_some_and(|a| native_index(&a.tracks, selection) == Some(0))
    }

    fn ready_to_start(&mut self, _: &MainThread) -> Result<(), UriNotReady> {
        self.port.offered().map_err(|_| UriNotReady::Driver("the host has no media player"))
    }
}

/// The requirements a plain progressive play states (a helper for the tests).
#[cfg(test)]
pub(super) fn plain_requirements() -> Requirements {
    use plx_playback_contract::Fidelity;
    Requirements { source: SourceKind::ProgressiveUri, video: Fidelity::Exact, audio: Fidelity::Exact,
        audio_track: TrackChoice::Unspecified, subtitle_track: TrackChoice::Unspecified }
}
