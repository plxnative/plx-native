use crate::json::{read, Json, Text};
use crate::matching::Candidate;
use crate::message::{Control, MatchState, PlayerState};
use crate::party::deadlines::{Deadlines, Timer};
use crate::party::link::{Link, LinkEffect, LinkEvent};
use crate::party::{relay_at, Moment, NetEvent, Outcome, Party, PartyEffect, PartyEvent, PartyInput, PlayerAction, Reads, RelaySource, Request};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

const TRACES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/vectors/party.json");

fn field<'a>(value: &'a Json, name: &str) -> &'a Json {
    value.get(name).unwrap_or(&Json::Null)
}

fn text(value: &Json, name: &str) -> Option<String> {
    value.get(name).and_then(Json::as_text).map(|text| text.value.clone())
}

fn string(value: &str) -> Json {
    Json::Str(Text::new(value))
}

fn object(members: Vec<(&str, Json)>) -> Json {
    Json::Object(members.into_iter().map(|(name, value)| (Text::new(name), value)).collect())
}

fn opt_int(value: Option<i64>) -> Json {
    value.map_or(Json::Null, Json::Int)
}

fn opt_string(value: Option<&str>) -> Json {
    value.map_or(Json::Null, string)
}

fn same(a: &Json, b: &Json) -> bool {
    match (a, b) {
        (Json::Null, Json::Null) => true,
        (Json::Bool(x), Json::Bool(y)) => x == y,
        (Json::Str(x), Json::Str(y)) => x.value == y.value,
        (Json::Array(x), Json::Array(y)) => x.len() == y.len() && x.iter().zip(y).all(|(p, q)| same(p, q)),
        (Json::Object(x), Json::Object(y)) => x.len() == y.len() && x.iter().all(|(k, v)| y.iter().any(|(k2, v2)| k.value == k2.value && same(v, v2))),
        _ => matches!((a.as_number(), b.as_number()), (Some(p), Some(q)) if (p - q).abs() < 1e-9),
    }
}

#[derive(Debug, Clone)]
struct Conn {
    url: String,
    opened: bool,
    closed: bool,
}

#[derive(Debug, Clone)]
struct Adapter {
    id: u64,
    name: String,
    position_ms: i64,
    set_at_ms: i64,
    paused: bool,
    buffering: bool,
    duration_ms: Option<i64>,
    first_frame_shown: bool,
    can_set_rate: bool,
    seek_accuracy_ms: i64,
    play_throws: bool,
    rate: f64,
    hold_next: bool,
    held: Option<(u64, PlayerAction)>,
}

impl Adapter {
    fn position(&self, now: i64) -> i64 {
        if self.paused || self.buffering {
            self.position_ms
        } else {
            self.position_ms + ((now - self.set_at_ms) as f64 * self.rate).floor() as i64
        }
    }

    fn anchor(&mut self, now: i64) {
        self.position_ms = self.position(now);
        self.set_at_ms = now;
    }

    fn apply(&mut self, action: PlayerAction, threw: bool, now: i64) {
        self.anchor(now);
        if threw {
            return;
        }
        match action {
            PlayerAction::Play => self.paused = false,
            PlayerAction::Pause => self.paused = true,
            PlayerAction::Seek { position_ms } => self.position_ms = position_ms,
            PlayerAction::SetRate { rate } => self.rate = rate,
        }
    }
}

#[derive(Debug)]
enum Ready {
    Open(u64),
    OpenFail(u64),
    Get(u64, Option<String>),
    Command(u64, u64, PlayerAction),
    Draw,
}

#[derive(Debug, Default)]
struct Output {
    send: Vec<(String, Json)>,
    command: Vec<Json>,
    event: Vec<Json>,
    resolver: Vec<Json>,
    net: Vec<(String, Json)>,
    done: Vec<Json>,
}

struct Driver {
    party: Party,
    now: i64,
    relay_offset: i64,
    rtt_default: Option<i64>,
    rtt_by_url: BTreeMap<String, Option<i64>>,
    random: VecDeque<f64>,
    next_open: VecDeque<(Option<String>, String)>,
    next_get: VecDeque<Json>,
    list: Option<String>,
    conns: BTreeMap<u64, Conn>,
    pongs: Vec<(i64, u64, u64, i64, i64)>,
    ready: VecDeque<Ready>,
    adapters: BTreeMap<String, Adapter>,
    attached: Option<String>,
    calls: BTreeMap<u64, String>,
    pending_calls: BTreeSet<String>,
    next_call: u64,
    resolving: Option<(u64, String)>,
    probes: BTreeMap<String, u64>,
    out: Output,
    faults: Vec<String>,
}

