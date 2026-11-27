#![deny(clippy::wildcard_enum_match_arm)]

pub(crate) mod deadlines;
mod lane;
pub(crate) mod link;
mod machine;
mod view;

pub use lane::PlayerAction;
pub use machine::Party;

use crate::json::Json;
use crate::matching::{Candidate, Walk};
use crate::message::{Control, MatchReport, PlayerState};
use crate::relays::Relay;
use crate::timeline::Timeline;
use link::Hello;

pub const RESOLVE_TIMEOUT_MS: i64 = 30_000;
pub const HEARTBEAT_MS: i64 = 15_000;
pub const STATUS_SPACING_MS: i64 = 250;
pub const TICK_MS: i64 = 500;
pub const HELD_LOADING_MS: i64 = 500;
pub const HOLD_MS: i64 = 2000;
pub const RESUME_FOR_MS: i64 = 300_000;
pub const BACKOFF_FIRST_MS: i64 = 500;
pub const BACKOFF_CEILING_MS: i64 = 8000;
pub const FETCH_TIMEOUT_MS: i64 = 5000;
pub const REFRESH_AFTER_MS: i64 = 24 * 60 * 60 * 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Connection {
    #[default]
    Idle,
    Connecting,
    Connected,
    Reconnecting,
    Ended,
}

