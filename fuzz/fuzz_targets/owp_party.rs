#![no_main]
use libfuzzer_sys::fuzz_target;
use plx_owp::matching::Candidate;
use plx_owp::message::Control;
use plx_owp::party::{relay_at, Moment, NetEvent, Party, PartyEffect, PartyInput, Reads, RelaySource, Request};
use plxnative_fuzz as _;
use std::collections::{BTreeMap, BTreeSet};

struct Bytes<'a> {
    data: &'a [u8],
    at: usize,
}

impl Bytes<'_> {
    fn byte(&mut self) -> Option<u8> {
        let byte = self.data.get(self.at).copied();
        self.at += 1;
        byte
    }

    fn pick(&mut self, n: usize) -> usize {
        self.byte().map_or(0, |byte| usize::from(byte) % n.max(1))
    }

    fn int(&mut self, scale: i64) -> i64 {
        let high = i64::from(self.byte().unwrap_or(0));
        let low = i64::from(self.byte().unwrap_or(0));
        (high << 8 | low) * scale
    }

    fn flag(&mut self) -> bool {
        self.byte().is_some_and(|byte| byte & 1 == 1)
    }
}

#[derive(Default)]
struct Conn {
    opened: bool,
    gone: bool,
}

struct Model {
    party: Party,
    now: i64,
    next_call: u64,
    calls: BTreeMap<u64, u32>,
    conns: BTreeMap<u64, Conn>,
    gets: BTreeSet<u64>,
    running: BTreeMap<u64, u64>,
    resolves: BTreeSet<u64>,
    probes: BTreeSet<u64>,
    draws: usize,
    ended_ids: BTreeSet<u64>,
    attached: Option<u64>,
    reads: Reads,
}

const PHASES: [&str; 4] = ["idle", "paused", "waiting", "playing"];
const CODES: [&str; 7] = ["relay_full", "room_not_found", "resume_failed", "client_blocked", "rate_limited", "forbidden", "invalid_message"];
const RES: [&str; 8] = ["create", "join", "resume", "setMedia", "pause", "seek", "rate", "kick"];
const REASONS: [&str; 4] = ["kicked", "expired", "ended", "maintenance"];
const LIST: &str = r#"{"relays":[{"id":"a","name":"A","url":"wss://a.relay.test/v1/ws","prefixes":"23456789ABCDEFGHJK"},{"id":"b","name":"B","url":"wss://b.relay.test/v1/ws","prefixes":"MNPQRSTUVWXYZ"}]}"#;
const HEAT: &str = r#"{"kind":"movie","title":"Heat","year":1995,"ids":{"imdb":"tt0113277"},"durationMs":10200000}"#;

fn room(bytes: &mut Bytes) -> String {
    let rev = bytes.pick(8) + 1;
    let epoch = bytes.pick(3);
    let phase = PHASES[bytes.pick(PHASES.len())];
    let position = bytes.int(7);
    let ann_hosts = bytes.flag();
    let control = if bytes.flag() { "everyone" } else { "host" };
    format!(
        r#""rev":{rev},"code":"K7MQ2X","epoch":{epoch},"phase":"{phase}","positionMs":{position},"atMs":1000000,"rate":1,"control":"{control}","locked":false,"waitingOn":[],"participants":[{{"peerId":"ann","name":"Ann","client":"Example/1.0","host":{ann_hosts},"connected":true,"player":"idle","match":null,"offsetMs":0}},{{"peerId":"ben","name":"Ben","client":"Example/1.0","host":{},"connected":true,"player":"idle","match":null,"offsetMs":0}}]"#,
        !ann_hosts
    )
}

