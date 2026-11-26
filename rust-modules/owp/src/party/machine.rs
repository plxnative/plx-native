#![deny(clippy::wildcard_enum_match_arm)]

use super::deadlines::{self, Deadlines, Ids, Timer};
use super::lane::{Job, JobKind, Lane, LaneEffect, LaneEvent, PlayerAction};
use super::link::{Hello, Link, LinkEffect, LinkEvent, LinkState, CONNECT_TIMEOUT_MS};
use super::view;
use super::{
    Answer, Attached, Attempt, AttemptStep, Binding, Choosing, EndReason, Fetch, Goal, Hold, Matching, Media, Moment, NetEvent, Outcome, Own, OwnFile, PartyEffect, PartyError, PartyEvent, PartyInput,
    PartyState, Phase, Pick, Reads, Refreshed, RelaySource, Relays, Request, Resume, ResumeStep, Search, Seat, Session, Status, StatusValues, BACKOFF_CEILING_MS, BACKOFF_FIRST_MS, FETCH_TIMEOUT_MS,
    HEARTBEAT_MS, HELD_LOADING_MS, HOLD_MS, REFRESH_AFTER_MS, RESOLVE_TIMEOUT_MS, RESUME_FOR_MS, STATUS_SPACING_MS, TICK_MS,
};
use crate::code;
use crate::json::{self, Json, Text};
use crate::matching::{self, Candidate, Outcome as WalkOutcome, ProbeRun, Progress, Walk};
use crate::media::{self, Direction};
use crate::message::{decode, Incoming, MatchReport, MatchState, Outgoing, PingId, PlayerState, Received};
use crate::reconcile::{self, Action, Player as StepPlayer, StepInput};
use crate::relays;
use crate::room::{MAX_OFFSET_MS, MAX_RATE, MIN_RATE};
use crate::text;
use crate::timeline::Phase as RoomPhase;
use std::collections::BTreeMap;

#[derive(Debug)]
pub struct Party {
    name: String,
    client: String,
    resolver: bool,
    relays: Relays,
    now_ms: i64,
    reads: Option<Reads>,
    phase: Phase,
    links: BTreeMap<u64, Link>,
    lanes: BTreeMap<u64, Lane>,
    fetches: BTreeMap<u64, Fetch>,
    attached: Attached,
    binding: Binding,
    player: PlayerState,
    offset_ms: i64,
    matching: Matching,
    status: Status,
    hold: Hold,
    released_waiters: Vec<u64>,
    deadlines: Deadlines,
    ids: Ids,
    view: PartyState,
}

#[derive(Debug, Clone, PartialEq)]
enum Work {
    Input(PartyInput),
    Timer(Timer),
    LinkFx { conn: u64, fx: Vec<LinkEffect> },
    Ponged { conn: u64 },
    Text { conn: u64, text: String },
    Routed { conn: u64, text: String, received: Received },
    Lost { conn: u64, code: Option<u16> },
    Hello { conn: u64, hello: Hello },
    HelloRefused { conn: u64, error: PartyError },
    FailAttempt { error: PartyError },
    OpenJoin { url: String },
    Enter { conn: u64, answer: Answer, done: Option<(u64, Outcome)> },
    EnterRest { conn: u64, before: Option<Json>, resend: Option<OwnFile>, buffered: Vec<String>, done: Option<(u64, Outcome)> },
    Receive { message: Incoming },
    AcceptRest { before: Json, action: Option<(String, Option<String>)> },
    RelayError { error: PartyError },
    Dropped { code: Option<u16> },
    ScheduleResume,
    ResumeFailed { conn: u64, error: PartyError },
    End { reason: EndReason, error: Option<PartyError> },
    EndRest { reason: EndReason, error: Option<PartyError> },
    EndFinal { reason: EndReason, error: Option<PartyError> },
    Reset,
    NewEpoch,
    StartMatching { walk: bool },
    RestartSearch,
    CancelMatching,
    WalkOn,
    WalkDone { outcome: WalkOutcome, ranked: Vec<Candidate> },
    ChooseWith { call: u64, candidate: Candidate, duration_ms: i64 },
    Rebind,
    Bind,
    Unbind,
    UnbindAndPause,
    ReleaseAdapter,
    Attach { player: u64, reference: String, can_set_rate: bool, same: bool },
    GoAway,
    SettleReleased,
    DoneWhenSettled { call: u64 },
    Enqueue { player: u64, job: Job },
    LaneFx { player: u64, fx: Vec<LaneEffect>, threw: bool },
    PlayRefused { player: u64 },
    DrainEnd { player: u64 },
    Step,
    StepIfWanted,
    RefreshPlayer,
    Changed,
    QueueStatus { force: bool },
    SendStatus,
    StopStatus,
    HoldFor { re: &'static str },
    ReleaseHold,
    Leave,
    MarkClosed,
    SendText { conn: u64, text: String },
    Done { call: u64, outcome: Outcome },
    RelaysConnected,
    Fetched { get: u64, ok: bool },
    ClearMatching,
    Emit { events: Vec<PartyEvent> },
    Chose { candidate: Candidate, report: Option<MatchReport> },
}

fn then(stack: &mut Vec<Work>, items: Vec<Work>) {
    stack.extend(items.into_iter().rev());
}

fn wire(message: &Outgoing) -> String {
    message.encode().unwrap_or_default()
}

fn failed(code: &str, message: &str) -> Outcome {
    Outcome::Failed(PartyError::new(code, message))
}

fn report(state: MatchState, delta_ms: Option<i64>) -> Option<MatchReport> {
    Some(MatchReport { state, delta_ms })
}

fn backoff_ms(attempt: u32, u: f64) -> i64 {
    let base = if attempt > 5 { BACKOFF_CEILING_MS } else { (BACKOFF_FIRST_MS << (attempt.max(1) - 1)).min(BACKOFF_CEILING_MS) };
    (base as f64 * (0.5 + 0.5 * u)).floor() as i64
}

fn clock_of(links: &BTreeMap<u64, Link>, phase: &Phase) -> Option<crate::clock::Sample> {
    let seat = phase.seat()?;
    links.get(&seat.conn).and_then(Link::best)
}

fn member_conn(phase: &Phase) -> Option<u64> {
    match phase {
        Phase::InRoom(seat) => Some(seat.conn),
        Phase::Idle | Phase::Connecting(_) | Phase::Reconnecting(..) | Phase::Ended(_) | Phase::Closed(_) => None,
    }
}

fn own_candidate(file: &OwnFile) -> Candidate {
    Candidate { id: String::new(), reference: file.reference.clone(), duration_ms: Some(view::duration_ms(&file.media)), ..Candidate::default() }
}

fn chosen(pick: &Pick) -> Option<&Candidate> {
    match pick {
        Pick::Chosen(candidate) => Some(candidate),
        Pick::None | Pick::Offered { .. } => None,
    }
}

fn attached_player(attached: &Attached) -> Option<u64> {
    match attached {
        Attached::Player { player, .. } => Some(*player),
        Attached::None | Attached::Away => None,
    }
}

fn with_seek(attached: Attached, at_ms: Option<i64>) -> Attached {
    match attached {
        Attached::Player { player, reference, can_set_rate, step_wanted, .. } => Attached::Player { player, reference, can_set_rate, last_seek_ms: at_ms, step_wanted },
        other @ (Attached::None | Attached::Away) => other,
    }
}

fn wanting(attached: Attached, wanted: bool) -> Attached {
    match attached {
        Attached::Player { player, reference, can_set_rate, last_seek_ms, .. } => Attached::Player { player, reference, can_set_rate, last_seek_ms, step_wanted: wanted },
        other @ (Attached::None | Attached::Away) => other,
    }
}

fn bindable(attached: &Attached, phase: &Phase, matching: &Matching) -> bool {
    let Attached::Player { reference, .. } = attached else { return false };
    let Some(seat) = phase.seat() else { return false };
    let state = matching.report.map(|report| report.state);
    seat.media.descriptor().is_some()
        && !seat.own.pending()
        && chosen(&matching.pick).is_some_and(|choice| &choice.reference == reference)
        && matches!(state, Some(MatchState::Matched | MatchState::Mismatched))
}

fn released_busy(lanes: &BTreeMap<u64, Lane>, attached: &Attached) -> bool {
    let current = attached_player(attached);
    lanes.iter().any(|(player, lane)| Some(*player) != current && lane.busy())
}

#[allow(clippy::too_many_arguments)]
fn compute_player(phase: &Phase, matching: &Matching, attached: &Attached, binding: Binding, reads: Option<&Reads>, lanes: &BTreeMap<u64, Lane>, relay_now_ms: Option<i64>, offset_ms: i64) -> PlayerState {
    let Some(seat) = phase.seat() else { return PlayerState::Idle };
    let Some(media) = seat.media.descriptor() else { return PlayerState::Idle };
    if matching.report.is_some_and(|report| report.state == MatchState::Unavailable) || matches!(attached, Attached::Away) {
        return PlayerState::Idle;
    }
    let Attached::Player { player, .. } = attached else { return PlayerState::Loading };
    let Binding::Bound { blocked, .. } = binding else { return PlayerState::Loading };
    let Some(reads) = reads else { return PlayerState::Loading };
    if !reads.first_frame_shown || lanes.get(player).is_some_and(Lane::seeking) {
        return PlayerState::Loading;
    }
    let room = view::timeline(&seat.room);
    if room.phase == RoomPhase::Playing && relay_now_ms.is_none() {
        return PlayerState::Loading;
    }
    let accuracy = reads.seek_accuracy_ms.max(0);
    let hold = reconcile::hold(&room, offset_ms, relay_now_ms.unwrap_or(0), reads.duration_ms);
    if hold.as_ref().is_some_and(|hold| reads.position_ms.saturating_sub(hold.at_ms).saturating_abs() > HELD_LOADING_MS.max(accuracy)) {
        return PlayerState::Loading;
    }
    if blocked {
        return PlayerState::Loading;
    }
    if reads.buffering {
        let running = relay_now_ms.is_some_and(|now| room.at(view::duration_ms(media), now).running);
        return if hold.is_none() && running { PlayerState::Buffering } else { PlayerState::Loading };
    }
    PlayerState::Ready
}

fn build_view(phase: &Phase, matching: &Matching, links: &BTreeMap<u64, Link>, offset_ms: i64, player: PlayerState) -> PartyState {
    let seat = phase.seat();
    let me = seat.is_some_and(|seat| view::has_peer(&seat.room, &seat.session.peer_id));
    let is_host = seat.is_some_and(|seat| view::is_host(&seat.room, &seat.session.peer_id));
    let everyone = seat.is_some_and(|seat| view::everyone_controls(&seat.room));
    let media = seat.and_then(|seat| seat.media.descriptor().cloned());
    let (choice, mismatch) = match &matching.pick {
        Pick::None => (None, None),
        Pick::Chosen(candidate) => (Some(candidate.clone()), None),
        Pick::Offered { candidate, delta_ms } => (None, Some((candidate.clone(), *delta_ms))),
    };
    PartyState {
        connection: phase.connection(),
        peer_id: seat.map(|seat| seat.session.peer_id.clone()),
        room: seat.map(|seat| seat.room.clone()),
        media: media.clone(),
        report: matching.report,
        choice,
        mismatch,
        candidates: matching.candidates.clone(),
        offset_ms,
        player,
        end_reason: phase.end_reason(),
        is_host,
        can_control: is_host || (me && everyone),
        timeline: seat.zip(media).map(|(seat, media)| (view::timeline(&seat.room), view::duration_ms(&media))),
        relay_offset_ms: clock_of(links, phase).map(|best| best.offset_ms),
    }
}

fn answer_of(message: &Incoming) -> Option<Answer> {
    match message {
        Incoming::Joined { code, peer_id, token, room, media, .. } => {
            Some(Answer { code: code.clone(), peer_id: peer_id.clone(), token: token.clone(), room: room.clone(), media: media.clone(), buffered: Vec::new() })
        }
        Incoming::Room { .. } | Incoming::Pong { .. } | Incoming::Error { .. } | Incoming::Closed { .. } => None,
    }
}

fn error_of(message: &Incoming) -> Option<PartyError> {
    match message {
        Incoming::Error { code, message, re } => Some(PartyError { code: code.clone(), re: re.clone(), message: message.clone() }),
        Incoming::Joined { .. } | Incoming::Room { .. } | Incoming::Pong { .. } | Incoming::Closed { .. } => None,
    }
}

fn answers(received: &Received, re: &str) -> bool {
    match received {
        Received::Message(Incoming::Joined { .. }) | Received::Malformed("joined") => true,
        Received::Message(Incoming::Error { re: Some(answered), .. }) => answered == re,
        Received::Message(Incoming::Error { re: None, .. } | Incoming::Room { .. } | Incoming::Pong { .. } | Incoming::Closed { .. }) | Received::Malformed(_) | Received::Ignored | Received::TooLarge => false,
    }
}

impl Party {
    pub fn new(name: &str, client: &str, resolver: bool, source: RelaySource) -> Party {
        let (list, public) = match source {
            RelaySource::Public => (relays::embedded_public_list().unwrap_or_default(), true),
            RelaySource::At(relay) => (vec![relay], false),
        };
        Party {
            name: name.to_owned(),
            client: client.to_owned(),
            resolver,
            relays: Relays { list, public, last_refresh_ms: None },
            now_ms: i64::MIN,
            reads: None,
            phase: Phase::Idle,
            links: BTreeMap::new(),
            lanes: BTreeMap::new(),
            fetches: BTreeMap::new(),
            attached: Attached::None,
            binding: Binding::Unbound,
            player: PlayerState::Idle,
            offset_ms: 0,
            matching: Matching { search: Search::Idle, choosing: Choosing::Idle, report: None, pick: Pick::None, candidates: Vec::new() },
            status: Status::Off,
            hold: Hold::None,
            released_waiters: Vec::new(),
            deadlines: Deadlines::default(),
            ids: Ids::default(),
            view: PartyState::idle(),
        }
    }