impl Driver {
    fn new(setup: &Json) -> Driver {
        let name = text(setup, "name").unwrap_or_default();
        let client = text(setup, "client").unwrap_or_default();
        let resolver = matches!(setup.get("resolver"), Some(Json::Bool(true)));
        let source = match text(setup, "base") {
            Some(base) => RelaySource::At(relay_at(&base).expect("base")),
            None => RelaySource::Public,
        };
        let random = match field(setup, "random") {
            Json::Array(draws) => draws.iter().filter_map(Json::as_number).collect(),
            _ => VecDeque::new(),
        };
        Driver {
            party: Party::new(&name, &client, resolver, source),
            now: field(setup, "startMs").as_int().unwrap_or(0),
            relay_offset: field(setup, "relayOffsetMs").as_int().unwrap_or(0),
            rtt_default: field(setup, "rttMs").as_int(),
            rtt_by_url: BTreeMap::new(),
            random,
            next_open: VecDeque::new(),
            next_get: VecDeque::new(),
            list: match setup.get("list") {
                Some(Json::Str(list)) => Some(list.value.clone()),
                Some(Json::Null) | None => None,
                Some(list) => Some(list.compact()),
            },
            conns: BTreeMap::new(),
            pongs: Vec::new(),
            ready: VecDeque::new(),
            adapters: BTreeMap::new(),
            attached: None,
            calls: BTreeMap::new(),
            pending_calls: BTreeSet::new(),
            next_call: 1000,
            resolving: None,
            probes: BTreeMap::new(),
            out: Output::default(),
            faults: Vec::new(),
        }
    }

    fn reads(&self) -> Option<Reads> {
        let adapter = self.adapters.get(self.attached.as_ref()?)?;
        Some(Reads {
            position_ms: adapter.position(self.now),
            paused: adapter.paused,
            buffering: adapter.buffering,
            duration_ms: adapter.duration_ms,
            first_frame_shown: adapter.first_frame_shown,
            seek_accuracy_ms: adapter.seek_accuracy_ms,
        })
    }

    fn at(&self) -> Moment {
        Moment { now_ms: self.now, player: self.reads() }
    }

    fn feed(&mut self, input: PartyInput) {
        let mut effects = Vec::new();
        self.party.step(input, &mut effects);
        for effect in effects {
            self.effect(effect);
        }
    }

    fn rtt_for(&self, url: &str) -> Option<i64> {
        self.rtt_by_url.get(url).copied().unwrap_or(self.rtt_default)
    }

    fn adapter_name(&self, id: u64) -> String {
        self.adapters.values().find(|adapter| adapter.id == id).map(|adapter| adapter.name.clone()).unwrap_or_default()
    }

    fn effect(&mut self, effect: PartyEffect) {
        match effect {
            PartyEffect::Open { conn, url } => {
                self.out.net.push((url.clone(), object(vec![("open", string(&url))])));
                self.conns.insert(conn, Conn { url: url.clone(), opened: false, closed: false });
                let scripted = self.next_open.iter().position(|(on, _)| on.as_deref() == Some(url.as_str())).or_else(|| self.next_open.iter().position(|(on, _)| on.is_none()));
                match scripted.and_then(|at| self.next_open.remove(at)).map(|(_, how)| how) {
                    Some(how) if how == "fail" => self.ready.push_back(Ready::OpenFail(conn)),
                    Some(_) => {}
                    None => self.ready.push_back(Ready::Open(conn)),
                }
            }
            PartyEffect::Send { conn, text } => {
                let message = read(&text).expect("sent JSON").value;
                let url = self.conns.get(&conn).map(|conn| conn.url.clone()).unwrap_or_default();
                if field(&message, "type").as_text().is_some_and(|kind| kind.value == "ping") {
                    if let (Some(rtt), Some(id)) = (self.rtt_for(&url), field(&message, "id").as_int()) {
                        let server_time = self.now + rtt / 2 + self.relay_offset;
                        let order = self.party.timers_set();
                        self.pongs.push((self.now + rtt, order, conn, id, server_time));
                    }
                } else {
                    self.out.send.push((url, message));
                }
            }
            PartyEffect::Close { conn, code } => {
                if let Some(state) = self.conns.get_mut(&conn) {
                    if state.opened && !state.closed {
                        self.out.net.push((state.url.clone(), object(vec![("close", string(&state.url)), ("code", Json::Int(i64::from(code)))])));
                    }
                    state.closed = true;
                }
                self.pongs.retain(|pong| pong.2 != conn);
            }
            PartyEffect::Get { get, url } => {
                self.out.net.push((url.clone(), object(vec![("get", string(&url))])));
                match self.next_get.pop_front() {
                    Some(Json::Str(how)) if how.value == "fail" => self.ready.push_back(Ready::Get(get, None)),
                    Some(Json::Str(_)) => {}
                    Some(doc) => self.ready.push_back(Ready::Get(get, Some(doc.compact()))),
                    None => self.ready.push_back(Ready::Get(get, self.list.clone())),
                }
            }
            PartyEffect::AbortGet { get } => self.ready.retain(|ready| !matches!(ready, Ready::Get(g, _) if *g == get)),
            PartyEffect::Command { cmd, player, action } => {
                let name = self.adapter_name(player);
                let mut entry = vec![("player", string(&name))];
                match action {
                    PlayerAction::Play => entry.push(("do", string("play"))),
                    PlayerAction::Pause => entry.push(("do", string("pause"))),
                    PlayerAction::Seek { position_ms } => {
                        entry.push(("do", string("seek")));
                        entry.push(("positionMs", Json::Int(position_ms)));
                    }
                    PlayerAction::SetRate { rate } => {
                        entry.push(("do", string("setRate")));
                        entry.push(("rate", Json::Float((rate * 1e6).round() / 1e6)));
                    }
                }
                self.out.command.push(object(entry));
                if let Some(adapter) = self.adapters.get_mut(&name) {
                    if adapter.hold_next {
                        adapter.hold_next = false;
                        adapter.held = Some((cmd, action));
                        return;
                    }
                }
                self.ready.push_back(Ready::Command(cmd, player, action));
            }
            PartyEffect::Resolve { search, media } => {
                let title = text(&media, "title").unwrap_or_default();
                self.out.resolver.push(object(vec![("resolve", string(&title))]));
                self.resolving = Some((search, title));
            }
            PartyEffect::CancelResolve { search } => {
                if let Some((running, title)) = self.resolving.take() {
                    if running == search {
                        self.out.resolver.push(object(vec![("resolveCancelled", string(&title))]));
                    } else {
                        self.resolving = Some((running, title));
                    }
                }
            }
            PartyEffect::Probe { probe, candidate } => {
                self.out.resolver.push(object(vec![("probe", string(&candidate))]));
                self.probes.insert(candidate, probe);
            }
            PartyEffect::CancelProbe { probe } => {
                if let Some(candidate) = self.probes.iter().find(|(_, running)| **running == probe).map(|(id, _)| id.clone()) {
                    self.probes.remove(&candidate);
                    self.out.resolver.push(object(vec![("probeCancelled", string(&candidate))]));
                }
            }
            PartyEffect::Done { call, outcome } => {
                let Some(id) = self.calls.get(&call).cloned() else {
                    if let Outcome::Failed(error) = outcome {
                        self.faults.push(format!("a call without id failed: {}", error.code));
                    }
                    return;
                };
                if !self.pending_calls.remove(&id) {
                    self.faults.push(format!("done twice for {id}"));
                }
                let mut entry = vec![("id", string(&id))];
                match outcome {
                    Outcome::Done => {}
                    Outcome::Code(code) => entry.push(("value", string(&code))),
                    Outcome::Answer(answer) => entry.push(("value", Json::Bool(answer))),
                    Outcome::Failed(error) => {
                        entry.push(("error", string(&error.code)));
                        entry.push(("re", opt_string(error.re.as_deref())));
                        entry.push(("message", string(&error.message)));
                    }
                }
                self.out.done.push(object(entry));
            }
            PartyEffect::Event(event) => self.out.event.push(event_json(event)),
            PartyEffect::Draw => self.ready.push_front(Ready::Draw),
        }
    }