fn message(bytes: &mut Bytes) -> String {
    match bytes.pick(7) {
        0 => format!(r#"{{"type":"joined","code":"K7MQ2X","you":{{"peerId":"ann","token":"ann-ttttttttttttttttttttttttttttttttttttttt"}},"room":{{{}}},"media":null,"serverTime":1000000}}"#, room(bytes)),
        1 => {
            let media = if bytes.flag() { HEAT } else { "null" };
            let action = if bytes.flag() { r#","action":{"kind":"play","by":"ben"}"# } else { "" };
            format!(r#"{{"type":"room",{},"media":{media}{action}}}"#, room(bytes))
        }
        2 => {
            let re = if bytes.flag() { format!(r#""{}""#, RES[bytes.pick(RES.len())]) } else { "null".to_owned() };
            format!(r#"{{"type":"error","code":"{}","message":"refused","re":{re}}}"#, CODES[bytes.pick(CODES.len())])
        }
        3 => format!(r#"{{"type":"closed","reason":"{}"}}"#, REASONS[bytes.pick(REASONS.len())]),
        4 => format!(r#"{{"type":"pong","id":{},"serverTime":{}}}"#, bytes.pick(12), 1_000_000 + bytes.int(1)),
        5 => r#"{"type":"joined"}"#.to_owned(),
        _ => String::from_utf8_lossy(&bytes.data[bytes.at.min(bytes.data.len())..(bytes.at + 16).min(bytes.data.len())]).into_owned(),
    }
}

impl Model {
    fn at(&self) -> Moment {
        Moment { now_ms: self.now, player: self.attached.map(|_| self.reads.clone()) }
    }

    fn feed(&mut self, input: PartyInput) {
        let mut effects = Vec::new();
        self.party.step(input, &mut effects);
        for effect in effects {
            self.check(effect);
        }
    }

    fn check(&mut self, effect: PartyEffect) {
        match effect {
            PartyEffect::Open { conn, .. } => {
                assert!(!self.conns.contains_key(&conn) && !self.ended_ids.contains(&conn), "a reused connection ID {conn}");
                self.conns.insert(conn, Conn::default());
            }
            PartyEffect::Send { conn, .. } => assert!(self.conns.get(&conn).is_some_and(|conn| conn.opened && !conn.gone), "a send on a dead connection {conn}"),
            PartyEffect::Close { conn, .. } => {
                let state = self.conns.get_mut(&conn).filter(|conn| !conn.gone).expect("a close of a dead connection");
                state.gone = true;
            }
            PartyEffect::Get { get, .. } => assert!(self.gets.insert(get), "a reused GET ID"),
            PartyEffect::AbortGet { get } => {
                assert!(self.gets.remove(&get), "an abort of a dead GET {get}");
                self.ended_ids.insert(get);
            }
            PartyEffect::Command { cmd, player, .. } => {
                assert!(!self.running.contains_key(&player), "two commands in flight on lane {player}");
                self.running.insert(player, cmd);
            }
            PartyEffect::Resolve { search, .. } => assert!(self.resolves.insert(search)),
            PartyEffect::CancelResolve { search } => {
                assert!(self.resolves.remove(&search), "a cancel of a dead search {search}");
                self.ended_ids.insert(search);
            }
            PartyEffect::Probe { probe, .. } => assert!(self.probes.insert(probe)),
            PartyEffect::CancelProbe { probe } => {
                assert!(self.probes.remove(&probe), "a cancel of a dead probe {probe}");
                self.ended_ids.insert(probe);
            }
            PartyEffect::Done { call, .. } => {
                let count = self.calls.get_mut(&call).expect("a done for a call never made");
                *count += 1;
                assert_eq!(*count, 1, "call {call} done twice");
            }
            PartyEffect::Event(_) => {}
            PartyEffect::Draw => self.draws += 1,
        }
    }

    fn call(&mut self, request: Request) {
        let call = self.next_call;
        self.next_call += 1;
        self.calls.insert(call, 0);
        self.feed(PartyInput::Call { at: self.at(), call, request });
    }

    fn request(&mut self, bytes: &mut Bytes) -> Request {
        match bytes.pick(24) {
            0 => Request::Create,
            1 => Request::Join { code: if bytes.flag() { "k7m-q2x".to_owned() } else { "N7MQ2X".to_owned() }, create: bytes.flag() },
            2 => Request::Leave,
            3 => Request::End,
            4 => Request::SetMedia { media: plx_owp::json::read(HEAT).expect("descriptor").value, reference: "heat".to_owned(), position_ms: bytes.int(1) },
            5 => Request::ClearMedia,
            6 => Request::Play,
            7 => Request::Pause,
            8 => Request::Seek { position_ms: bytes.int(997) - 3 },
            9 => Request::SetRate { rate: f64::from(bytes.byte().unwrap_or(0)) / 64.0 },
            10 => Request::SetOffset { offset_ms: bytes.int(10_000) - 300_000_000 },
            11 => Request::SetControl { mode: if bytes.flag() { Control::Everyone } else { Control::Host } },
            12 => Request::Lock { locked: bytes.flag() },
            13 => Request::Kick { peer_id: if bytes.flag() { "ben".to_owned() } else { "zed".to_owned() } },
            14 => Request::TransferHost { peer_id: "ben".to_owned() },
            15 => Request::Choose { candidate: candidate(bytes) },
            16 => Request::AcceptMismatch,
            17 => Request::DeclineMismatch,
            18 => Request::ResolveAgain,
            19 => {
                let player = bytes.pick(3) as u64 + 1;
                self.attached = Some(player);
                Request::AttachPlayer { player, reference: if bytes.flag() { "heat".to_owned() } else { "a".to_owned() }, can_set_rate: bytes.flag() }
            }
            20 => {
                let player = bytes.flag().then(|| bytes.pick(3) as u64 + 1);
                if player.is_none() || player == self.attached {
                    self.attached = None;
                }
                Request::DetachPlayer { player }
            }
            21 => Request::PlayerChanged,
            22 => Request::Refresh,
            _ => Request::Close,
        }
    }

    fn live_conn(&self, bytes: &mut Bytes, opened: bool) -> Option<u64> {
        let live: Vec<u64> = self.conns.iter().filter(|(_, conn)| !conn.gone && conn.opened == opened).map(|(id, _)| *id).collect();
        (!live.is_empty()).then(|| live[bytes.pick(live.len())])
    }

    fn act(&mut self, bytes: &mut Bytes) {
        match bytes.pick(14) {
            0 | 1 => {
                let request = self.request(bytes);
                self.call(request);
            }
            2 => {
                if let Some(conn) = self.live_conn(bytes, false) {
                    let fails = bytes.pick(4) == 0;
                    if let Some(state) = self.conns.get_mut(&conn) {
                        state.opened = !fails;
                        state.gone = fails;
                    }
                    self.feed(PartyInput::Net { at: self.at(), conn, event: if fails { NetEvent::OpenFailed } else { NetEvent::Opened } });
                }
            }
            3 | 4 => {
                if let Some(conn) = self.live_conn(bytes, true) {
                    let text = message(bytes);
                    self.feed(PartyInput::Net { at: self.at(), conn, event: NetEvent::Text(text) });
                }
            }
            5 => {
                if let Some(conn) = self.live_conn(bytes, true) {
                    if let Some(state) = self.conns.get_mut(&conn) {
                        state.gone = true;
                    }
                    let code = [None, Some(1000), Some(1001), Some(4000)][bytes.pick(4)];
                    self.feed(PartyInput::Net { at: self.at(), conn, event: NetEvent::Closed(code) });
                }
            }
            6 => {
                if let Some(&get) = self.gets.iter().nth(bytes.pick(self.gets.len().max(1))) {
                    self.gets.remove(&get);
                    self.ended_ids.insert(get);
                    let body = [Some(LIST.to_owned()), Some("[]".to_owned()), None][bytes.pick(3)].clone();
                    self.feed(PartyInput::Got { at: self.at(), get, body });
                }
            }
            7 => {
                if let Some((&player, &cmd)) = self.running.iter().nth(bytes.pick(self.running.len().max(1))) {
                    self.running.remove(&player);
                    self.ended_ids.insert(cmd);
                    let threw = bytes.flag();
                    self.feed(PartyInput::CommandDone { at: self.at(), cmd, threw });
                }
            }
            8 => {
                if let Some(&search) = self.resolves.iter().next() {
                    self.resolves.remove(&search);
                    self.ended_ids.insert(search);
                    let answer = bytes.flag().then(|| (0..bytes.pick(4)).map(|_| candidate(bytes)).collect());
                    self.feed(PartyInput::Resolved { at: self.at(), search, answer });
                }
            }
            9 => {
                if let Some(&probe) = self.probes.iter().nth(bytes.pick(self.probes.len().max(1))) {
                    self.probes.remove(&probe);
                    self.ended_ids.insert(probe);
                    let duration_ms = bytes.flag().then(|| bytes.int(157));
                    self.feed(PartyInput::Probed { at: self.at(), probe, duration_ms });
                }
            }
            10 => {
                if self.draws > 0 {
                    self.draws -= 1;
                    let u = f64::from(bytes.byte().unwrap_or(0)) / 256.0;
                    self.feed(PartyInput::Drawn { at: self.at(), u });
                }
            }
            11 => {
                self.now += bytes.int(3);
                self.wake_until_now();
            }
            12 => {
                let dead = self.ended_ids.iter().nth(bytes.pick(self.ended_ids.len().max(1))).copied().unwrap_or(9999);
                let input = match bytes.pick(5) {
                    0 => PartyInput::CommandDone { at: self.at(), cmd: dead, threw: false },
                    1 => PartyInput::Resolved { at: self.at(), search: dead, answer: Some(Vec::new()) },
                    2 => PartyInput::Probed { at: self.at(), probe: dead, duration_ms: Some(1) },
                    3 => PartyInput::Got { at: self.at(), get: dead, body: Some(LIST.to_owned()) },
                    _ => PartyInput::Net { at: self.at(), conn: dead, event: NetEvent::Opened },
                };
                self.feed(input);
            }
            _ => {
                self.reads.paused = bytes.flag();
                self.reads.buffering = bytes.flag();
                self.reads.position_ms = bytes.int(311);
                self.reads.first_frame_shown = bytes.pick(5) != 0;
            }
        }
    }

    fn wake_until_now(&mut self) {
        for _ in 0..10_000 {
            match self.party.next_wake_ms() {
                Some(due) if due <= self.now => self.feed(PartyInput::Wake { at: self.at() }),
                Some(_) | None => return,
            }
        }
        panic!("a Wake made no progress");
    }

    fn drain(&mut self) {
        self.call(Request::Close);
        for _ in 0..100_000 {
            if self.draws > 0 {
                self.draws -= 1;
                self.feed(PartyInput::Drawn { at: self.at(), u: 0.5 });
            } else if let Some(conn) = self.conns.iter().find(|(_, conn)| !conn.gone && !conn.opened).map(|(id, _)| *id) {
                if let Some(state) = self.conns.get_mut(&conn) {
                    state.gone = true;
                }
                self.feed(PartyInput::Net { at: self.at(), conn, event: NetEvent::OpenFailed });
            } else if let Some(get) = self.gets.pop_first() {
                self.feed(PartyInput::Got { at: self.at(), get, body: None });
            } else if let Some((player, cmd)) = self.running.pop_first() {
                let _ = player;
                self.feed(PartyInput::CommandDone { at: self.at(), cmd, threw: false });
            } else if let Some(search) = self.resolves.pop_first() {
                self.feed(PartyInput::Resolved { at: self.at(), search, answer: None });
            } else if let Some(probe) = self.probes.pop_first() {
                self.feed(PartyInput::Probed { at: self.at(), probe, duration_ms: None });
            } else if let Some(due) = self.party.next_wake_ms() {
                self.now = self.now.max(due);
                self.feed(PartyInput::Wake { at: self.at() });
            } else {
                break;
            }
        }
        assert_eq!(self.party.next_wake_ms(), None, "a deadline outlived close and drain");
        assert!(self.calls.values().all(|count| *count == 1), "a call without its Done: {:?}", self.calls);
        assert!(self.conns.values().all(|conn| conn.gone), "an Open that never reached Close");
    }
}

fn candidate(bytes: &mut Bytes) -> Candidate {
    let id = ["a", "b", "c", "heat"][bytes.pick(4)].to_owned();
    Candidate {
        reference: id.clone(),
        id,
        duration_ms: bytes.flag().then(|| 10_200_000 + bytes.int(1) - 32_768),
        size_bytes: None,
        oshash: None,
        name: None,
        probe: bytes.flag(),
    }
}

fuzz_target!(|data: &[u8]| {
    let mut bytes = Bytes { data, at: 0 };
    let source = if bytes.flag() { RelaySource::Public } else { RelaySource::At(relay_at("http://relay.test").expect("relay")) };
    let mut model = Model {
        party: Party::new("Ann", "Example/1.0", bytes.flag(), source),
        now: 0,
        next_call: 1,
        calls: BTreeMap::new(),
        conns: BTreeMap::new(),
        gets: BTreeSet::new(),
        running: BTreeMap::new(),
        resolves: BTreeSet::new(),
        probes: BTreeSet::new(),
        draws: 0,
        ended_ids: BTreeSet::new(),
        attached: None,
        reads: Reads { position_ms: 0, paused: true, buffering: false, duration_ms: Some(10_200_000), first_frame_shown: true, seek_accuracy_ms: 0 },
    };
    for _ in 0..400 {
        if bytes.at >= data.len() {
            break;
        }
        model.act(&mut bytes);
    }
    model.drain();
});