    pub fn checked(name: &str, client: &str, resolver: bool, source: RelaySource) -> Result<Party, PartyError> {
        if !text::valid_name(&Json::Str(Text::new(name))) {
            return Err(PartyError::new("invalid_message", "name"));
        }
        if !text::valid_client(&Json::Str(Text::new(client))) {
            return Err(PartyError::new("invalid_message", "client"));
        }
        Ok(Party::new(name, client, resolver, source))
    }

    pub fn step(&mut self, input: PartyInput, effects: &mut Vec<PartyEffect>) {
        self.transition(input, effects);
    }

    pub fn next_wake_ms(&self) -> Option<i64> {
        self.deadlines.next_ms()
    }

    pub fn next_wake_order(&self) -> Option<(i64, u64)> {
        self.deadlines.next_key()
    }

    pub fn timers_set(&self) -> u64 {
        self.deadlines.set_count()
    }

    pub fn state(&self) -> &PartyState {
        &self.view
    }

    #[allow(clippy::too_many_lines)]
    fn transition(&mut self, input: PartyInput, effects: &mut Vec<PartyEffect>) {
        let moment = input.moment();
        self.now_ms = self.now_ms.max(moment.now_ms);
        self.reads = moment.player.as_ref().cloned();
        let now = self.now_ms;
        let mut stack = vec![Work::Input(input)];
        while let Some(work) = stack.pop() {
            match work {
                Work::Input(PartyInput::Wake { .. }) => {
                    if let Some((_, timer)) = self.deadlines.take_due(now) {
                        stack.push(Work::Timer(timer));
                    }
                }
                Work::Input(PartyInput::Drawn { u, .. }) => {
                    let mut phase = std::mem::take(&mut self.phase);
                    if let Phase::Reconnecting(_, resume) = &mut phase {
                        if resume.step == ResumeStep::Drawing {
                            resume.step = ResumeStep::Waiting;
                            self.deadlines.set(now, backoff_ms(resume.attempt, u), Timer::Backoff);
                        }
                    }
                    self.phase = phase;
                }
                Work::Input(PartyInput::Net { conn, event, .. }) => match event {
                    NetEvent::Opened => {
                        if self.links.get(&conn).is_some_and(|link| link.state() == LinkState::Opening) {
                            self.deadlines.cancel(Timer::LinkOpen { conn });
                            let mut fx = Vec::new();
                            if let Some(link) = self.links.get_mut(&conn) {
                                link.step(LinkEvent::Opened { at_ms: now }, &mut fx);
                            }
                            let hello = match &self.phase {
                                Phase::Connecting(Attempt { step: AttemptStep::Opening { conn: opening }, goal, .. }) if *opening == conn => Some(match goal {
                                    Goal::Create { .. } => Hello::Create,
                                    Goal::Join { .. } => Hello::Join,
                                }),
                                Phase::Reconnecting(_, Resume { step: ResumeStep::Opening { conn: opening }, .. }) if *opening == conn => Some(Hello::Resume),
                                Phase::Idle | Phase::Connecting(_) | Phase::InRoom(_) | Phase::Reconnecting(..) | Phase::Ended(_) | Phase::Closed(_) => None,
                            };
                            let mut seq = vec![Work::LinkFx { conn, fx }];
                            seq.extend(hello.map(|hello| Work::Hello { conn, hello }));
                            then(&mut stack, seq);
                        }
                    }
                    NetEvent::OpenFailed => {
                        if self.links.get(&conn).is_some_and(|link| link.state() == LinkState::Opening) {
                            if let Some(link) = self.links.get_mut(&conn) {
                                link.step(LinkEvent::Lost, &mut Vec::new());
                            }
                            self.deadlines.cancel_where(deadlines::of_conn, conn);
                            stack.push(Work::Lost { conn, code: None });
                        }
                    }
                    NetEvent::Text(text) => {
                        if self.links.get(&conn).is_some_and(Link::is_open) {
                            let mut fx = Vec::new();
                            if let Some(link) = self.links.get_mut(&conn) {
                                link.step(LinkEvent::Heard { at_ms: now }, &mut fx);
                            }
                            then(&mut stack, vec![Work::LinkFx { conn, fx }, Work::Text { conn, text }]);
                        }
                    }
                    NetEvent::Closed(code) => {
                        if self.links.get(&conn).is_some_and(Link::is_open) {
                            if let Some(link) = self.links.get_mut(&conn) {
                                link.step(LinkEvent::Lost, &mut Vec::new());
                            }
                            self.deadlines.cancel_where(deadlines::of_conn, conn);
                            stack.push(Work::Lost { conn, code });
                        }
                    }
                },
                Work::Input(PartyInput::Got { get, body, .. }) => {
                    if self.fetches.contains_key(&get) {
                        self.deadlines.cancel(Timer::Fetch { get });
                        let list = body.as_deref().and_then(|body| json::read(body).ok()).and_then(|read| relays::validate(&read.value));
                        let ok = list.is_some();
                        if let Some(list) = list {
                            self.relays.list = list;
                            self.relays.last_refresh_ms = Some(now);
                        }
                        stack.push(Work::Fetched { get, ok });
                    }
                }
                Work::Input(PartyInput::CommandDone { cmd, threw, .. }) => {
                    let player = self.lanes.iter().find(|(_, lane)| lane.running_cmd() == Some(cmd)).map(|(player, _)| *player);
                    if let Some(player) = player {
                        let next = self.ids.mint();
                        let mut fx = Vec::new();
                        if let Some(lane) = self.lanes.get_mut(&player) {
                            lane.step(LaneEvent::Finished { cmd, next }, &mut fx);
                        }
                        stack.push(Work::LaneFx { player, fx, threw });
                    }
                }
                Work::Input(PartyInput::Resolved { search, answer, .. }) => {
                    let walk = match &self.matching.search {
                        Search::Resolving { search: running, walk } if *running == search => Some(*walk),
                        Search::Idle | Search::Resolving { .. } | Search::Walking { .. } => None,
                    };
                    if let Some(walk) = walk {
                        self.deadlines.cancel(Timer::ResolveTimeout { search });
                        if answer.is_none() {
                            effects.push(PartyEffect::Event(PartyEvent::ResolveFailed { timeout: false }));
                        }
                        let media = self.phase.seat().and_then(|seat| seat.media.descriptor().cloned()).unwrap_or(Json::Null);
                        let ranked = matching::rank(&media, answer.unwrap_or_default());
                        let started = Walk::new(&media, &ranked);
                        self.matching.candidates = ranked.iter().cloned().zip(started.deltas().iter().copied()).collect();
                        if walk {
                            self.matching.search = Search::Walking { walk: started, ranked, probe: 0, started_ms: now };
                            stack.push(Work::WalkOn);
                        } else {
                            self.matching.search = Search::Idle;
                        }
                    }
                }
                Work::Input(PartyInput::Probed { probe, duration_ms, .. }) => {
                    let search = std::mem::replace(&mut self.matching.search, Search::Idle);
                    match search {
                        Search::Walking { walk, ranked, probe: running, started_ms } if running == probe && probe != 0 => {
                            self.deadlines.cancel(Timer::ProbeDeadline { probe });
                            let walk = walk.answered(ProbeRun { duration_ms, took_ms: now - started_ms });
                            self.matching.candidates = ranked.iter().cloned().zip(walk.deltas().iter().copied()).collect();
                            self.matching.search = Search::Walking { walk, ranked, probe: 0, started_ms: now };
                            stack.push(Work::WalkOn);
                        }
                        other @ (Search::Idle | Search::Resolving { .. } | Search::Walking { .. }) => {
                            self.matching.search = other;
                            let choosing = std::mem::replace(&mut self.matching.choosing, Choosing::Idle);
                            match choosing {
                                Choosing::Probing { probe: running, call, candidate } if running == probe => match matching::counting(duration_ms) {
                                    Some(duration_ms) => stack.push(Work::ChooseWith { call, candidate, duration_ms }),
                                    None => effects.push(PartyEffect::Done { call, outcome: Outcome::Answer(false) }),
                                },
                                other @ (Choosing::Idle | Choosing::Probing { .. }) => self.matching.choosing = other,
                            }
                        }
                    }
                }
                Work::Input(PartyInput::Call { call, request, .. }) => {
                    if self.phase.closed() && request != Request::Close {
                        effects.push(PartyEffect::Done { call, outcome: failed("closed", "this WatchParty is closed") });
                        continue;
                    }
                    let connected = member_conn(&self.phase);
                    let seat_room = self.phase.seat().map(|seat| seat.room.clone());
                    let has_media = self.phase.seat().is_some_and(|seat| seat.media.descriptor().is_some());
                    let (is_host, can_control) = (self.view.is_host, self.view.can_control);
                    let bound_player = match (&self.attached, self.binding) {
                        (Attached::Player { player, can_set_rate, .. }, Binding::Bound { .. }) => Some((*player, *can_set_rate)),
                        (Attached::Player { .. } | Attached::None | Attached::Away, Binding::Unbound | Binding::Bound { .. }) => None,
                    };
                    let member = |needs_host: bool, needs_media: bool, needs_control: bool| -> Result<u64, PartyError> {
                        let conn = connected.ok_or_else(|| PartyError::new("not_in_room", "not in a room"))?;
                        if needs_host && !is_host {
                            return Err(PartyError::new("forbidden", "only the host may do this"));
                        }
                        if needs_media && !has_media {
                            return Err(PartyError::new("invalid_message", "the room has no media"));
                        }
                        if needs_control && !can_control {
                            return Err(PartyError::new("forbidden", "only a controller may do this"));
                        }
                        Ok(conn)
                    };
                    let busy = matches!(self.phase, Phase::Connecting(_) | Phase::InRoom(_) | Phase::Reconnecting(..));
                    match request {
                        Request::Create if busy => effects.push(PartyEffect::Done { call, outcome: failed("already_in_room", "the party is in a room or connecting") }),
                        Request::Create => {
                            stack.push(Work::Reset);
                            let urls: Vec<String> = self.relays.list.iter().map(|relay| relay.url.clone()).collect();
                            let mut conns = Vec::new();
                            for url in &urls {
                                let conn = self.ids.mint();
                                self.links.insert(conn, Link::new(url));
                                self.deadlines.set(now, CONNECT_TIMEOUT_MS, Timer::LinkOpen { conn });
                                effects.push(PartyEffect::Open { conn, url: url.clone() });
                                conns.push(conn);
                            }
                            let (step, untried) = if conns.len() == 1 {
                                (AttemptStep::Opening { conn: conns[0] }, Vec::new())
                            } else {
                                self.deadlines.set(now, CONNECT_TIMEOUT_MS, Timer::Race);
                                (AttemptStep::Racing { conns: conns.clone(), failed: 0 }, urls)
                            };
                            self.phase = Phase::Connecting(Attempt { call, goal: Goal::Create { untried }, step, links: conns });
                        }
                        Request::Join { code: typed, create } => match code::normalize(&typed) {
                            None => effects.push(PartyEffect::Done { call, outcome: failed("invalid_code", "not a room code") }),
                            Some(_) if busy => effects.push(PartyEffect::Done { call, outcome: failed("already_in_room", "the party is in a room or connecting") }),
                            Some(normalized) => {
                                stack.push(Work::Reset);
                                let relay = code::relay_for(&normalized, &self.relays.list).map(|relay| relay.url.clone());
                                let goal = Goal::Join { code: normalized, create, url: relay.clone().unwrap_or_default(), refreshed: relay.is_none() };
                                self.phase = Phase::Connecting(Attempt { call, goal, step: AttemptStep::Refreshing { get: 0, then: Refreshed::Route }, links: Vec::new() });
                                match relay {
                                    Some(url) => stack.push(Work::OpenJoin { url }),
                                    None if self.relays.public => {
                                        let get = self.ids.mint();
                                        self.fetches.insert(get, Fetch::Join);
                                        self.deadlines.set(now, FETCH_TIMEOUT_MS, Timer::Fetch { get });
                                        effects.push(PartyEffect::Get { get, url: relays::PUBLIC_LIST_URL.to_owned() });
                                        let mut phase = std::mem::take(&mut self.phase);
                                        if let Phase::Connecting(attempt) = &mut phase {
                                            attempt.step = AttemptStep::Refreshing { get, then: Refreshed::Route };
                                        }
                                        self.phase = phase;
                                    }
                                    None => stack.push(Work::Fetched { get: 0, ok: true }),
                                }
                            }
                        },
                        Request::Leave => then(&mut stack, vec![Work::Leave, Work::Done { call, outcome: Outcome::Done }]),
                        Request::Close => {
                            if self.phase.closed() {
                                stack.push(Work::DoneWhenSettled { call });
                            } else {
                                then(&mut stack, vec![Work::Leave, Work::ReleaseAdapter, Work::MarkClosed, Work::DoneWhenSettled { call }]);
                            }
                        }
                        Request::Refresh => {
                            if self.relays.public {
                                let get = self.ids.mint();
                                self.fetches.insert(get, Fetch::Call(call));
                                self.deadlines.set(now, FETCH_TIMEOUT_MS, Timer::Fetch { get });
                                effects.push(PartyEffect::Get { get, url: relays::PUBLIC_LIST_URL.to_owned() });
                            } else {
                                effects.push(PartyEffect::Done { call, outcome: Outcome::Answer(true) });
                            }
                        }
                        Request::Play => match member(false, true, true) {
                            Ok(conn) => {
                                effects.push(PartyEffect::Send { conn, text: wire(&Outgoing::Play) });
                                effects.push(PartyEffect::Done { call, outcome: Outcome::Done });
                            }
                            Err(error) => effects.push(PartyEffect::Done { call, outcome: Outcome::Failed(error) }),
                        },
                        Request::Pause => match member(false, true, true) {
                            Ok(conn) => {
                                let local = bound_player.and(self.reads.as_ref()).map(|reads| reads.position_ms.checked_sub(self.offset_ms).filter(|local| local.abs() <= json::MAX_SAFE_INT));
                                if local == Some(None) {
                                    effects.push(PartyEffect::Done { call, outcome: failed("invalid_message", "the player's position minus the offset is not an int") });
                                    continue;
                                }
                                let mut seq = Vec::new();
                                if let Some((player, _)) = bound_player {
                                    seq.push(Work::Enqueue { player, job: Job { action: PlayerAction::Pause, kind: JobKind::Command } });
                                }
                                let position_ms = local.flatten().map(|local| local.max(0));
                                seq.push(Work::SendText { conn, text: wire(&Outgoing::Pause { position_ms }) });
                                seq.push(Work::HoldFor { re: "pause" });
                                seq.push(Work::Done { call, outcome: Outcome::Done });
                                then(&mut stack, seq);
                            }
                            Err(error) => effects.push(PartyEffect::Done { call, outcome: Outcome::Failed(error) }),
                        },
                        Request::Seek { position_ms } => match member(false, true, true) {
                            Ok(conn) => {
                                let local = position_ms.checked_sub(self.offset_ms).filter(|local| local.abs() <= json::MAX_SAFE_INT);
                                let (true, Some(local)) = ((0..=json::MAX_SAFE_INT).contains(&position_ms), local) else {
                                    effects.push(PartyEffect::Done { call, outcome: failed("invalid_message", "positionMs must be an integer ≥ 0") });
                                    continue;
                                };
                                let mut seq = Vec::new();
                                if let Some((player, _)) = bound_player {
                                    self.attached = with_seek(std::mem::replace(&mut self.attached, Attached::None), Some(now));
                                    seq.push(Work::Enqueue { player, job: Job { action: PlayerAction::Seek { position_ms }, kind: JobKind::Seek } });
                                }
                                seq.push(Work::SendText { conn, text: wire(&Outgoing::Seek { position_ms: local.max(0) }) });
                                seq.push(Work::HoldFor { re: "seek" });
                                if bound_player.is_some() {
                                    seq.push(Work::Changed);
                                }
                                seq.push(Work::Done { call, outcome: Outcome::Done });
                                then(&mut stack, seq);
                            }
                            Err(error) => effects.push(PartyEffect::Done { call, outcome: Outcome::Failed(error) }),
                        },
                        Request::SetRate { rate } => match member(false, true, true) {
                            Ok(conn) => {
                                if !(MIN_RATE..=MAX_RATE).contains(&rate) {
                                    effects.push(PartyEffect::Done { call, outcome: failed("invalid_message", "rate must be between 0.5 and 2.0") });
                                    continue;
                                }
                                let mut seq = Vec::new();
                                if let Some((player, true)) = bound_player {
                                    if let Binding::Bound { nudging, blocked, .. } = self.binding {
                                        self.binding = Binding::Bound { applied_rate: rate, nudging, blocked };
                                    }
                                    seq.push(Work::Enqueue { player, job: Job { action: PlayerAction::SetRate { rate }, kind: JobKind::Command } });
                                }
                                seq.push(Work::SendText { conn, text: wire(&Outgoing::Rate { rate }) });
                                seq.push(Work::HoldFor { re: "rate" });
                                seq.push(Work::Done { call, outcome: Outcome::Done });
                                then(&mut stack, seq);
                            }
                            Err(error) => effects.push(PartyEffect::Done { call, outcome: Outcome::Failed(error) }),
                        },
                        Request::SetOffset { offset_ms } => {
                            if offset_ms.abs() > MAX_OFFSET_MS {
                                effects.push(PartyEffect::Done { call, outcome: failed("invalid_message", "offsetMs must be within ±604800000") });
                            } else {
                                self.offset_ms = offset_ms;
                                then(&mut stack, vec![Work::Changed, Work::Step, Work::Done { call, outcome: Outcome::Done }]);
                            }
                        }
                        Request::SetMedia { media: descriptor, reference, position_ms } => match member(true, false, false) {
                            Ok(conn) => {
                                if media::check(&descriptor, Direction::Send).is_err() {
                                    effects.push(PartyEffect::Done { call, outcome: failed("invalid_message", "media fails the send checks") });
                                    continue;
                                }
                                if !(0..=json::MAX_SAFE_INT).contains(&position_ms) {
                                    effects.push(PartyEffect::Done { call, outcome: failed("invalid_message", "positionMs must be an integer ≥ 0") });
                                    continue;
                                }
                                let own = OwnFile { media: media::known_members(&descriptor), reference, position_ms };
                                let text = wire(&Outgoing::SetMedia { media: own.media.clone(), position_ms: Some(position_ms) });
                                let mut phase = std::mem::take(&mut self.phase);
                                if let Some(seat) = phase.seat_mut() {
                                    seat.own = Own::Pending(own);
                                }
                                self.phase = phase;
                                then(&mut stack, vec![Work::Rebind, Work::SendText { conn, text }, Work::Changed, Work::Done { call, outcome: Outcome::Done }]);
                            }
                            Err(error) => effects.push(PartyEffect::Done { call, outcome: Outcome::Failed(error) }),
                        },
                        Request::ClearMedia | Request::End | Request::SetControl { .. } | Request::Lock { .. } => match member(true, false, false) {
                            Ok(conn) => {
                                let message = match request {
                                    Request::SetControl { mode } => Outgoing::SetControl { mode },
                                    Request::Lock { locked } => Outgoing::Lock { locked },
                                    Request::End => Outgoing::End,
                                    Request::ClearMedia
                                    | Request::Create
                                    | Request::Join { .. }
                                    | Request::Leave
                                    | Request::Close
                                    | Request::SetMedia { .. }
                                    | Request::Play
                                    | Request::Pause
                                    | Request::Seek { .. }
                                    | Request::SetRate { .. }
                                    | Request::SetOffset { .. }
                                    | Request::Kick { .. }
                                    | Request::TransferHost { .. }
                                    | Request::Choose { .. }
                                    | Request::AcceptMismatch
                                    | Request::DeclineMismatch
                                    | Request::ResolveAgain
                                    | Request::AttachPlayer { .. }
                                    | Request::DetachPlayer { .. }
                                    | Request::PlayerChanged
                                    | Request::Refresh => Outgoing::ClearMedia,
                                };
                                effects.push(PartyEffect::Send { conn, text: wire(&message) });
                                effects.push(PartyEffect::Done { call, outcome: Outcome::Done });
                            }
                            Err(error) => effects.push(PartyEffect::Done { call, outcome: Outcome::Failed(error) }),
                        },
                        Request::Kick { peer_id } | Request::TransferHost { peer_id } if member(true, false, false).is_ok() && !seat_room.as_ref().is_some_and(|room| view::has_peer(room, &peer_id)) => {
                            effects.push(PartyEffect::Done { call, outcome: failed("invalid_message", "no such participant") });
                        }
                        Request::Kick { peer_id } => match member(true, false, false) {
                            Ok(conn) => match (Outgoing::Kick { peer_id }).encode() {
                                Ok(text) => {
                                    effects.push(PartyEffect::Send { conn, text });
                                    effects.push(PartyEffect::Done { call, outcome: Outcome::Done });
                                }
                                Err(invalid) => effects.push(PartyEffect::Done { call, outcome: failed("invalid_message", invalid.0) }),
                            },
                            Err(error) => effects.push(PartyEffect::Done { call, outcome: Outcome::Failed(error) }),
                        },
                        Request::TransferHost { peer_id } => match member(true, false, false) {
                            Ok(conn) => match (Outgoing::TransferHost { peer_id }).encode() {
                                Ok(text) => {
                                    effects.push(PartyEffect::Send { conn, text });
                                    effects.push(PartyEffect::Done { call, outcome: Outcome::Done });
                                }
                                Err(invalid) => effects.push(PartyEffect::Done { call, outcome: failed("invalid_message", invalid.0) }),
                            },
                            Err(error) => effects.push(PartyEffect::Done { call, outcome: Outcome::Failed(error) }),
                        },
                        Request::Choose { candidate } => {
                            if self.phase.seat().and_then(|seat| seat.media.descriptor()).is_none() {
                                effects.push(PartyEffect::Done { call, outcome: Outcome::Answer(false) });
                                continue;
                            }
                            if let Choosing::Probing { probe, call: earlier, .. } = std::mem::replace(&mut self.matching.choosing, Choosing::Idle) {
                                effects.push(PartyEffect::CancelProbe { probe });
                                effects.push(PartyEffect::Done { call: earlier, outcome: Outcome::Answer(false) });
                            }
                            match (matching::counting(candidate.duration_ms), candidate.probe) {
                                (Some(duration_ms), _) => stack.push(Work::ChooseWith { call, candidate, duration_ms }),
                                (None, true) => {
                                    let probe = self.ids.mint();
                                    effects.push(PartyEffect::Probe { probe, candidate: candidate.id.clone() });
                                    self.matching.choosing = Choosing::Probing { probe, call, candidate };
                                }
                                (None, false) => effects.push(PartyEffect::Done { call, outcome: Outcome::Answer(false) }),
                            }
                        }
                        Request::AcceptMismatch => {
                            let mut seq = Vec::new();
                            if let Pick::Offered { candidate, delta_ms } = std::mem::replace(&mut self.matching.pick, Pick::None) {
                                self.matching.pick = Pick::Chosen(candidate);
                                self.matching.report = report(MatchState::Mismatched, Some(delta_ms));
                                seq.extend([Work::Rebind, Work::Changed, Work::Step]);
                            } else {
                                self.matching.pick = self.view.choice.clone().map_or(Pick::None, Pick::Chosen);
                            }
                            seq.push(Work::Done { call, outcome: Outcome::Done });
                            then(&mut stack, seq);
                        }
                        Request::DeclineMismatch => {
                            let mut seq = Vec::new();
                            if let Pick::Offered { delta_ms, .. } = std::mem::replace(&mut self.matching.pick, Pick::None) {
                                self.matching.report = report(MatchState::Unavailable, Some(delta_ms));
                                seq.extend([Work::Rebind, Work::Changed]);
                            } else {
                                self.matching.pick = self.view.choice.clone().map_or(Pick::None, Pick::Chosen);
                            }
                            seq.push(Work::Done { call, outcome: Outcome::Done });
                            then(&mut stack, seq);
                        }
                        Request::ResolveAgain => {
                            let theirs = self.phase.seat().is_some_and(|seat| matches!(seat.media, Media::Theirs(_)));
                            let mut seq = Vec::new();
                            if theirs {
                                seq.push(Work::CancelMatching);
                                seq.push(if chosen(&self.matching.pick).is_none() { Work::RestartSearch } else { Work::StartMatching { walk: false } });
                                seq.push(Work::Changed);
                            }
                            seq.push(Work::Done { call, outcome: Outcome::Done });
                            then(&mut stack, seq);
                        }
                        Request::AttachPlayer { player, reference, can_set_rate } => {
                            let same = attached_player(&self.attached) == Some(player);
                            let mut seq = Vec::new();
                            if !same {
                                seq.push(Work::ReleaseAdapter);
                            }
                            seq.extend([
                                Work::Attach { player, reference, can_set_rate, same },
                                Work::Rebind,
                                Work::SettleReleased,
                                Work::Changed,
                                Work::Step,
                                Work::Done { call, outcome: Outcome::Done },
                            ]);
                            then(&mut stack, seq);
                        }
                        Request::DetachPlayer { player } => {
                            let current = attached_player(&self.attached);
                            if player.is_none() || player == current {
                                then(&mut stack, vec![Work::ReleaseAdapter, Work::GoAway, Work::Changed, Work::DoneWhenSettled { call }]);
                            } else {
                                stack.push(Work::DoneWhenSettled { call });
                            }
                        }
                        Request::PlayerChanged => then(&mut stack, vec![Work::Changed, Work::Step, Work::Done { call, outcome: Outcome::Done }]),
                    }
                }
                Work::Timer(timer) => match timer {
                    Timer::LinkOpen { conn } => {
                        if let Some(link) = self.links.get_mut(&conn) {
                            link.step(LinkEvent::Lost, &mut Vec::new());
                        }
                        self.deadlines.cancel_where(deadlines::of_conn, conn);
                        stack.push(Work::Lost { conn, code: None });
                    }
                    Timer::LinkPing { conn } => {
                        let mut fx = Vec::new();
                        if let Some(link) = self.links.get_mut(&conn) {
                            link.step(LinkEvent::PingDue { at_ms: now }, &mut fx);
                        }
                        stack.push(Work::LinkFx { conn, fx });
                    }
                    Timer::LinkSilence { conn } => {
                        let mut fx = Vec::new();
                        if let Some(link) = self.links.get_mut(&conn) {
                            link.step(LinkEvent::Close, &mut fx);
                        }
                        then(&mut stack, vec![Work::LinkFx { conn, fx }, Work::Lost { conn, code: None }]);
                    }
                    Timer::LinkAnswer { conn } => {
                        if self.links.get(&conn).is_some_and(Link::lingers) {
                            let mut fx = Vec::new();
                            if let Some(link) = self.links.get_mut(&conn) {
                                link.step(LinkEvent::Close, &mut fx);
                            }
                            stack.push(Work::LinkFx { conn, fx });
                        } else {
                            let resuming = matches!(&self.phase, Phase::Reconnecting(_, Resume { step: ResumeStep::Asking { conn: asked, .. }, .. }) if *asked == conn);
                            let re = match &self.phase {
                                Phase::Connecting(Attempt { step: AttemptStep::Asking { conn: asked, hello }, .. }) if *asked == conn => Some(hello.re()),
                                Phase::Idle | Phase::Connecting(_) | Phase::InRoom(_) | Phase::Reconnecting(..) | Phase::Ended(_) | Phase::Closed(_) => None,
                            };
                            if resuming {
                                stack.push(Work::ResumeFailed { conn, error: PartyError::new("unreachable", "no answer to resume") });
                            } else if let Some(re) = re {
                                stack.push(Work::FailAttempt { error: PartyError::new("unreachable", &format!("no answer to {re}")) });
                            }
                        }
                    }
                    Timer::Race => {
                        if let Phase::Connecting(Attempt { step: AttemptStep::Racing { conns, .. }, .. }) = &self.phase {
                            let mut seq = Vec::new();
                            for conn in conns.clone() {
                                let mut fx = Vec::new();
                                if let Some(link) = self.links.get_mut(&conn) {
                                    link.step(LinkEvent::Close, &mut fx);
                                }
                                seq.push(Work::LinkFx { conn, fx });
                            }
                            seq.push(Work::FailAttempt { error: PartyError::new("unreachable", "no relay answered a ping within 10000 ms") });
                            then(&mut stack, seq);
                        }
                    }
                    Timer::Fetch { get } => {
                        effects.push(PartyEffect::AbortGet { get });
                        if self.fetches.contains_key(&get) {
                            stack.push(Work::Fetched { get, ok: false });
                        }
                    }
                    Timer::Tick => {
                        self.deadlines.set(now, TICK_MS, Timer::Tick);
                        then(&mut stack, vec![Work::Changed, Work::Step]);
                    }
                    Timer::Play => stack.push(Work::Step),
                    Timer::Hold => {
                        self.hold = Hold::None;
                        stack.push(Work::Step);
                    }
                    Timer::StatusSpacing => stack.push(Work::QueueStatus { force: false }),
                    Timer::Heartbeat => stack.push(Work::QueueStatus { force: true }),
                    Timer::Backoff => {
                        let url = match &self.phase {
                            Phase::Reconnecting(seat, Resume { step: ResumeStep::Waiting, .. }) => Some(seat.session.url.clone()),
                            Phase::Idle | Phase::Connecting(_) | Phase::InRoom(_) | Phase::Reconnecting(..) | Phase::Ended(_) | Phase::Closed(_) => None,
                        };
                        if let Some(url) = url {
                            let conn = self.ids.mint();
                            self.links.insert(conn, Link::new(&url));
                            self.deadlines.set(now, CONNECT_TIMEOUT_MS, Timer::LinkOpen { conn });
                            effects.push(PartyEffect::Open { conn, url });
                            let mut phase = std::mem::take(&mut self.phase);
                            if let Phase::Reconnecting(_, resume) = &mut phase {
                                resume.step = ResumeStep::Opening { conn };
                            }
                            self.phase = phase;
                        }
                    }
                    Timer::ResumeWindow => stack.push(Work::End { reason: EndReason::Lost, error: Some(PartyError::new("unreachable", "could not resume within 300000 ms")) }),
                    Timer::FirstSample { conn } => stack.push(Work::ResumeFailed { conn, error: PartyError::new("unreachable", "no clock sample within the connect timeout of sending resume") }),
                    Timer::ResolveTimeout { search } => {
                        let running = matches!(&self.matching.search, Search::Resolving { search: running, .. } if *running == search);
                        if running {
                            effects.push(PartyEffect::CancelResolve { search });
                            effects.push(PartyEffect::Event(PartyEvent::ResolveFailed { timeout: true }));
                            let at = Moment { now_ms: now, player: self.reads.as_ref().cloned() };
                            stack.push(Work::Input(PartyInput::Resolved { at, search, answer: Some(Vec::new()) }));
                        }
                    }
                    Timer::ProbeDeadline { probe } => {
                        let search = std::mem::replace(&mut self.matching.search, Search::Idle);
                        match search {
                            Search::Walking { walk, ranked, probe: running, started_ms } if running == probe => {
                                effects.push(PartyEffect::CancelProbe { probe });
                                let walk = walk.answered(ProbeRun { duration_ms: None, took_ms: now - started_ms });
                                self.matching.candidates = ranked.iter().cloned().zip(walk.deltas().iter().copied()).collect();
                                self.matching.search = Search::Walking { walk, ranked, probe: 0, started_ms: now };
                                stack.push(Work::WalkOn);
                            }
                            other @ (Search::Idle | Search::Resolving { .. } | Search::Walking { .. }) => self.matching.search = other,
                        }
                    }
                },
                Work::LinkFx { conn, fx } => {
                    let mut seq = Vec::new();
                    for effect in fx {
                        match effect {
                            LinkEffect::Ping { id } => effects.push(PartyEffect::Send { conn, text: wire(&Outgoing::Ping { id: PingId::Number(id) }) }),
                            LinkEffect::PingAt { at_ms } => self.deadlines.set(now, at_ms - now, Timer::LinkPing { conn }),
                            LinkEffect::SilenceAt { at_ms } => self.deadlines.set(now, at_ms - now, Timer::LinkSilence { conn }),
                            LinkEffect::Ponged => seq.push(Work::Ponged { conn }),
                            LinkEffect::Closed { opened } => {
                                self.deadlines.cancel_where(deadlines::of_conn, conn);
                                if opened {
                                    effects.push(PartyEffect::Close { conn, code: 1000 });
                                }
                            }
                        }
                    }
                    then(&mut stack, seq);
                }
                Work::Ponged { conn } => {
                    let sampled = self.links.get(&conn).and_then(Link::best).is_some();
                    if !sampled {
                        continue;
                    }
                    match &self.phase {
                        Phase::Connecting(Attempt { step: AttemptStep::Racing { conns, .. }, .. }) if conns.contains(&conn) => {
                            self.deadlines.cancel(Timer::Race);
                            let mut seq = Vec::new();
                            for other in conns.clone().into_iter().filter(|other| *other != conn) {
                                let mut fx = Vec::new();
                                if let Some(link) = self.links.get_mut(&other) {
                                    link.step(LinkEvent::Close, &mut fx);
                                }
                                seq.push(Work::LinkFx { conn: other, fx });
                            }
                            let url = self.links.get(&conn).map(|link| link.url().to_owned()).unwrap_or_default();
                            let mut phase = std::mem::take(&mut self.phase);
                            if let Phase::Connecting(attempt) = &mut phase {
                                if let Goal::Create { untried } = &mut attempt.goal {
                                    if let Some(at) = untried.iter().position(|tried| *tried == url) {
                                        untried.remove(at);
                                    }
                                }
                                attempt.step = AttemptStep::Opening { conn };
                            }
                            self.phase = phase;
                            seq.push(Work::Hello { conn, hello: Hello::Create });
                            then(&mut stack, seq);
                        }
                        Phase::Reconnecting(_, Resume { step: ResumeStep::Sampling { conn: sampling, answer }, .. }) if *sampling == conn => {
                            let answer = answer.clone();
                            self.deadlines.cancel(Timer::FirstSample { conn });
                            self.deadlines.cancel(Timer::ResumeWindow);
                            stack.push(Work::Enter { conn, answer, done: None });
                        }
                        Phase::Idle | Phase::Connecting(_) | Phase::InRoom(_) | Phase::Reconnecting(..) | Phase::Ended(_) | Phase::Closed(_) => {}
                    }
                }
                Work::Text { conn, text } => match decode(&text) {
                    Received::Message(Incoming::Pong { id: PingId::Number(id), server_time }) => {
                        let mut fx = Vec::new();
                        if let Some(link) = self.links.get_mut(&conn) {
                            link.step(LinkEvent::Pong { id, server_time, at_ms: now }, &mut fx);
                        }
                        stack.push(Work::LinkFx { conn, fx });
                    }
                    Received::Message(Incoming::Pong { id: PingId::Text(_), .. }) | Received::Malformed("pong") => {}
                    received @ (Received::Message(Incoming::Joined { .. } | Incoming::Room { .. } | Incoming::Error { .. } | Incoming::Closed { .. }) | Received::Malformed(_) | Received::Ignored | Received::TooLarge) => {
                        stack.push(Work::Routed { conn, text, received });
                    }
                },
                Work::Routed { conn, text, received } => {
                    let lingering = self.links.get(&conn).map(Link::state);
                    if let Some(LinkState::Lingering { hello, .. }) = lingering {
                        if answers(&received, hello.re()) {
                            if matches!(received, Received::Message(Incoming::Joined { .. }) | Received::Malformed("joined")) {
                                effects.push(PartyEffect::Send { conn, text: wire(&Outgoing::Leave) });
                            }
                            let mut fx = Vec::new();
                            if let Some(link) = self.links.get_mut(&conn) {
                                link.step(LinkEvent::Close, &mut fx);
                            }
                            stack.push(Work::LinkFx { conn, fx });
                        }
                        continue;
                    }
                    match &self.phase {
                        Phase::Connecting(Attempt { step: AttemptStep::Asking { conn: asked, hello }, call, goal, .. }) if *asked == conn => {
                            let (hello, call) = (*hello, *call);
                            match &received {
                                Received::Message(message @ Incoming::Joined { .. }) => {
                                    if let Some(answer) = answer_of(message) {
                                        if let Some(link) = self.links.get_mut(&conn) {
                                            link.step(LinkEvent::Joined, &mut Vec::new());
                                        }
                                        let done = match goal {
                                            Goal::Create { .. } => Outcome::Code(answer.code.clone()),
                                            Goal::Join { .. } => Outcome::Done,
                                        };
                                        stack.push(Work::Enter { conn, answer, done: Some((call, done)) });
                                    }
                                }
                                Received::Malformed("joined") => {
                                    effects.push(PartyEffect::Send { conn, text: wire(&Outgoing::Leave) });
                                    let mut fx = Vec::new();
                                    if let Some(link) = self.links.get_mut(&conn) {
                                        link.step(LinkEvent::Close, &mut fx);
                                    }
                                    let error = PartyError::new("invalid_message", &format!("the relay answered {} with a malformed joined", hello.re()));
                                    then(&mut stack, vec![Work::LinkFx { conn, fx }, Work::FailAttempt { error }]);
                                }
                                Received::Message(message @ Incoming::Error { re: Some(re), .. }) if re == hello.re() => {
                                    if let Some(error) = error_of(message) {
                                        stack.push(Work::HelloRefused { conn, error });
                                    }
                                }
                                Received::Message(Incoming::Error { .. } | Incoming::Room { .. } | Incoming::Pong { .. } | Incoming::Closed { .. }) | Received::Malformed(_) | Received::Ignored | Received::TooLarge => {}
                            }
                        }
                        Phase::InRoom(seat) if seat.conn == conn => {
                            if let Received::Message(message) = received {
                                stack.push(Work::Receive { message });
                            }
                        }
                        Phase::Reconnecting(_, Resume { step: ResumeStep::Asking { conn: asked, sent_at_ms }, .. }) if *asked == conn => {
                            let sent_at_ms = *sent_at_ms;
                            match &received {
                                Received::Message(message @ Incoming::Joined { .. }) => {
                                    if let Some(answer) = answer_of(message) {
                                        if let Some(link) = self.links.get_mut(&conn) {
                                            link.step(LinkEvent::Joined, &mut Vec::new());
                                        }
                                        self.deadlines.cancel(Timer::LinkAnswer { conn });
                                        if self.links.get(&conn).and_then(Link::best).is_some() {
                                            self.deadlines.cancel(Timer::ResumeWindow);
                                            stack.push(Work::Enter { conn, answer, done: None });
                                        } else {
                                            self.deadlines.set(now, sent_at_ms + CONNECT_TIMEOUT_MS - now, Timer::FirstSample { conn });
                                            let mut phase = std::mem::take(&mut self.phase);
                                            if let Phase::Reconnecting(_, resume) = &mut phase {
                                                resume.step = ResumeStep::Sampling { conn, answer };
                                            }
                                            self.phase = phase;
                                        }
                                    }
                                }
                                Received::Malformed("joined") => {
                                    stack.push(Work::ResumeFailed { conn, error: PartyError::new("invalid_message", "the relay answered resume with a malformed joined") });
                                }
                                Received::Message(message @ Incoming::Error { re: Some(re), .. }) if re == "resume" => {
                                    if let Some(error) = error_of(message) {
                                        stack.push(Work::ResumeFailed { conn, error });
                                    }
                                }
                                Received::Message(Incoming::Error { .. } | Incoming::Room { .. } | Incoming::Pong { .. } | Incoming::Closed { .. }) | Received::Malformed(_) | Received::Ignored | Received::TooLarge => {}
                            }
                        }
                        Phase::Reconnecting(_, Resume { step: ResumeStep::Sampling { conn: sampling, .. }, .. }) if *sampling == conn => {
                            let mut phase = std::mem::take(&mut self.phase);
                            if let Phase::Reconnecting(_, Resume { step: ResumeStep::Sampling { answer, .. }, .. }) = &mut phase {
                                answer.buffered.push(text);
                            }
                            self.phase = phase;
                        }
                        Phase::Idle | Phase::Connecting(_) | Phase::InRoom(_) | Phase::Reconnecting(..) | Phase::Ended(_) | Phase::Closed(_) => {}
                    }
                }
                Work::Hello { conn, hello } => {
                    if let Some(link) = self.links.get_mut(&conn) {
                        link.step(LinkEvent::Asked { hello }, &mut Vec::new());
                    }
                    self.deadlines.set(now, CONNECT_TIMEOUT_MS, Timer::LinkAnswer { conn });
                    let mut phase = std::mem::take(&mut self.phase);
                    let message = match &mut phase {
                        Phase::Connecting(attempt) => {
                            attempt.step = AttemptStep::Asking { conn, hello };
                            match &attempt.goal {
                                Goal::Create { .. } => Some(Outgoing::Create { name: self.name.clone(), client: self.client.clone() }),
                                Goal::Join { code, create, .. } => Some(Outgoing::Join { code: code.clone(), name: self.name.clone(), client: self.client.clone(), create: *create }),
                            }
                        }
                        Phase::Reconnecting(seat, resume) => {
                            resume.step = ResumeStep::Asking { conn, sent_at_ms: now };
                            Some(Outgoing::Resume { code: seat.session.code.clone(), peer_id: seat.session.peer_id.clone(), token: seat.session.token.clone() })
                        }
                        Phase::Idle | Phase::InRoom(_) | Phase::Ended(_) | Phase::Closed(_) => None,
                    };
                    self.phase = phase;
                    if let Some(message) = message {
                        effects.push(PartyEffect::Send { conn, text: wire(&message) });
                    }
                }
                Work::HelloRefused { conn, error } => {
                    let goal = match &self.phase {
                        Phase::Connecting(attempt) => Some(attempt.goal.clone()),
                        Phase::Idle | Phase::InRoom(_) | Phase::Reconnecting(..) | Phase::Ended(_) | Phase::Closed(_) => None,
                    };
                    let mut fx = Vec::new();
                    if let Some(link) = self.links.get_mut(&conn) {
                        link.step(LinkEvent::Close, &mut fx);
                    }
                    let mut seq = vec![Work::LinkFx { conn, fx }];
                    match goal {
                        Some(Goal::Create { untried }) if error.code == "relay_full" && !untried.is_empty() => {
                            let url = untried[0].clone();
                            let next = self.ids.mint();
                            self.links.insert(next, Link::new(&url));
                            self.deadlines.set(now, CONNECT_TIMEOUT_MS, Timer::LinkOpen { conn: next });
                            effects.push(PartyEffect::Open { conn: next, url });
                            let mut phase = std::mem::take(&mut self.phase);
                            if let Phase::Connecting(attempt) = &mut phase {
                                if let Goal::Create { untried } = &mut attempt.goal {
                                    untried.remove(0);
                                }
                                attempt.step = AttemptStep::Opening { conn: next };
                                attempt.links.push(next);
                            }
                            self.phase = phase;
                        }
                        Some(Goal::Join { refreshed: false, .. }) if error.code == "room_not_found" => {
                            if self.relays.public {
                                let get = self.ids.mint();
                                self.fetches.insert(get, Fetch::Join);
                                self.deadlines.set(now, FETCH_TIMEOUT_MS, Timer::Fetch { get });
                                effects.push(PartyEffect::Get { get, url: relays::PUBLIC_LIST_URL.to_owned() });
                                let mut phase = std::mem::take(&mut self.phase);
                                if let Phase::Connecting(attempt) = &mut phase {
                                    attempt.step = AttemptStep::Refreshing { get, then: Refreshed::Moved { error } };
                                }
                                self.phase = phase;
                            } else {
                                let mut phase = std::mem::take(&mut self.phase);
                                if let Phase::Connecting(attempt) = &mut phase {
                                    attempt.step = AttemptStep::Refreshing { get: 0, then: Refreshed::Moved { error } };
                                }
                                self.phase = phase;
                                seq.push(Work::Fetched { get: 0, ok: true });
                            }
                        }
                        Some(Goal::Create { .. } | Goal::Join { .. }) | None => seq.push(Work::FailAttempt { error }),
                    }
                    then(&mut stack, seq);
                }
                Work::FailAttempt { error } => {
                    let attempt = match std::mem::take(&mut self.phase) {
                        Phase::Connecting(attempt) => Some(attempt),
                        other @ (Phase::Idle | Phase::InRoom(_) | Phase::Reconnecting(..) | Phase::Ended(_) | Phase::Closed(_)) => {
                            self.phase = other;
                            None
                        }
                    };
                    if let Some(attempt) = attempt {
                        let mut seq = Vec::new();
                        for conn in attempt.links {
                            if self.links.get(&conn).is_some_and(|link| !link.lingers()) {
                                let mut fx = Vec::new();
                                if let Some(link) = self.links.get_mut(&conn) {
                                    link.step(LinkEvent::Close, &mut fx);
                                }
                                seq.push(Work::LinkFx { conn, fx });
                            }
                        }
                        self.phase = Phase::Idle;
                        seq.push(Work::Done { call: attempt.call, outcome: Outcome::Failed(error) });
                        then(&mut stack, seq);
                    }
                }
                Work::OpenJoin { url } => {
                    let conn = self.ids.mint();
                    self.links.insert(conn, Link::new(&url));
                    self.deadlines.set(now, CONNECT_TIMEOUT_MS, Timer::LinkOpen { conn });
                    effects.push(PartyEffect::Open { conn, url: url.clone() });
                    let mut phase = std::mem::take(&mut self.phase);
                    if let Phase::Connecting(attempt) = &mut phase {
                        attempt.step = AttemptStep::Opening { conn };
                        attempt.links.push(conn);
                        if let Goal::Join { url: target, .. } = &mut attempt.goal {
                            *target = url;
                        }
                    }
                    self.phase = phase;
                }
                Work::Fetched { get, ok } => {
                    let purpose = self.fetches.remove(&get).unwrap_or(Fetch::Join);
                    match purpose {
                        Fetch::Call(call) => effects.push(PartyEffect::Done { call, outcome: Outcome::Answer(ok) }),
                        Fetch::Background => {}
                        Fetch::Join => {
                            let next = match &self.phase {
                                Phase::Connecting(Attempt { step: AttemptStep::Refreshing { get: waiting, then }, goal: Goal::Join { code, url, .. }, .. }) if *waiting == get => {
                                    let routed = code::relay_for(code, &self.relays.list).map(|relay| relay.url.clone());
                                    Some(match (then, routed) {
                                        (Refreshed::Route, Some(url)) => Work::OpenJoin { url },
                                        (Refreshed::Route, None) => Work::FailAttempt { error: PartyError::new("room_not_found", &format!("no relay owns {}", code.chars().next().unwrap_or_default())) },
                                        (Refreshed::Moved { error }, Some(moved)) if moved != *url => Work::OpenJoin { url: moved },
                                        (Refreshed::Moved { error }, Some(_) | None) => Work::FailAttempt { error: error.clone() },
                                    })
                                }
                                Phase::Idle | Phase::Connecting(_) | Phase::InRoom(_) | Phase::Reconnecting(..) | Phase::Ended(_) | Phase::Closed(_) => None,
                            };
                            if let Some(next) = next {
                                let mut phase = std::mem::take(&mut self.phase);
                                if let Phase::Connecting(Attempt { goal: Goal::Join { refreshed, .. }, .. }) = &mut phase {
                                    *refreshed = true;
                                }
                                self.phase = phase;
                                stack.push(next);
                            }
                        }
                    }
                }
                Work::Lost { conn, code } => {
                    let lingered = self.links.get(&conn).is_some_and(|link| matches!(link.state(), LinkState::Lingering { .. }));
                    if lingered {
                        continue;
                    }
                    let url = self.links.get(&conn).map(|link| link.url().to_owned()).unwrap_or_default();
                    match &self.phase {
                        Phase::Connecting(Attempt { step: AttemptStep::Opening { conn: opening }, .. }) if *opening == conn => {
                            stack.push(Work::FailAttempt { error: PartyError::new("unreachable", &format!("could not connect to {url}")) });
                        }
                        Phase::Connecting(Attempt { step: AttemptStep::Asking { conn: asked, hello }, .. }) if *asked == conn => {
                            let re = hello.re();
                            stack.push(Work::FailAttempt { error: PartyError::new("unreachable", &format!("the relay closed the connection before answering {re}")) });
                        }
                        Phase::Connecting(Attempt { step: AttemptStep::Racing { conns, failed }, .. }) if conns.contains(&conn) => {
                            let (count, failed) = (conns.len(), failed + 1);
                            let mut phase = std::mem::take(&mut self.phase);
                            if let Phase::Connecting(Attempt { step: AttemptStep::Racing { failed: counted, .. }, .. }) = &mut phase {
                                *counted = failed;
                            }
                            self.phase = phase;
                            if failed == count {
                                self.deadlines.cancel(Timer::Race);
                                stack.push(Work::FailAttempt { error: PartyError::new("unreachable", "every relay connection failed") });
                            }
                        }
                        Phase::InRoom(seat) if seat.conn == conn => stack.push(Work::Dropped { code }),
                        Phase::Reconnecting(_, Resume { step: ResumeStep::Opening { conn: resumed } | ResumeStep::Asking { conn: resumed, .. } | ResumeStep::Sampling { conn: resumed, .. }, .. }) if *resumed == conn => {
                            stack.push(Work::ResumeFailed { conn, error: PartyError::new("unreachable", "the relay closed the connection") });
                        }
                        Phase::Idle | Phase::Connecting(_) | Phase::InRoom(_) | Phase::Reconnecting(..) | Phase::Ended(_) | Phase::Closed(_) => {}
                    }
                }
                Work::Enter { conn, answer, done } => {
                    self.deadlines.cancel(Timer::LinkAnswer { conn });
                    let session = Session { url: self.links.get(&conn).map(|link| link.url().to_owned()).unwrap_or_default(), code: answer.code.clone(), peer_id: answer.peer_id.clone(), token: answer.token.clone() };
                    let media = answer.media.clone().map_or(Media::None, Media::Theirs);
                    let mut seq = Vec::new();
                    let mut before = None;
                    let mut resend = None;
                    let phase = std::mem::take(&mut self.phase);
                    let seat = match phase {
                        Phase::Reconnecting(mut seat, _) => {
                            before = Some(seat.room.clone());
                            seat.conn = conn;
                            seat.session = session;
                            let epoch = view::epoch(&answer.room);
                            seat.room = answer.room.clone();
                            if epoch != seat.epoch {
                                seat.epoch = epoch;
                                seat.media = media;
                                seat.own = std::mem::take(&mut seat.own).settle();
                                seq.push(Work::NewEpoch);
                            } else if let Own::Pending(file) = &seat.own {
                                if view::is_host(&answer.room, &answer.peer_id) {
                                    resend = Some(file.clone());
                                } else {
                                    seat.own = std::mem::take(&mut seat.own).settle();
                                    seq.push(Work::Rebind);
                                }
                            }
                            seat
                        }
                        Phase::Idle | Phase::Connecting(_) | Phase::InRoom(_) | Phase::Ended(_) | Phase::Closed(_) => {
                            seq.push(Work::NewEpoch);
                            Seat { conn, session, room: answer.room.clone(), media, epoch: view::epoch(&answer.room), own: Own::None }
                        }
                    };
                    self.phase = Phase::InRoom(seat);
                    self.status = Status::Live { sent: None, force: false };
                    seq.push(Work::EnterRest { conn, before, resend, buffered: answer.buffered, done });
                    then(&mut stack, seq);
                }
                Work::EnterRest { conn, before, resend, buffered, done } => {
                    if !self.deadlines.is_set(Timer::Tick) {
                        self.deadlines.set(now, TICK_MS, Timer::Tick);
                    }
                    let mut seq = vec![Work::RefreshPlayer];
                    if let (Some(before), Some(seat)) = (&before, self.phase.seat()) {
                        let (joined, left) = view::joined_and_left(before, &seat.room);
                        effects.extend(joined.into_iter().map(|peer_id| PartyEffect::Event(PartyEvent::ParticipantJoined { peer_id })));
                        effects.extend(left.into_iter().map(|peer_id| PartyEffect::Event(PartyEvent::ParticipantLeft { peer_id })));
                    }
                    seq.push(Work::SendStatus);
                    if let Some(file) = resend {
                        seq.push(Work::SendText { conn, text: wire(&Outgoing::SetMedia { media: file.media, position_ms: Some(file.position_ms) }) });
                    }
                    seq.push(Work::RelaysConnected);
                    seq.extend(buffered.into_iter().map(|text| Work::Text { conn, text }));
                    seq.push(Work::Step);
                    if let Some((call, outcome)) = done {
                        seq.push(Work::Done { call, outcome });
                    }
                    then(&mut stack, seq);
                }
                Work::Receive { message } => match message {
                    Incoming::Room { room, media } => {
                        let (current_rev, epoch) = self.phase.seat().map_or((i64::MAX, 0), |seat| (view::rev(&seat.room), seat.epoch));
                        if view::rev(&room) <= current_rev {
                            continue;
                        }
                        let new_epoch = view::epoch(&room) != epoch;
                        let checked = if new_epoch { view::received_media(media.as_ref()) } else { None };
                        if new_epoch && checked.is_none() {
                            if let Some(conn) = member_conn(&self.phase) {
                                effects.push(PartyEffect::Send { conn, text: wire(&Outgoing::Leave) });
                            }
                            stack.push(Work::End { reason: EndReason::Lost, error: Some(PartyError::new("invalid_message", "the relay sent a new epoch without valid media")) });
                            continue;
                        }
                        let action = view::action(&room);
                        let mut phase = std::mem::take(&mut self.phase);
                        let mut before = Json::Null;
                        if let Some(seat) = phase.seat_mut() {
                            before = std::mem::replace(&mut seat.room, room);
                            if let Some(media) = checked {
                                seat.epoch = view::epoch(&seat.room);
                                seat.media = media.map_or(Media::None, Media::Theirs);
                                seat.own = std::mem::take(&mut seat.own).settle();
                            }
                        }
                        self.phase = phase;
                        let mut seq = Vec::new();
                        if new_epoch {
                            seq.push(Work::NewEpoch);
                        }
                        seq.push(Work::AcceptRest { before, action });
                        then(&mut stack, seq);
                    }
                    message @ Incoming::Error { .. } => {
                        if let Some(error) = error_of(&message) {
                            stack.push(Work::RelayError { error });
                        }
                    }
                    Incoming::Closed { reason } => {
                        let reason = match reason.as_str() {
                            "kicked" => EndReason::Kicked,
                            "expired" => EndReason::Expired,
                            _ => EndReason::Ended,
                        };
                        stack.push(Work::End { reason, error: None });
                    }
                    Incoming::Joined { .. } | Incoming::Pong { .. } => {}
                },
                Work::AcceptRest { before, action } => {
                    let mut seq = Vec::new();
                    if action.is_some() {
                        seq.push(Work::ReleaseHold);
                    }
                    seq.push(Work::RefreshPlayer);
                    let Some(seat) = self.phase.seat() else { continue };
                    let (joined, left) = view::joined_and_left(&before, &seat.room);
                    let mut events: Vec<PartyEvent> = joined.into_iter().map(|peer_id| PartyEvent::ParticipantJoined { peer_id }).collect();
                    events.extend(left.into_iter().map(|peer_id| PartyEvent::ParticipantLeft { peer_id }));
                    if let Some((kind, by_peer_id)) = action {
                        let by = by_peer_id.clone().filter(|peer| view::has_peer(&seat.room, peer));
                        let by_me = by_peer_id.as_deref() == Some(seat.session.peer_id.as_str());
                        events.push(PartyEvent::Action { kind, by_peer_id, by, by_me, rev: view::rev(&seat.room) });
                    }
                    seq.push(Work::Emit { events });
                    seq.push(Work::QueueStatus { force: false });
                    seq.push(Work::Step);
                    then(&mut stack, seq);
                }
                Work::Emit { events } => effects.extend(events.into_iter().map(PartyEffect::Event)),
                Work::RelayError { error } => {
                    let mut seq = Vec::new();
                    if matches!(self.hold, Hold::Held { re } if error.re.as_deref() == Some(re)) {
                        seq.push(Work::ReleaseHold);
                    }
                    if error.re.as_deref() == Some("setMedia") && self.phase.seat().is_some_and(|seat| seat.own.pending()) {
                        let mut phase = std::mem::take(&mut self.phase);
                        if let Some(seat) = phase.seat_mut() {
                            seat.own = std::mem::take(&mut seat.own).settle();
                        }
                        self.phase = phase;
                        seq.push(Work::Rebind);
                    }
                    seq.push(Work::Changed);
                    seq.push(Work::Emit { events: vec![PartyEvent::Error { error }] });
                    seq.push(Work::Step);
                    then(&mut stack, seq);
                }
                Work::Dropped { code } => {
                    if code == Some(4000) {
                        stack.push(Work::End { reason: EndReason::Replaced, error: None });
                        continue;
                    }
                    self.phase = match std::mem::take(&mut self.phase) {
                        Phase::InRoom(seat) => Phase::Reconnecting(seat, Resume { attempt: 0, step: ResumeStep::Drawing }),
                        other @ (Phase::Idle | Phase::Connecting(_) | Phase::Reconnecting(..) | Phase::Ended(_) | Phase::Closed(_)) => other,
                    };
                    self.deadlines.set(now, RESUME_FOR_MS, Timer::ResumeWindow);
                    then(&mut stack, vec![Work::StopStatus, Work::ScheduleResume]);
                }
                Work::ScheduleResume => {
                    let mut phase = std::mem::take(&mut self.phase);
                    if let Phase::Reconnecting(_, resume) = &mut phase {
                        resume.attempt += 1;
                        resume.step = ResumeStep::Drawing;
                        effects.push(PartyEffect::Draw);
                    }
                    self.phase = phase;
                }
                Work::ResumeFailed { conn, error } => {
                    let ours = match &self.phase {
                        Phase::Reconnecting(_, Resume { step: ResumeStep::Opening { conn: c } | ResumeStep::Asking { conn: c, .. } | ResumeStep::Sampling { conn: c, .. }, .. }) => *c == conn,
                        Phase::Idle | Phase::Connecting(_) | Phase::InRoom(_) | Phase::Reconnecting(..) | Phase::Ended(_) | Phase::Closed(_) => false,
                    };
                    let mut seq = Vec::new();
                    if self.links.get(&conn).is_some_and(|link| link.is_open() && !link.lingers()) {
                        let mut fx = Vec::new();
                        if let Some(link) = self.links.get_mut(&conn) {
                            link.step(LinkEvent::Close, &mut fx);
                        }
                        seq.push(Work::LinkFx { conn, fx });
                    }
                    if ours {
                        self.deadlines.cancel(Timer::FirstSample { conn });
                        if error.code == "resume_failed" || error.code == "client_blocked" {
                            seq.push(Work::End { reason: EndReason::Lost, error: Some(error) });
                        } else {
                            seq.push(Work::ScheduleResume);
                        }
                    }
                    then(&mut stack, seq);
                }
                Work::End { reason, error } => {
                    let live = matches!(self.phase, Phase::InRoom(_) | Phase::Reconnecting(..) | Phase::Connecting(_));
                    if live {
                        then(&mut stack, vec![Work::ReleaseAdapter, Work::EndRest { reason, error }]);
                    }
                }
                Work::EndRest { reason, error } => {
                    let mut seq = Vec::new();
                    let closing = match &self.phase {
                        Phase::Reconnecting(_, resume) => {
                            self.deadlines.cancel(Timer::Backoff);
                            self.deadlines.cancel(Timer::ResumeWindow);
                            match &resume.step {
                                ResumeStep::Opening { conn } => Some((*conn, LinkEvent::Close)),
                                ResumeStep::Asking { conn, .. } => Some((*conn, LinkEvent::Linger)),
                                ResumeStep::Sampling { conn, .. } => {
                                    effects.push(PartyEffect::Send { conn: *conn, text: wire(&Outgoing::Leave) });
                                    self.deadlines.cancel(Timer::FirstSample { conn: *conn });
                                    Some((*conn, LinkEvent::Close))
                                }
                                ResumeStep::Drawing | ResumeStep::Waiting => None,
                            }
                        }
                        Phase::InRoom(seat) => Some((seat.conn, LinkEvent::Close)),
                        Phase::Idle | Phase::Connecting(_) | Phase::Ended(_) | Phase::Closed(_) => None,
                    };
                    if let Some((conn, event)) = closing {
                        let mut fx = Vec::new();
                        if let Some(link) = self.links.get_mut(&conn) {
                            link.step(event, &mut fx);
                        }
                        seq.push(Work::LinkFx { conn, fx });
                    }
                    seq.push(Work::Reset);
                    seq.push(Work::EndFinal { reason, error });
                    then(&mut stack, seq);
                }
                Work::EndFinal { reason, error } => {
                    self.phase = Phase::Ended(reason);
                    effects.push(PartyEffect::Event(PartyEvent::Ended { reason, error }));
                }
                Work::Reset => {
                    self.deadlines.cancel(Timer::Tick);
                    self.deadlines.cancel(Timer::Play);
                    self.offset_ms = 0;
                    self.attached = match std::mem::replace(&mut self.attached, Attached::None) {
                        Attached::Away | Attached::None => Attached::None,
                        player @ Attached::Player { .. } => with_seek(player, None),
                    };
                    self.binding = Binding::Unbound;
                    self.player = PlayerState::Idle;
                    then(&mut stack, vec![Work::ReleaseHold, Work::StopStatus, Work::CancelMatching, Work::ClearMatching]);
                }
                Work::ClearMatching => {
                    self.matching.report = None;
                    self.matching.pick = Pick::None;
                    self.matching.candidates = Vec::new();
                    self.status = Status::Off;
                }
                Work::NewEpoch => {
                    let mut phase = std::mem::take(&mut self.phase);
                    let mut own_file = None;
                    let mut theirs = None;
                    if let Some(seat) = phase.seat_mut() {
                        let descriptor = seat.media.descriptor().cloned();
                        let own = seat.own.file().cloned();
                        match (descriptor, own) {
                            (Some(media), Some(file)) if view::same_value(&media, &file.media) => {
                                seat.media = Media::Own(media);
                                own_file = Some(file);
                            }
                            (Some(media), Some(_) | None) => {
                                seat.media = Media::Theirs(media.clone());
                                theirs = Some(media);
                            }
                            (None, Some(_) | None) => seat.media = Media::None,
                        }
                    }
                    self.phase = phase;
                    self.offset_ms = 0;
                    self.matching.pick = Pick::None;
                    self.matching.candidates = Vec::new();
                    let mut seq = vec![Work::CancelMatching];
                    match (own_file, theirs) {
                        (Some(file), _) => {
                            self.matching.pick = Pick::Chosen(own_candidate(&file));
                            self.matching.report = report(MatchState::Matched, Some(0));
                            seq.extend([Work::Unbind, Work::Rebind]);
                        }
                        (None, Some(_)) => {
                            self.matching.report = report(MatchState::Searching, None);
                            seq.extend([Work::UnbindAndPause, Work::StartMatching { walk: true }]);
                        }
                        (None, None) => {
                            self.matching.report = None;
                            seq.push(Work::UnbindAndPause);
                        }
                    }
                    then(&mut stack, seq);
                }
                Work::StartMatching { walk } => {
                    let media = self.phase.seat().and_then(|seat| seat.media.descriptor().cloned());
                    match (self.resolver, media) {
                        (true, Some(media)) => {
                            let search = self.ids.mint();
                            self.matching.search = Search::Resolving { search, walk };
                            self.deadlines.set(now, RESOLVE_TIMEOUT_MS, Timer::ResolveTimeout { search });
                            effects.push(PartyEffect::Resolve { search, media });
                        }
                        (false, _) | (true, None) => {
                            self.matching.candidates = Vec::new();
                            if walk {
                                self.matching.report = report(MatchState::Unavailable, None);
                            }
                        }
                    }
                }
                Work::RestartSearch => {
                    self.matching.pick = Pick::None;
                    self.matching.candidates = Vec::new();
                    self.matching.report = report(MatchState::Searching, None);
                    stack.push(Work::StartMatching { walk: true });
                }
                Work::CancelMatching => {
                    match std::mem::replace(&mut self.matching.search, Search::Idle) {
                        Search::Resolving { search, .. } => {
                            self.deadlines.cancel(Timer::ResolveTimeout { search });
                            effects.push(PartyEffect::CancelResolve { search });
                        }
                        Search::Walking { probe, .. } if probe != 0 => {
                            self.deadlines.cancel(Timer::ProbeDeadline { probe });
                            effects.push(PartyEffect::CancelProbe { probe });
                        }
                        Search::Walking { .. } | Search::Idle => {}
                    }
                    if let Choosing::Probing { probe, call, .. } = std::mem::replace(&mut self.matching.choosing, Choosing::Idle) {
                        effects.push(PartyEffect::CancelProbe { probe });
                        effects.push(PartyEffect::Done { call, outcome: Outcome::Answer(false) });
                    }
                }
                Work::WalkOn => {
                    let search = std::mem::replace(&mut self.matching.search, Search::Idle);
                    match search {
                        Search::Walking { walk, ranked, .. } => match walk.progress() {
                            Progress::Probing { index, deadline_ms } => {
                                let probe = self.ids.mint();
                                effects.push(PartyEffect::Probe { probe, candidate: ranked.get(index).map(|candidate| candidate.id.clone()).unwrap_or_default() });
                                self.deadlines.set(now, deadline_ms.max(0) + 1, Timer::ProbeDeadline { probe });
                                self.matching.search = Search::Walking { walk, ranked, probe, started_ms: now };
                            }
                            Progress::Done(outcome) => stack.push(Work::WalkDone { outcome, ranked }),
                        },
                        other @ (Search::Idle | Search::Resolving { .. }) => self.matching.search = other,
                    }
                }
                Work::WalkDone { outcome, ranked } => {
                    match outcome {
                        WalkOutcome::Matched { index, delta_ms } => {
                            self.matching.pick = ranked.get(index).cloned().map_or(Pick::None, Pick::Chosen);
                            self.matching.report = report(MatchState::Matched, Some(delta_ms));
                        }
                        WalkOutcome::Mismatch { index, delta_ms } => {
                            self.matching.pick = ranked.get(index).cloned().map_or(Pick::None, |candidate| Pick::Offered { candidate, delta_ms });
                            self.matching.report = report(MatchState::Searching, Some(delta_ms));
                        }
                        WalkOutcome::Unavailable => self.matching.report = report(MatchState::Unavailable, None),
                    }
                    then(&mut stack, vec![Work::Rebind, Work::Changed, Work::Step]);
                }
                Work::ChooseWith { call, candidate, duration_ms } => {
                    let media_duration = self.phase.seat().and_then(|seat| seat.media.descriptor()).map(view::duration_ms);
                    match media_duration {
                        Some(media_duration) => {
                            let delta_ms = duration_ms - media_duration;
                            let state = if delta_ms.abs() <= matching::MATCH_TOLERANCE_MS { MatchState::Matched } else { MatchState::Mismatched };
                            then(&mut stack, vec![Work::CancelMatching, Work::Chose { candidate, report: report(state, Some(delta_ms)) }, Work::Rebind, Work::Changed, Work::Step, Work::Done { call, outcome: Outcome::Answer(true) }]);
                        }
                        None => effects.push(PartyEffect::Done { call, outcome: Outcome::Answer(false) }),
                    }
                }
                Work::Chose { candidate, report } => {
                    self.matching.pick = Pick::Chosen(candidate);
                    self.matching.report = report;
                }
                Work::Rebind => {
                    let wanted = bindable(&self.attached, &self.phase, &self.matching);
                    let bound = matches!(self.binding, Binding::Bound { .. });
                    if wanted && !bound {
                        stack.push(Work::Bind);
                    } else if !wanted && bound {
                        stack.push(Work::Unbind);
                    }
                }
                Work::Bind => {
                    if let Attached::Player { player, can_set_rate, .. } = self.attached {
                        let rate = self.phase.seat().map_or(1.0, |seat| view::rate(&seat.room));
                        self.binding = Binding::Bound { applied_rate: rate, nudging: false, blocked: false };
                        if can_set_rate {
                            stack.push(Work::Enqueue { player, job: Job { action: PlayerAction::SetRate { rate }, kind: JobKind::Command } });
                        }
                    }
                }
                Work::Unbind => {
                    self.binding = Binding::Unbound;
                    self.deadlines.cancel(Timer::Play);
                }
                Work::UnbindAndPause => {
                    let mut seq = vec![Work::Unbind];
                    if let Some(player) = attached_player(&self.attached) {
                        seq.push(Work::Enqueue { player, job: Job { action: PlayerAction::Pause, kind: JobKind::Command } });
                    }
                    then(&mut stack, seq);
                }
                Work::ReleaseAdapter => {
                    if let Attached::Player { player, can_set_rate, .. } = self.attached {
                        let nudging = matches!(self.binding, Binding::Bound { nudging: true, .. });
                        self.binding = Binding::Unbound;
                        self.deadlines.cancel(Timer::Play);
                        self.attached = Attached::None;
                        if let Some(lane) = self.lanes.get_mut(&player) {
                            lane.step(LaneEvent::Release, &mut Vec::new());
                        }
                        let rate = self.phase.seat().map(|seat| view::rate(&seat.room));
                        if let (true, Some(rate), true) = (nudging, rate, can_set_rate) {
                            stack.push(Work::Enqueue { player, job: Job { action: PlayerAction::SetRate { rate }, kind: JobKind::Restore } });
                        }
                    }
                }
                Work::Attach { player, reference, can_set_rate, same } => {
                    let (last_seek_ms, step_wanted) = match &self.attached {
                        Attached::Player { last_seek_ms, step_wanted, .. } if same => (*last_seek_ms, *step_wanted),
                        Attached::Player { .. } | Attached::None | Attached::Away => (None, false),
                    };
                    self.attached = Attached::Player { player, reference, can_set_rate, last_seek_ms, step_wanted };
                }
                Work::GoAway => self.attached = Attached::Away,
                Work::SettleReleased => {
                    if !self.released_waiters.is_empty() && !released_busy(&self.lanes, &self.attached) {
                        for call in std::mem::take(&mut self.released_waiters) {
                            effects.push(PartyEffect::Done { call, outcome: Outcome::Done });
                        }
                    }
                }
                Work::DoneWhenSettled { call } => {
                    if released_busy(&self.lanes, &self.attached) {
                        self.released_waiters.push(call);
                    } else {
                        effects.push(PartyEffect::Done { call, outcome: Outcome::Done });
                    }
                }
                Work::Enqueue { player, job } => {
                    let cmd = self.ids.mint();
                    let mut fx = Vec::new();
                    self.lanes.entry(player).or_default().step(LaneEvent::Push { job, cmd }, &mut fx);
                    stack.push(Work::LaneFx { player, fx, threw: false });
                }
                Work::LaneFx { player, fx, threw } => {
                    let mut seq = Vec::new();
                    for effect in fx {
                        match effect {
                            LaneEffect::Run { cmd, action } => effects.push(PartyEffect::Command { cmd, player, action }),
                            LaneEffect::Finished { action } => {
                                if action == PlayerAction::Play && threw {
                                    seq.push(Work::PlayRefused { player });
                                }
                            }
                            LaneEffect::Drained => seq.push(Work::DrainEnd { player }),
                        }
                    }
                    then(&mut stack, seq);
                }
                Work::PlayRefused { player } => {
                    let playing = self.phase.seat().is_some_and(|seat| view::timeline(&seat.room).phase == RoomPhase::Playing);
                    let paused = self.reads.as_ref().is_some_and(|reads| reads.paused);
                    if let (Some(current), Binding::Bound { applied_rate, nudging, blocked: false }) = (attached_player(&self.attached), self.binding) {
                        if current == player && paused && playing {
                            self.binding = Binding::Bound { applied_rate, nudging, blocked: true };
                            then(&mut stack, vec![Work::Changed, Work::Emit { events: vec![PartyEvent::PlayBlocked] }]);
                        }
                    }
                }
                Work::DrainEnd { player } => {
                    self.lanes.remove(&player);
                    if attached_player(&self.attached) == Some(player) {
                        let mut seq = Vec::new();
                        if matches!(self.phase, Phase::InRoom(_) | Phase::Reconnecting(..)) {
                            seq.push(Work::Changed);
                        }
                        seq.push(Work::StepIfWanted);
                        then(&mut stack, seq);
                    } else {
                        stack.push(Work::SettleReleased);
                    }
                }
                Work::StepIfWanted => {
                    if matches!(self.attached, Attached::Player { step_wanted: true, .. }) {
                        self.attached = wanting(std::mem::replace(&mut self.attached, Attached::None), false);
                        stack.push(Work::Step);
                    }
                }
                Work::Step => {
                    self.deadlines.cancel(Timer::Play);
                    let relay_now_ms = clock_of(&self.links, &self.phase).map(|best| now + best.offset_ms);
                    let (Attached::Player { player, can_set_rate, last_seek_ms, .. }, Binding::Bound { applied_rate, nudging, blocked }) = (&self.attached, self.binding) else { continue };
                    let (player, can_set_rate, last_seek_ms) = (*player, *can_set_rate, *last_seek_ms);
                    let Some((room, duration_ms)) = self.phase.seat().and_then(|seat| seat.media.descriptor().map(|media| (view::timeline(&seat.room), view::duration_ms(media)))) else { continue };
                    let Some(relay_now_ms) = relay_now_ms else { continue };
                    if self.lanes.get(&player).is_some_and(Lane::busy) {
                        self.attached = wanting(std::mem::replace(&mut self.attached, Attached::None), true);
                        continue;
                    }
                    if self.hold != Hold::None {
                        continue;
                    }
                    let Some(reads) = self.reads.as_ref() else { continue };
                    let step = reconcile::step(&StepInput {
                        room,
                        duration_ms,
                        relay_now_ms,
                        offset_ms: self.offset_ms,
                        player: StepPlayer {
                            position_ms: reads.position_ms,
                            paused: reads.paused,
                            buffering: reads.buffering,
                            duration_ms: reads.duration_ms,
                            can_set_rate,
                            applied_rate,
                            seek_accuracy_ms: reads.seek_accuracy_ms,
                        },
                        nudging,
                        since_seek_ms: last_seek_ms.map(|at| now - at),
                    });
                    let mut rate = applied_rate;
                    let mut seq = Vec::new();
                    for action in step.actions() {
                        let (action, kind) = match action {
                            Action::Pause => (PlayerAction::Pause, JobKind::Command),
                            Action::SetRate { rate: set } => {
                                rate = set;
                                (PlayerAction::SetRate { rate: set }, JobKind::Command)
                            }
                            Action::Seek { position_ms } => {
                                self.attached = with_seek(std::mem::replace(&mut self.attached, Attached::None), Some(now));
                                (PlayerAction::Seek { position_ms }, JobKind::Seek)
                            }
                            Action::Play => (PlayerAction::Play, JobKind::Command),
                        };
                        seq.push(Work::Enqueue { player, job: Job { action, kind } });
                    }
                    self.binding = Binding::Bound { applied_rate: rate, nudging: step.nudging(), blocked };
                    if let Some(play_in_ms) = step.play_in_ms() {
                        self.deadlines.set(now, play_in_ms, Timer::Play);
                    }
                    seq.push(Work::Changed);
                    then(&mut stack, seq);
                }
                Work::RefreshPlayer => {
                    let playing = self.phase.seat().is_some_and(|seat| view::timeline(&seat.room).phase == RoomPhase::Playing);
                    let paused = self.reads.as_ref().is_some_and(|reads| reads.paused);
                    if let Binding::Bound { applied_rate, nudging, blocked: true } = self.binding {
                        if !paused || !playing {
                            self.binding = Binding::Bound { applied_rate, nudging, blocked: false };
                        }
                    }
                    let relay_now_ms = clock_of(&self.links, &self.phase).map(|best| now + best.offset_ms);
                    self.player = compute_player(&self.phase, &self.matching, &self.attached, self.binding, self.reads.as_ref(), &self.lanes, relay_now_ms, self.offset_ms);
                }
                Work::Changed => then(&mut stack, vec![Work::RefreshPlayer, Work::QueueStatus { force: false }]),
                Work::QueueStatus { force } => {
                    if member_conn(&self.phase).is_none() {
                        continue;
                    }
                    let Status::Live { sent, force: forced } = std::mem::replace(&mut self.status, Status::Off) else { continue };
                    let force = forced || force;
                    let values = StatusValues { epoch: self.phase.seat().map_or(0, |seat| seat.epoch), player: self.player, report: self.matching.report, offset_ms: self.offset_ms };
                    let unchanged = sent.as_ref().is_some_and(|(last, _)| *last == values);
                    let new_epoch = sent.as_ref().is_some_and(|(last, _)| last.epoch != values.epoch);
                    let wait = sent.as_ref().map_or(i64::MIN, |(_, at)| at + STATUS_SPACING_MS - now);
                    self.status = Status::Live { sent, force };
                    if !force && unchanged {
                        continue;
                    }
                    if wait > 0 && !new_epoch {
                        if !self.deadlines.is_set(Timer::StatusSpacing) {
                            self.deadlines.set(now, wait, Timer::StatusSpacing);
                        }
                        continue;
                    }
                    stack.push(Work::SendStatus);
                }
                Work::SendStatus => {
                    let Some(conn) = member_conn(&self.phase) else { continue };
                    self.deadlines.cancel(Timer::StatusSpacing);
                    let values = StatusValues { epoch: self.phase.seat().map_or(0, |seat| seat.epoch), player: self.player, report: self.matching.report, offset_ms: self.offset_ms };
                    let rtt_ms = self.links.get(&conn).and_then(Link::best).map(|best| best.rtt_ms);
                    let message = Outgoing::Status { epoch: values.epoch, player: values.player, report: values.report, offset_ms: values.offset_ms, rtt_ms };
                    effects.push(PartyEffect::Send { conn, text: wire(&message) });
                    self.status = Status::Live { sent: Some((values, now)), force: false };
                    self.deadlines.set(now, HEARTBEAT_MS, Timer::Heartbeat);
                }
                Work::StopStatus => {
                    self.deadlines.cancel(Timer::StatusSpacing);
                    self.deadlines.cancel(Timer::Heartbeat);
                    if let Status::Live { sent, .. } = std::mem::replace(&mut self.status, Status::Off) {
                        self.status = Status::Live { sent, force: false };
                    }
                }
                Work::HoldFor { re } => {
                    self.deadlines.cancel(Timer::Hold);
                    self.hold = Hold::Held { re };
                    self.deadlines.set(now, HOLD_MS, Timer::Hold);
                }
                Work::ReleaseHold => {
                    if self.hold != Hold::None {
                        self.deadlines.cancel(Timer::Hold);
                        self.hold = Hold::None;
                    }
                }
                Work::Leave => {
                    let phase = std::mem::take(&mut self.phase);
                    match phase {
                        Phase::Connecting(attempt) => {
                            let mut seq = Vec::new();
                            if let AttemptStep::Asking { conn, .. } = attempt.step {
                                if let Some(link) = self.links.get_mut(&conn) {
                                    link.step(LinkEvent::Linger, &mut Vec::new());
                                }
                            }
                            self.deadlines.cancel(Timer::Race);
                            for conn in attempt.links {
                                if self.links.get(&conn).is_some_and(|link| !link.lingers()) {
                                    let mut fx = Vec::new();
                                    if let Some(link) = self.links.get_mut(&conn) {
                                        link.step(LinkEvent::Close, &mut fx);
                                    }
                                    seq.push(Work::LinkFx { conn, fx });
                                }
                            }
                            self.phase = Phase::Idle;
                            seq.push(Work::Done { call: attempt.call, outcome: failed("cancelled", "cancelled by leave() or close()") });
                            then(&mut stack, seq);
                        }
                        Phase::InRoom(seat) => {
                            effects.push(PartyEffect::Send { conn: seat.conn, text: wire(&Outgoing::Leave) });
                            self.phase = Phase::InRoom(seat);
                            stack.push(Work::End { reason: EndReason::Left, error: None });
                        }
                        reconnecting @ Phase::Reconnecting(..) => {
                            self.phase = reconnecting;
                            stack.push(Work::End { reason: EndReason::Left, error: None });
                        }
                        other @ (Phase::Idle | Phase::Ended(_) | Phase::Closed(_)) => self.phase = other,
                    }
                }
                Work::MarkClosed => {
                    self.phase = match std::mem::take(&mut self.phase) {
                        Phase::Ended(reason) | Phase::Closed(Some(reason)) => Phase::Closed(Some(reason)),
                        Phase::Idle | Phase::Connecting(_) | Phase::InRoom(_) | Phase::Reconnecting(..) | Phase::Closed(None) => Phase::Closed(None),
                    };
                }
                Work::SendText { conn, text } => effects.push(PartyEffect::Send { conn, text }),
                Work::Done { call, outcome } => effects.push(PartyEffect::Done { call, outcome }),
                Work::RelaysConnected => {
                    let due = self.relays.last_refresh_ms.is_none_or(|last| now - last >= REFRESH_AFTER_MS);
                    let running = self.fetches.values().any(|fetch| *fetch == Fetch::Background);
                    if self.relays.public && due && !running {
                        let get = self.ids.mint();
                        self.fetches.insert(get, Fetch::Background);
                        self.deadlines.set(now, FETCH_TIMEOUT_MS, Timer::Fetch { get });
                        effects.push(PartyEffect::Get { get, url: relays::PUBLIC_LIST_URL.to_owned() });
                    }
                }
            }
        }
        let kept = self.phase.seat().map(|seat| seat.conn);
        self.links.retain(|conn, link| link.is_open() || link.state() == LinkState::Opening || Some(*conn) == kept);
        self.view = build_view(&self.phase, &self.matching, &self.links, self.offset_ms, self.player);
    }
}