    fn settle(&mut self) {
        loop {
            if let Some(ready) = self.ready.pop_front() {
                match ready {
                    Ready::Open(conn) => {
                        let live = self.conns.get_mut(&conn).filter(|conn| !conn.closed).map(|conn| conn.opened = true).is_some();
                        if live {
                            self.feed(PartyInput::Net { at: self.at(), conn, event: NetEvent::Opened });
                        }
                    }
                    Ready::OpenFail(conn) => {
                        if let Some(state) = self.conns.get_mut(&conn) {
                            state.closed = true;
                        }
                        self.feed(PartyInput::Net { at: self.at(), conn, event: NetEvent::OpenFailed });
                    }
                    Ready::Get(get, body) => self.feed(PartyInput::Got { at: self.at(), get, body }),
                    Ready::Command(cmd, player, action) => {
                        let name = self.adapter_name(player);
                        let now = self.now;
                        let threw = self.adapters.get_mut(&name).map(|adapter| {
                            let threw = action == PlayerAction::Play && adapter.play_throws;
                            adapter.apply(action, threw, now);
                            threw
                        });
                        self.feed(PartyInput::CommandDone { at: self.at(), cmd, threw: threw.unwrap_or(false) });
                    }
                    Ready::Draw => match self.random.pop_front() {
                        Some(u) => self.feed(PartyInput::Drawn { at: self.at(), u }),
                        None => self.faults.push("one more draw than scripted".into()),
                    },
                }
                continue;
            }
            let core = self.party.next_wake_order().filter(|(due, _)| *due <= self.now);
            let pong = self.pongs.iter().enumerate().filter(|(_, pong)| pong.0 <= self.now).min_by_key(|(_, pong)| (pong.0, pong.1)).map(|(at, pong)| (at, pong.0, pong.1));
            match (core, pong) {
                (Some((due, order)), Some((_, pong_due, pong_order))) if (due, order) <= (pong_due, pong_order) => self.feed(PartyInput::Wake { at: self.at() }),
                (_, Some((at, _, _))) => {
                    let (_, _, conn, id, server_time) = self.pongs.remove(at);
                    if self.conns.get(&conn).is_some_and(|conn| conn.opened && !conn.closed) {
                        let pong = format!(r#"{{"type":"pong","id":{id},"serverTime":{server_time}}}"#);
                        self.feed(PartyInput::Net { at: self.at(), conn, event: NetEvent::Text(pong) });
                    }
                }
                (Some(_), None) => self.feed(PartyInput::Wake { at: self.at() }),
                (None, None) => return,
            }
        }
    }

    fn advance(&mut self, ms: i64) {
        let end = self.now + ms;
        loop {
            let core = self.party.next_wake_ms();
            let pong = self.pongs.iter().map(|pong| pong.0).min();
            let next = [core, pong].into_iter().flatten().min();
            match next {
                Some(next) if next <= end => {
                    self.now = self.now.max(next);
                    self.settle();
                }
                Some(_) | None => break,
            }
        }
        self.now = end;
        self.settle();
    }

    fn conn_for(&self, on: Option<&str>) -> Option<u64> {
        self.conns.iter().rev().find(|(_, conn)| conn.opened && !conn.closed && on.is_none_or(|url| conn.url == url)).map(|(id, _)| *id)
    }

    fn adapter(&mut self, name: &str) -> &mut Adapter {
        let id = self.adapters.len() as u64 + 1;
        let now = self.now;
        self.adapters.entry(name.to_owned()).or_insert_with(|| Adapter {
            id,
            name: name.to_owned(),
            position_ms: 0,
            set_at_ms: now,
            paused: true,
            buffering: false,
            duration_ms: None,
            first_frame_shown: true,
            can_set_rate: false,
            seek_accuracy_ms: 0,
            play_throws: false,
            rate: 1.0,
            hold_next: false,
            held: None,
        })
    }

    fn call(&mut self, step: &Json) {
        let call = self.next_call;
        self.next_call += 1;
        if let Some(id) = text(step, "id") {
            self.calls.insert(call, id.clone());
            self.pending_calls.insert(id);
        }
        let name = text(step, "call").unwrap_or_default();
        let int = |member: &str| field(step, member).as_int().unwrap_or(0);
        let request = match name.as_str() {
            "create" => Request::Create,
            "join" => Request::Join { code: text(step, "code").unwrap_or_default(), create: matches!(step.get("create"), Some(Json::Bool(true))) },
            "leave" => Request::Leave,
            "end" => Request::End,
            "close" => {
                self.attached = None;
                Request::Close
            }
            "setMedia" => Request::SetMedia { media: field(step, "media").clone(), reference: text(step, "ref").unwrap_or_default(), position_ms: step.get("positionMs").and_then(Json::as_int).unwrap_or(0) },
            "clearMedia" => Request::ClearMedia,
            "play" => Request::Play,
            "pause" => Request::Pause,
            "seek" => Request::Seek { position_ms: field(step, "ms").as_int().unwrap_or(-1) },
            "setRate" => Request::SetRate { rate: field(step, "rate").as_number().unwrap_or(f64::NAN) },
            "setOffset" => Request::SetOffset { offset_ms: field(step, "ms").as_int().unwrap_or(i64::MAX) },
            "setControl" => Request::SetControl { mode: if text(step, "mode").as_deref() == Some("everyone") { Control::Everyone } else { Control::Host } },
            "lock" => Request::Lock { locked: matches!(step.get("locked"), Some(Json::Bool(true))) },
            "kick" => Request::Kick { peer_id: text(step, "peerId").unwrap_or_default() },
            "transferHost" => Request::TransferHost { peer_id: text(step, "peerId").unwrap_or_default() },
            "choose" => Request::Choose { candidate: candidate_of(field(step, "candidate")) },
            "acceptMismatch" => Request::AcceptMismatch,
            "declineMismatch" => Request::DeclineMismatch,
            "resolveAgain" => Request::ResolveAgain,
            "attachPlayer" => {
                let player = text(step, "player").unwrap_or_default();
                let adapter = self.adapter(&player);
                let (id, can_set_rate) = (adapter.id, adapter.can_set_rate);
                self.attached = Some(player);
                Request::AttachPlayer { player: id, reference: text(step, "ref").unwrap_or_default(), can_set_rate }
            }
            "detachPlayer" => {
                let player = text(step, "player");
                let id = player.as_ref().map(|player| self.adapter(player).id);
                if player.is_none() || player == self.attached {
                    self.attached = None;
                }
                Request::DetachPlayer { player: id }
            }
            "playerChanged" => Request::PlayerChanged,
            "refresh" => Request::Refresh,
            other => panic!("unknown call {other}"),
        };
        let _ = int;
        self.feed(PartyInput::Call { at: self.at(), call, request });
    }

    fn run(&mut self, step: &Json) {
        if step.get("call").is_some() {
            self.call(step);
        } else if let Some(message) = step.get("recv") {
            if let Some(conn) = self.conn_for(text(step, "on").as_deref()) {
                self.feed(PartyInput::Net { at: self.at(), conn, event: NetEvent::Text(message.compact()) });
            }
        } else if let Some(raw) = text(step, "recvText") {
            if let Some(conn) = self.conn_for(text(step, "on").as_deref()) {
                self.feed(PartyInput::Net { at: self.at(), conn, event: NetEvent::Text(raw) });
            }
        } else if let Some(code) = step.get("drop") {
            if let Some(conn) = self.conn_for(text(step, "on").as_deref()) {
                if let Some(state) = self.conns.get_mut(&conn) {
                    state.closed = true;
                }
                let code = code.as_int().and_then(|code| u16::try_from(code).ok());
                self.feed(PartyInput::Net { at: self.at(), conn, event: NetEvent::Closed(code) });
            }
        } else if let Some(how) = text(step, "nextOpen") {
            self.next_open.push_back((text(step, "on"), how));
        } else if let Some(rtt) = step.get("rtt") {
            let rtt = rtt.as_int();
            match text(step, "on") {
                Some(url) => {
                    self.rtt_by_url.insert(url, rtt);
                }
                None => {
                    self.rtt_by_url.clear();
                    self.rtt_default = rtt;
                }
            }
        } else if let Some(doc) = step.get("nextGet") {
            self.next_get.push_back(doc.clone());
        } else if let Some(ms) = step.get("advance").and_then(Json::as_int) {
            self.advance(ms);
            return;
        } else if let Some(player) = text(step, "player") {
            let now = self.now;
            let adapter = self.adapter(&player);
            adapter.anchor(now);
            if let Some(position) = step.get("positionMs").and_then(Json::as_int) {
                adapter.position_ms = position;
            }
            if let Some(Json::Bool(paused)) = step.get("paused") {
                adapter.paused = *paused;
            }
            if let Some(Json::Bool(buffering)) = step.get("buffering") {
                adapter.buffering = *buffering;
            }
            if let Some(duration) = step.get("durationMs") {
                adapter.duration_ms = duration.as_int();
            }
            if let Some(Json::Bool(shown)) = step.get("firstFrameShown") {
                adapter.first_frame_shown = *shown;
            }
            if let Some(Json::Bool(can)) = step.get("canSetRate") {
                adapter.can_set_rate = *can;
            }
            if let Some(accuracy) = step.get("seekAccuracyMs").and_then(Json::as_int) {
                adapter.seek_accuracy_ms = accuracy;
            }
            if let Some(Json::Bool(throws)) = step.get("playThrows") {
                adapter.play_throws = *throws;
            }
        } else if let Some(player) = text(step, "hold") {
            self.adapter(&player).hold_next = true;
        } else if let Some(player) = text(step, "finish") {
            let throws = matches!(step.get("throws"), Some(Json::Bool(true)));
            let now = self.now;
            let held = self.adapter(&player).held.take();
            if let Some((cmd, action)) = held {
                let adapter = self.adapter(&player);
                let threw = throws || (action == PlayerAction::Play && adapter.play_throws);
                adapter.apply(action, threw, now);
                self.feed(PartyInput::CommandDone { at: self.at(), cmd, threw });
            }
        } else if let Some(Json::Array(candidates)) = step.get("answer") {
            if let Some((search, _)) = self.resolving.take() {
                let answer = candidates.iter().map(candidate_of).collect();
                self.feed(PartyInput::Resolved { at: self.at(), search, answer: Some(answer) });
            }
        } else if step.get("answerThrows").is_some() {
            if let Some((search, _)) = self.resolving.take() {
                self.feed(PartyInput::Resolved { at: self.at(), search, answer: None });
            }
        } else if let Some(id) = text(step, "probeAnswer") {
            if let Some(probe) = self.probes.remove(&id) {
                self.feed(PartyInput::Probed { at: self.at(), probe, duration_ms: field(step, "durationMs").as_int() });
            }
        } else if let Some(id) = text(step, "probeThrows") {
            if let Some(probe) = self.probes.remove(&id) {
                self.feed(PartyInput::Probed { at: self.at(), probe, duration_ms: None });
            }
        } else {
            panic!("unknown step {}", step.compact());
        }
        self.settle();
    }

    fn state_json(&self, member: &str) -> Json {
        let state = self.party.state();
        match member {
            "connection" => string(state.connection.name()),
            "peerId" => opt_string(state.peer_id.as_deref()),
            "offsetMs" => Json::Int(state.offset_ms),
            "player" => string(player_name(state.player)),
            "endReason" => opt_string(state.end_reason.map(|reason| reason.name())),
            "isHost" => Json::Bool(state.is_host),
            "canControl" => Json::Bool(state.can_control),
            "room" => state.room.as_ref().map_or(Json::Null, |room| field(room, "rev").clone()),
            "media" => state.media.as_ref().map_or(Json::Null, |media| field(media, "title").clone()),
            "match" => state.report.map_or(Json::Null, |report| {
                let mut members = vec![("state", string(match_name(report.state)))];
                if let Some(delta) = report.delta_ms {
                    members.push(("deltaMs", Json::Int(delta)));
                }
                object(members)
            }),
            "choice" => state.choice.as_ref().map_or(Json::Null, |choice| object(vec![("id", string(&choice.id)), ("ref", string(&choice.reference))])),
            "mismatch" => state.mismatch.as_ref().map_or(Json::Null, |(candidate, delta)| object(vec![("id", string(&candidate.id)), ("deltaMs", Json::Int(*delta))])),
            "candidates" => Json::Array(state.candidates.iter().map(|(candidate, delta)| object(vec![("id", string(&candidate.id)), ("deltaMs", opt_int(*delta))])).collect()),
            "roomPositionMs" => opt_int(state.room_position_ms(self.now)),
            other => panic!("unknown state member {other}"),
        }
    }

    fn compare(&mut self, expect: &Json) -> Vec<String> {
        let out = std::mem::take(&mut self.out);
        let mut problems = Vec::new();
        let list = |name: &str| match expect.get(name) {
            Some(Json::Array(items)) => items.clone(),
            _ => Vec::new(),
        };
        let sends = list("send");
        let send_ok = sends.len() == out.send.len()
            && sends.iter().zip(&out.send).all(|(want, (url, got))| match (want.get("on"), want.get("msg")) {
                (Some(on), Some(msg)) => on.as_text().is_some_and(|on| on.value == *url) && same(msg, got),
                _ => same(want, got),
            });
        if !send_ok {
            problems.push(format!("send: want {} got {}", Json::Array(sends).compact(), Json::Array(out.send.iter().map(|(_, m)| m.clone()).collect()).compact()));
        }
        for (name, got) in [("command", &out.command), ("event", &out.event), ("resolver", &out.resolver)] {
            let want = list(name);
            if !(want.len() == got.len() && want.iter().zip(got).all(|(w, g)| same(w, g))) {
                problems.push(format!("{name}: want {} got {}", Json::Array(want).compact(), Json::Array(got.clone()).compact()));
            }
        }
        let net_want = list("net");
        let url_of = |entry: &Json| ["open", "get", "close"].iter().find_map(|key| text(entry, key)).unwrap_or_default();
        let mut by_url_want: BTreeMap<String, Vec<Json>> = BTreeMap::new();
        for entry in &net_want {
            by_url_want.entry(url_of(entry)).or_default().push(entry.clone());
        }
        let mut by_url_got: BTreeMap<String, Vec<Json>> = BTreeMap::new();
        for (url, entry) in &out.net {
            by_url_got.entry(url.clone()).or_default().push(entry.clone());
        }
        let net_ok = by_url_want.len() == by_url_got.len() && by_url_want.iter().all(|(url, want)| by_url_got.get(url).is_some_and(|got| want.len() == got.len() && want.iter().zip(got).all(|(w, g)| same(w, g))));
        if !net_ok {
            problems.push(format!("net: want {} got {}", Json::Array(net_want).compact(), Json::Array(out.net.iter().map(|(_, e)| e.clone()).collect()).compact()));
        }
        let done_want = list("done");
        let done_ok = done_want.len() == out.done.len()
            && done_want.iter().all(|want| {
                out.done.iter().any(|got| {
                    let id = text(want, "id") == text(got, "id");
                    let value = match want.get("value") {
                        Some(value) => got.get("value").is_some_and(|got| same(value, got)),
                        None => got.get("value").is_none(),
                    };
                    let error = text(want, "error") == text(got, "error");
                    let re = want.get("re").is_none_or(|re| got.get("re").is_some_and(|got| same(re, got)));
                    let message = want.get("message").is_none_or(|message| got.get("message").is_some_and(|got| same(message, got)));
                    id && value && error && re && message
                })
            });
        if !done_ok {
            problems.push(format!("done: want {} got {}", Json::Array(done_want).compact(), Json::Array(out.done.clone()).compact()));
        }
        if let Some(Json::Object(members)) = expect.get("state") {
            for (name, want) in members {
                let got = self.state_json(&name.value);
                if !same(want, &got) {
                    problems.push(format!("state.{}: want {} got {}", name.value, want.compact(), got.compact()));
                }
            }
        }
        problems
    }
}

fn candidate_of(entry: &Json) -> Candidate {
    let id = text(entry, "id").unwrap_or_default();
    Candidate {
        reference: text(entry, "ref").unwrap_or_else(|| id.clone()),
        id,
        duration_ms: entry.get("durationMs").and_then(Json::as_int),
        size_bytes: entry.get("sizeBytes").and_then(Json::as_int),
        oshash: text(entry, "oshash"),
        name: text(entry, "name"),
        probe: matches!(entry.get("probe"), Some(Json::Bool(true))),
    }
}

fn player_name(player: PlayerState) -> &'static str {
    match player {
        PlayerState::Idle => "idle",
        PlayerState::Loading => "loading",
        PlayerState::Ready => "ready",
        PlayerState::Buffering => "buffering",
    }
}

