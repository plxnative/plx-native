use crate::json::Json;
use crate::media::{self, Direction};
use crate::timeline::{Phase, Timeline};

pub fn int(value: &Json, name: &str) -> i64 {
    value.get(name).and_then(Json::as_int).unwrap_or(0)
}

pub fn text(value: &Json, name: &str) -> Option<String> {
    value.get(name).and_then(Json::as_text).map(|text| text.value.clone())
}

pub fn rev(room: &Json) -> i64 {
    int(room, "rev")
}

pub fn epoch(room: &Json) -> i64 {
    int(room, "epoch")
}

pub fn timeline(room: &Json) -> Timeline {
    Timeline {
        phase: text(room, "phase").and_then(|phase| Phase::parse(&phase)).unwrap_or(Phase::Idle),
        position_ms: int(room, "positionMs"),
        at_ms: int(room, "atMs"),
        rate: room.get("rate").and_then(Json::as_number).unwrap_or(1.0),
    }
}

pub fn rate(room: &Json) -> f64 {
    room.get("rate").and_then(Json::as_number).unwrap_or(1.0)
}

fn entries(room: &Json) -> &[Json] {
    if let Some(Json::Array(entries)) = room.get("participants") {
        entries.as_slice()
    } else {
        &[]
    }
}

pub fn peer_ids(room: &Json) -> Vec<String> {
    entries(room).iter().filter_map(|entry| text(entry, "peerId")).collect()
}

pub fn entry(room: &Json, peer_id: &str) -> Option<Json> {
    entries(room).iter().find(|entry| text(entry, "peerId").as_deref() == Some(peer_id)).cloned()
}

pub fn has_peer(room: &Json, peer_id: &str) -> bool {
    entries(room).iter().any(|entry| text(entry, "peerId").as_deref() == Some(peer_id))
}

pub fn is_host(room: &Json, peer_id: &str) -> bool {
    entries(room).iter().any(|entry| text(entry, "peerId").as_deref() == Some(peer_id) && matches!(entry.get("host"), Some(Json::Bool(true))))
}

pub fn everyone_controls(room: &Json) -> bool {
    text(room, "control").as_deref() == Some("everyone")
}

pub fn joined_and_left(before: &Json, after: &Json) -> (Vec<Json>, Vec<Json>) {
    let had = peer_ids(before);
    let has = peer_ids(after);
    let joined = entries(after).iter().filter(|entry| text(entry, "peerId").is_some_and(|peer| !had.contains(&peer))).cloned().collect();
    let left = entries(before).iter().filter(|entry| text(entry, "peerId").is_some_and(|peer| !has.contains(&peer))).cloned().collect();
    (joined, left)
}

pub fn action(room: &Json) -> Option<(String, Option<String>)> {
    let action = room.get("action")?;
    Some((text(action, "kind")?, text(action, "by")))
}

pub fn received_media(raw: Option<&Json>) -> Option<Option<Json>> {
    let raw = raw?;
    if matches!(raw, Json::Null) {
        return Some(None);
    }
    media::check(raw, Direction::Receive).is_ok().then(|| Some(media::known_members(raw)))
}

pub fn duration_ms(media: &Json) -> i64 {
    int(media, "durationMs")
}

pub fn same_value(a: &Json, b: &Json) -> bool {
    if let (Some(p), Some(q)) = (a.as_number(), b.as_number()) {
        return p == q;
    }
    match (a, b) {
        (Json::Null, Json::Null) => true,
        (Json::Bool(x), Json::Bool(y)) => x == y,
        (Json::Str(x), Json::Str(y)) => x == y,
        (Json::Array(x), Json::Array(y)) => x.len() == y.len() && x.iter().zip(y).all(|(p, q)| same_value(p, q)),
        (Json::Object(x), Json::Object(y)) => x.len() == y.len() && x.iter().all(|(name, value)| y.iter().any(|(other, peer)| name == other && same_value(value, peer))),
        (Json::Null | Json::Bool(_) | Json::Int(_) | Json::Float(_) | Json::Str(_) | Json::Array(_) | Json::Object(_), _) => false,
    }
}