impl Connection {
    pub fn name(self) -> &'static str {
        match self {
            Connection::Idle => "idle",
            Connection::Connecting => "connecting",
            Connection::Connected => "connected",
            Connection::Reconnecting => "reconnecting",
            Connection::Ended => "ended",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndReason {
    Left,
    Ended,
    Kicked,
    Expired,
    Replaced,
    Lost,
}

impl EndReason {
    pub fn name(self) -> &'static str {
        match self {
            EndReason::Left => "left",
            EndReason::Ended => "ended",
            EndReason::Kicked => "kicked",
            EndReason::Expired => "expired",
            EndReason::Replaced => "replaced",
            EndReason::Lost => "lost",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartyError {
    pub code: String,
    pub re: Option<String>,
    pub message: String,
}

impl PartyError {
    pub fn new(code: &str, message: &str) -> PartyError {
        PartyError { code: code.to_owned(), re: None, message: message.to_owned() }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Reads {
    pub position_ms: i64,
    pub paused: bool,
    pub buffering: bool,
    pub duration_ms: Option<i64>,
    pub first_frame_shown: bool,
    pub seek_accuracy_ms: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Moment {
    pub now_ms: i64,
    pub player: Option<Reads>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Request {
    Create,
    Join { code: String, create: bool },
    Leave,
    End,
    Close,
    SetMedia { media: Json, reference: String, position_ms: i64 },
    ClearMedia,
    Play,
    Pause,
    Seek { position_ms: i64 },
    SetRate { rate: f64 },
    SetOffset { offset_ms: i64 },
    SetControl { mode: Control },
    Lock { locked: bool },
    Kick { peer_id: String },
    TransferHost { peer_id: String },
    Choose { candidate: Candidate },
    AcceptMismatch,
    DeclineMismatch,
    ResolveAgain,
    AttachPlayer { player: u64, reference: String, can_set_rate: bool },
    DetachPlayer { player: Option<u64> },
    PlayerChanged,
    Refresh,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NetEvent {
    Opened,
    OpenFailed,
    Text(String),
    Closed(Option<u16>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum PartyInput {
    Call { at: Moment, call: u64, request: Request },
    Net { at: Moment, conn: u64, event: NetEvent },
    Got { at: Moment, get: u64, body: Option<String> },
    CommandDone { at: Moment, cmd: u64, threw: bool },
    Resolved { at: Moment, search: u64, answer: Option<Vec<Candidate>> },
    Probed { at: Moment, probe: u64, duration_ms: Option<i64> },
    Drawn { at: Moment, u: f64 },
    Wake { at: Moment },
}

impl PartyInput {
    fn moment(&self) -> &Moment {
        match self {
            PartyInput::Call { at, .. }
            | PartyInput::Net { at, .. }
            | PartyInput::Got { at, .. }
            | PartyInput::CommandDone { at, .. }
            | PartyInput::Resolved { at, .. }
            | PartyInput::Probed { at, .. }
            | PartyInput::Drawn { at, .. }
            | PartyInput::Wake { at } => at,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    Done,
    Code(String),
    Answer(bool),
    Failed(PartyError),
}

#[derive(Debug, Clone, PartialEq)]
pub enum PartyEvent {
    Action { kind: String, by_peer_id: Option<String>, by: Option<Json>, by_me: bool, room: Json },
    ParticipantJoined { participant: Json },
    ParticipantLeft { participant: Json },
    Error { error: PartyError },
    ResolveFailed { timeout: bool },
    PlayBlocked,
    Ended { reason: EndReason, error: Option<PartyError> },
}

#[derive(Debug, Clone, PartialEq)]
pub enum PartyEffect {
    Open { conn: u64, url: String },
    Send { conn: u64, text: String },
    Close { conn: u64, code: u16 },
    Get { get: u64, url: String },
    AbortGet { get: u64 },
    Command { cmd: u64, player: u64, action: PlayerAction },
    Resolve { search: u64, media: Json },
    CancelResolve { search: u64 },
    Probe { probe: u64, candidate: String },
    CancelProbe { probe: u64 },
    Done { call: u64, outcome: Outcome },
    Event(PartyEvent),
    Draw,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PartyState {
    pub connection: Connection,
    pub peer_id: Option<String>,
    pub room: Option<Json>,
    pub media: Option<Json>,
    pub report: Option<MatchReport>,
    pub choice: Option<Candidate>,
    pub mismatch: Option<(Candidate, i64)>,
    pub candidates: Vec<(Candidate, Option<i64>)>,
    pub offset_ms: i64,
    pub player: PlayerState,
    pub end_reason: Option<EndReason>,
    pub is_host: bool,
    pub can_control: bool,
    timeline: Option<(Timeline, i64)>,
    relay_offset_ms: Option<i64>,
}

impl PartyState {
    fn idle() -> PartyState {
        PartyState {
            connection: Connection::Idle,
            peer_id: None,
            room: None,
            media: None,
            report: None,
            choice: None,
            mismatch: None,
            candidates: Vec::new(),
            offset_ms: 0,
            player: PlayerState::Idle,
            end_reason: None,
            is_host: false,
            can_control: false,
            timeline: None,
            relay_offset_ms: None,
        }
    }

    pub fn room_position_ms(&self, now_ms: i64) -> Option<i64> {
        let (timeline, duration_ms) = self.timeline?;
        let offset = self.relay_offset_ms?;
        Some(timeline.at(duration_ms, now_ms + offset).position_ms)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Session {
    url: String,
    code: String,
    peer_id: String,
    token: String,
}

#[derive(Debug, Clone, PartialEq)]
struct OwnFile {
    media: Json,
    reference: String,
    position_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Default)]
enum Own {
    #[default]
    None,
    Pending(OwnFile),
    Settled(OwnFile),
}

impl Own {
    fn file(&self) -> Option<&OwnFile> {
        match self {
            Own::None => None,
            Own::Pending(file) | Own::Settled(file) => Some(file),
        }
    }

    fn pending(&self) -> bool {
        matches!(self, Own::Pending(_))
    }

    fn settle(self) -> Own {
        match self {
            Own::Pending(file) | Own::Settled(file) => Own::Settled(file),
            Own::None => Own::None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
enum Media {
    #[default]
    None,
    Theirs(Json),
    Own(Json),
}

impl Media {
    fn descriptor(&self) -> Option<&Json> {
        match self {
            Media::None => None,
            Media::Theirs(media) | Media::Own(media) => Some(media),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
struct Seat {
    conn: u64,
    session: Session,
    room: Json,
    media: Media,
    epoch: i64,
    own: Own,
}

#[derive(Debug, Clone, PartialEq)]
enum Goal {
    Create { untried: Vec<String> },
    Join { code: String, create: bool, url: String, refreshed: bool },
}

#[derive(Debug, Clone, PartialEq)]
enum Refreshed {
    Route,
    Moved { error: PartyError },
}

#[derive(Debug, Clone, PartialEq)]
enum AttemptStep {
    Opening { conn: u64 },
    Racing { conns: Vec<u64>, failed: usize },
    Asking { conn: u64, hello: Hello },
    Refreshing { get: u64, then: Refreshed },
}

#[derive(Debug, Clone, PartialEq)]
struct Attempt {
    call: u64,
    goal: Goal,
    step: AttemptStep,
    links: Vec<u64>,
}

#[derive(Debug, Clone, PartialEq)]
struct Answer {
    code: String,
    peer_id: String,
    token: String,
    room: Json,
    media: Option<Json>,
    buffered: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
enum ResumeStep {
    Drawing,
    Waiting,
    Opening { conn: u64 },
    Asking { conn: u64, sent_at_ms: i64 },
    Sampling { conn: u64, answer: Answer },
}

#[derive(Debug, Clone, PartialEq)]
struct Resume {
    attempt: u32,
    step: ResumeStep,
}

#[derive(Debug, Clone, PartialEq, Default)]
enum Phase {
    #[default]
    Idle,
    Connecting(Attempt),
    InRoom(Seat),
    Reconnecting(Seat, Resume),
    Ended(EndReason),
    Closed(Option<EndReason>),
}

impl Phase {
    fn seat(&self) -> Option<&Seat> {
        match self {
            Phase::InRoom(seat) | Phase::Reconnecting(seat, _) => Some(seat),
            Phase::Idle | Phase::Connecting(_) | Phase::Ended(_) | Phase::Closed(_) => None,
        }
    }

    fn seat_mut(&mut self) -> Option<&mut Seat> {
        match self {
            Phase::InRoom(seat) | Phase::Reconnecting(seat, _) => Some(seat),
            Phase::Idle | Phase::Connecting(_) | Phase::Ended(_) | Phase::Closed(_) => None,
        }
    }

    fn connection(&self) -> Connection {
        match self {
            Phase::Idle | Phase::Closed(None) => Connection::Idle,
            Phase::Connecting(_) => Connection::Connecting,
            Phase::InRoom(_) => Connection::Connected,
            Phase::Reconnecting(..) => Connection::Reconnecting,
            Phase::Ended(_) | Phase::Closed(Some(_)) => Connection::Ended,
        }
    }

    fn end_reason(&self) -> Option<EndReason> {
        match self {
            Phase::Ended(reason) | Phase::Closed(Some(reason)) => Some(*reason),
            Phase::Idle | Phase::Connecting(_) | Phase::InRoom(_) | Phase::Reconnecting(..) | Phase::Closed(None) => None,
        }
    }

    fn closed(&self) -> bool {
        matches!(self, Phase::Closed(_))
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Pick {
    None,
    Chosen(Candidate),
    Offered { candidate: Candidate, delta_ms: i64 },
}

#[derive(Debug, Clone, PartialEq)]
enum Search {
    Idle,
    Resolving { search: u64, walk: bool },
    Walking { walk: Walk, ranked: Vec<Candidate>, probe: u64, started_ms: i64 },
}

#[derive(Debug, Clone, PartialEq)]
enum Choosing {
    Idle,
    Probing { probe: u64, call: u64, candidate: Candidate },
}

#[derive(Debug, Clone, PartialEq)]
struct Matching {
    search: Search,
    choosing: Choosing,
    report: Option<MatchReport>,
    pick: Pick,
    candidates: Vec<(Candidate, Option<i64>)>,
}

#[derive(Debug, Clone, PartialEq)]
enum Attached {
    None,
    Away,
    Player { player: u64, reference: String, can_set_rate: bool, last_seek_ms: Option<i64>, step_wanted: bool },
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Binding {
    Unbound,
    Bound { applied_rate: f64, nudging: bool, blocked: bool },
}

#[derive(Debug, Clone, PartialEq)]
struct StatusValues {
    epoch: i64,
    player: PlayerState,
    report: Option<MatchReport>,
    offset_ms: i64,
}

#[derive(Debug, Clone, PartialEq)]
enum Status {
    Off,
    Live { sent: Option<(StatusValues, i64)>, force: bool },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Hold {
    None,
    Held { re: &'static str },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fetch {
    Call(u64),
    Join,
    Background,
}

#[derive(Debug, Clone, PartialEq)]
struct Relays {
    list: Vec<Relay>,
    public: bool,
    last_refresh_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RelaySource {
    Public,
    At(Relay),
}

pub fn relay_at(base_url: &str) -> Result<Relay, PartyError> {
    let base = base_url.trim_end_matches('/');
    let (secure, rest) = match (base.strip_prefix("https://"), base.strip_prefix("http://")) {
        (Some(rest), _) => (true, rest),
        (None, Some(rest)) => (false, rest),
        (None, None) => return Err(PartyError::new("invalid_message", "not an http(s) base URL")),
    };
    if rest.is_empty() || rest.starts_with(['/', '?', '#']) {
        return Err(PartyError::new("invalid_message", "not an http(s) base URL"));
    }
    let url = format!("{}{rest}/v1/ws", if secure { "wss://" } else { "ws://" });
    if !crate::relays::valid_url(&url, !secure) {
        return Err(PartyError::new("invalid_message", "not a relay base URL"));
    }
    Ok(Relay { id: "custom".to_owned(), name: base.to_owned(), url, prefixes: crate::code::CODE_ALPHABET.to_owned() })
}