fn match_name(state: MatchState) -> &'static str {
    match state {
        MatchState::Searching => "searching",
        MatchState::Matched => "matched",
        MatchState::Mismatched => "mismatched",
        MatchState::Unavailable => "unavailable",
    }
}

fn event_json(event: PartyEvent) -> Json {
    match event {
        PartyEvent::Action { kind, by_peer_id, by, by_me, rev } => object(vec![
            ("type", string("action")),
            ("kind", string(&kind)),
            ("byPeerId", opt_string(by_peer_id.as_deref())),
            ("by", opt_string(by.as_deref())),
            ("byMe", Json::Bool(by_me)),
            ("room", Json::Int(rev)),
        ]),
        PartyEvent::ParticipantJoined { peer_id } => object(vec![("type", string("participantJoined")), ("peerId", string(&peer_id))]),
        PartyEvent::ParticipantLeft { peer_id } => object(vec![("type", string("participantLeft")), ("peerId", string(&peer_id))]),
        PartyEvent::Error { error } => object(vec![("type", string("error")), ("code", string(&error.code)), ("re", opt_string(error.re.as_deref()))]),
        PartyEvent::ResolveFailed { timeout } => object(vec![("type", string("resolveFailed")), ("timeout", Json::Bool(timeout))]),
        PartyEvent::PlayBlocked => object(vec![("type", string("playBlocked"))]),
        PartyEvent::Ended { reason, error } => object(vec![("type", string("ended")), ("reason", string(reason.name())), ("error", opt_string(error.as_ref().map(|error| error.code.as_str())))]),
    }
}

