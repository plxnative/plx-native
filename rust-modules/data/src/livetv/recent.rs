//! Live TV's **Recent channels**: the last channels a profile tuned, newest first, at most
//! [`MAX_RECENT`] (SPEC-1: "a newly tuned channel joins at the front; channels already there never
//! move"; when a sixth joins, the last drops).
//!
//! The store owns the list in memory, keyed like the lineup by `(ServerId, dvr, channel)`, and
//! publishes it on [`super::LiveTvView::recent`] as the lineup's own [`Channel`]s (a key the lineup
//! does not list is skipped when the row is read, and kept in the list). It reads no preference and
//! no registry: the app sends `Restore` when it has read the profile's stored list, and `Noted`
//! when a live channel is tuned, and a change leaves as a [`LiveTvFx`] the app persists. What is
//! stored is DURABLE (the server's machine identifier, [`RecentChannelEntry`]) because a `ServerId`
//! is a registry slot, stable only for the life of the process; the two conversions below are pure
//! functions of the [`ClientKeys`] the app already captured for this frame.
use super::Channel;
use plx_plex::plex::session::prefs::RecentChannelEntry;
use plx_plex::plex::{ClientKeys, ServerId};

pub use plx_plex::plex::session::prefs::MAX_RECENT;

/// The in-process identity of a channel: what the lineup's [`Channel`] carries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecentKey {
    pub sid: ServerId,
    pub dvr: String,
    pub channel: String,
}

impl RecentKey {
    pub fn of(ch: &Channel) -> Self {
        Self { sid: ch.sid, dvr: ch.dvr.clone(), channel: ch.channel.clone() }
    }
    fn names(&self, ch: &Channel) -> bool {
        self.sid == ch.sid && self.dvr == ch.dvr && self.channel == ch.channel
    }
}

/// What a change to Recent asks the app to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LiveTvFx {
    /// Add this channel to the seated profile's stored list (the app converts it to its durable
    /// form and merges it into what is stored; an entry already there does not move).
    RememberRecent(RecentKey),
}

/// The list itself, newest first.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct RecentChannels {
    keys: Vec<RecentKey>,
}

impl RecentChannels {
    /// A tune. A channel already in the list stays where it is; a new one joins at the front and the
    /// last drops past [`MAX_RECENT`]. Whether the list changed.
    pub(super) fn note(&mut self, key: RecentKey) -> bool {
        if self.keys.contains(&key) {
            return false;
        }
        self.keys.insert(0, key);
        self.keys.truncate(MAX_RECENT);
        true
    }

    /// The stored list, newest first, replacing what is held. Whether the list changed.
    pub(super) fn restore(&mut self, mut keys: Vec<RecentKey>) -> bool {
        let mut seen = Vec::with_capacity(MAX_RECENT);
        keys.retain(|key| {
            let fresh = !seen.contains(key) && seen.len() < MAX_RECENT;
            if fresh { seen.push(key.clone()); }
            fresh
        });
        if keys == self.keys { return false; }
        self.keys = keys;
        true
    }

    /// The row: each remembered channel the lineup lists, in order. A channel it does not list is
    /// skipped here and kept in the list (its server may be offline, or its lineup still loading).
    pub(super) fn resolve(&self, lineup: &[Channel]) -> Vec<Channel> {
        self.keys.iter().filter_map(|key| lineup.iter().find(|ch| key.names(ch)).cloned()).collect()
    }
}

/// The durable form of `key`: its server's machine identifier from the roster the app captured.
/// `None` for a server the roster does not hold (or one with no machine identifier): there is
/// nothing durable to write for it.
pub fn entry_of(key: &RecentKey, keys: &ClientKeys) -> Option<RecentChannelEntry> {
    let machine = &keys.facts(key.sid)?.machine_id;
    let entry = RecentChannelEntry::new(machine, &key.dvr, &key.channel);
    (!entry.is_blank()).then_some(entry)
}

/// The stored list (oldest first, as the memory door keeps it) as this process's keys, NEWEST first.
/// An entry whose server is not in the roster now is left out, and stays in storage.
pub fn keys_of(stored: &[RecentChannelEntry], keys: &ClientKeys) -> Vec<RecentKey> {
    stored.iter().rev().filter_map(|entry| {
        let sid = keys.slots().find(|&sid| keys.facts(sid).is_some_and(|f| f.machine_id == entry.machine_id))?;
        Some(RecentKey { sid, dvr: entry.dvr.clone(), channel: entry.channel.clone() })
    }).collect()
}

/// **When the stored list is read again**: the first time, when the seated profile changes, and when
/// the server roster moves (the seat arrives BEFORE the servers register, so the first read finds
/// no server to resolve and the roster moving is what completes it).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RecentSync {
    seen: Option<(u32, u32)>,
}

impl RecentSync {
    /// Whether a restore is due for profile scope `scope` under this roster; says yes once per change.
    pub fn due(&mut self, scope: u32, keys: &ClientKeys) -> bool {
        let now = (scope, keys.roster_gen());
        let due = self.seen != Some(now);
        self.seen = Some(now);
        due
    }
}