fn run_trace(case: &Json) -> Result<(), String> {
    let input = field(case, "input");
    let mut driver = Driver::new(field(input, "setup"));
    let (Json::Array(steps), Json::Array(outputs)) = (field(input, "steps"), field(field(case, "expect"), "outputs")) else { return Err("no steps".into()) };
    for (index, (step, expect)) in steps.iter().zip(outputs).enumerate() {
        driver.run(step);
        let problems = driver.compare(expect);
        if !problems.is_empty() {
            return Err(format!("step {index} {}: {}", step.compact(), problems.join("; ")));
        }
        if let Some(fault) = driver.faults.first() {
            return Err(format!("step {index}: {fault}"));
        }
    }
    if !driver.random.is_empty() {
        return Err(format!("{} draws left", driver.random.len()));
    }
    if driver.resolving.is_some() || !driver.probes.is_empty() {
        return Err("a resolve or probe is unanswered".into());
    }
    if driver.adapters.values().any(|adapter| adapter.held.is_some()) {
        return Err("a held command is still running".into());
    }
    if !driver.pending_calls.is_empty() {
        return Err(format!("calls never done: {:?}", driver.pending_calls));
    }
    Ok(())
}

fn traces() -> Vec<Json> {
    let text = std::fs::read_to_string(TRACES).expect("party.json");
    let table = read(&text).expect("party.json").value;
    match table.get("cases") {
        Some(Json::Array(cases)) => cases.clone(),
        _ => Vec::new(),
    }
}

fn driver_after(trace: usize, steps: usize) -> Driver {
    let cases = traces();
    let input = field(&cases[trace], "input");
    let mut driver = Driver::new(field(input, "setup"));
    if let Json::Array(all) = field(input, "steps") {
        for step in &all[..steps] {
            driver.run(step);
        }
    }
    driver.out = Output::default();
    driver
}

fn step_of(json: &str) -> Json {
    read(json).expect("step").value
}

fn at(now_ms: i64) -> Moment {
    Moment { now_ms, player: None }
}

fn one_relay() -> Party {
    Party::new("Ann", "Example/1.0", false, RelaySource::At(relay_at("http://relay.test").expect("relay")))
}

#[test]
fn deadlines_due_together_fire_in_the_order_they_were_set() {
    let mut deadlines = Deadlines::default();
    deadlines.set(0, 500, Timer::Tick);
    deadlines.set(0, 500, Timer::Heartbeat);
    deadlines.set(100, 400, Timer::Hold);
    assert_eq!(deadlines.take_due(499), None);
    assert_eq!(deadlines.take_due(500), Some((500, Timer::Tick)));
    assert_eq!(deadlines.take_due(500), Some((500, Timer::Heartbeat)));
    assert_eq!(deadlines.take_due(500), Some((500, Timer::Hold)));
    assert_eq!(deadlines.take_due(500), None);
}

#[test]
fn a_timer_past_2147483647_ms_waits_its_whole_delay_once() {
    let mut deadlines = Deadlines::default();
    deadlines.set(1000, 3_000_000_000, Timer::Play);
    assert_eq!(deadlines.next_ms(), Some(3_000_001_000));
    assert_eq!(deadlines.take_due(1000 + 2_147_483_647), None);
    assert_eq!(deadlines.take_due(3_000_001_000), Some((3_000_001_000, Timer::Play)));
    assert_eq!(deadlines.next_ms(), None);
}

#[test]
fn a_link_pings_at_open_then_500_and_1000_then_every_5000_from_open() {
    let mut link = Link::new("ws://relay.test/v1/ws");
    let mut effects = Vec::new();
    link.step(LinkEvent::Opened { at_ms: 1000 }, &mut effects);
    let mut pings = Vec::new();
    for _ in 0..5 {
        pings.extend(effects.iter().filter_map(|effect| match effect {
            LinkEffect::Ping { id } => Some(*id),
            _ => None,
        }));
        let due = effects.iter().find_map(|effect| match effect {
            LinkEffect::PingAt { at_ms } => Some(*at_ms),
            _ => None,
        });
        let due = due.expect("next ping");
        assert_eq!(due - 1000, [500, 1000, 5000, 10_000, 15_000][pings.len() - 1]);
        effects.clear();
        link.step(LinkEvent::PingDue { at_ms: due }, &mut effects);
    }
    assert_eq!(pings, [1, 2, 3, 4, 5]);
}

#[test]
fn a_list_fetch_is_abandoned_after_5000_ms_and_its_late_answer_ignored() {
    let mut party = Party::new("Ann", "Example/1.0", false, RelaySource::Public);
    let mut effects = Vec::new();
    party.step(PartyInput::Call { at: at(0), call: 1, request: Request::Refresh }, &mut effects);
    let get = match effects.as_slice() {
        [PartyEffect::Get { get, .. }] => *get,
        other => panic!("{other:?}"),
    };
    assert_eq!(party.next_wake_ms(), Some(5000));
    effects.clear();
    party.step(PartyInput::Wake { at: at(4999) }, &mut effects);
    assert!(effects.is_empty(), "{effects:?}");
    party.step(PartyInput::Wake { at: at(5000) }, &mut effects);
    assert_eq!(effects, [PartyEffect::AbortGet { get }, PartyEffect::Done { call: 1, outcome: Outcome::Answer(false) }]);
    effects.clear();
    party.step(PartyInput::Got { at: at(5001), get, body: Some(crate::relays::EMBEDDED_PUBLIC_LIST.to_owned()) }, &mut effects);
    assert!(effects.is_empty(), "{effects:?}");
}

#[test]
fn a_computed_wire_position_past_2_pow_53_is_refused_before_any_local_effect() {
    let mut driver = driver_after(2, 7);
    driver.run(&step_of(r#"{"call":"setOffset","ms":-604800000}"#));
    driver.out = Output::default();
    driver.run(&step_of(r#"{"call":"seek","id":"s1","ms":9007199254740990}"#));
    let out = std::mem::take(&mut driver.out);
    assert!(out.send.is_empty() && out.command.is_empty(), "{out:?}");
    assert_eq!(out.done.len(), 1);
    assert_eq!(text(&out.done[0], "error").as_deref(), Some("invalid_message"));
}

#[test]
fn a_wake_before_any_deadline_changes_nothing() {
    let mut party = one_relay();
    let mut effects = Vec::new();
    party.step(PartyInput::Wake { at: at(0) }, &mut effects);
    assert!(effects.is_empty());
    assert_eq!(party.next_wake_ms(), None);
    party.step(PartyInput::Call { at: at(0), call: 1, request: Request::Create }, &mut effects);
    effects.clear();
    party.step(PartyInput::Wake { at: at(9999) }, &mut effects);
    assert!(effects.is_empty(), "{effects:?}");
    assert_eq!(party.next_wake_ms(), Some(10_000));
}

#[test]
fn each_wake_fires_one_due_deadline_so_a_late_host_wakes_until_none_is_due() {
    let mut party = one_relay();
    let mut effects = Vec::new();
    party.step(PartyInput::Call { at: at(0), call: 1, request: Request::Create }, &mut effects);
    let conn = match effects.as_slice() {
        [PartyEffect::Open { conn, .. }] => *conn,
        other => panic!("{other:?}"),
    };
    party.step(PartyInput::Net { at: at(0), conn, event: NetEvent::Opened }, &mut effects);
    let mut wakes = 0;
    while let Some(key) = party.next_wake_order().filter(|(due, _)| *due <= 20_000) {
        party.step(PartyInput::Wake { at: at(20_000) }, &mut effects);
        assert_ne!(party.next_wake_order(), Some(key), "a due Wake made no progress");
        wakes += 1;
        assert!(wakes < 100);
    }
    assert!(wakes >= 2, "{wakes}");
    assert!(effects.iter().any(|effect| matches!(effect, PartyEffect::Done { call: 1, outcome: Outcome::Failed(error) } if error.code == "unreachable")));
}

#[test]
fn a_now_earlier_than_the_last_input_counts_as_the_last() {
    let mut party = one_relay();
    let mut effects = Vec::new();
    party.step(PartyInput::Call { at: at(1000), call: 1, request: Request::Create }, &mut effects);
    let conn = match effects.as_slice() {
        [PartyEffect::Open { conn, .. }] => *conn,
        other => panic!("{other:?}"),
    };
    party.step(PartyInput::Net { at: at(0), conn, event: NetEvent::Opened }, &mut effects);
    assert_eq!(party.next_wake_ms(), Some(1500));
}

#[test]
fn every_party_trace_passes() {
    let cases = traces();
    let failed: Vec<String> = cases
        .iter()
        .enumerate()
        .filter_map(|(index, case)| run_trace(case).err().map(|problem| format!("#{index}: {}", problem.chars().take(300).collect::<String>())))
        .collect();
    assert!(cases.len() == 145 && failed.is_empty(), "{} of {} traces fail: {failed:?}", failed.len(), cases.len());
}

#[test]
#[ignore]
fn count_every_trace() {
    let cases = traces();
    let mut passed = Vec::new();
    let mut failed = Vec::new();
    for (index, case) in cases.iter().enumerate() {
        match run_trace(case) {
            Ok(()) => passed.push(index),
            Err(problem) => failed.push(format!("#{index} {}: {problem}", text(case, "name").unwrap_or_default())),
        }
    }
    for line in &failed {
        println!("FAIL {}", line.chars().take(600).collect::<String>());
    }
    println!("passed {} of {}: {passed:?}", passed.len(), cases.len());
}
