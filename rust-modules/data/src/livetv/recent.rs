//! Live TV's **Recent channels**: the last channels a profile tuned, newest first, at most
//! [`MAX_RECENT`] (SPEC-1: "a newly tuned channel joins at the front; channels already there never
//! move"; when a sixth joins, the last drops).
//!
//! **One list, one owner.** For a seated profile the list is the memory door's own
//! ([`RecentChannelEntry`], oldest first, bound to the account, capped at [`MAX_RECENT`] by the
//! door): the join-at-front rule and the cap are applied THERE, by the app's write
//! (`route::remember`), and nowhere else. The store holds no second list. It is handed what the door
//! holds as part of its captured input ([`RecentSeat::Seated`], every pump) and the row it publishes
//! is a pure function of that, the roster and the lineup ([`resolve`]): an entry whose server is not
//! registered, or whose channel the lineup does not list, is not shown and stays stored, where it
//! still counts toward the door's cap (a channel pushed out by the sixth tune is gone for good, shown
//! or not).
//!
//! With nobody seated nothing can be stored (the door refuses the write), so the tunes of a run with
//! no seat are kept for the session only, in memory ([`Recent::Unseated`]), by the same rule and the
//! same cap, and are dropped the moment a seat arrives: the seat's own list is the row from then on.
//! What is stored names a server by its `machineIdentifier`, never by a `ServerId` (a registry slot,
//! stable only for the life of the process); [`resolve`] joins the two through the [`ClientKeys`]
//! the app captured for this frame.
use super::Channel;
use plx_plex::plex::session::prefs::RecentChannelEntry;
use plx_plex::plex::session::Seat;
use plx_plex::plex::{ClientKeys, ServerId};

pub use plx_plex::plex::session::prefs::MAX_RECENT;

/// The in-process identity of a channel: what the lineup's [`Channel`] carries. It names a tune
/// only for a run with nobody seated (a seated one is written to the door, by durable name).
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

/// **Who is seated, and what the memory door holds for them**, as the app captured it for this
/// frame: one coherent observation, borrowed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecentSeat<'a> {
    Unseated,
    /// `stored` is the seated profile's list as the door holds it, oldest first.
    Seated { seat: &'a Seat, stored: &'a [RecentChannelEntry] },
}

/// **The store's mode**, a type and not a flag: what the seat input last said.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Recent {
    /// Nobody is seated: this run's tunes, newest first, session-only.
    Unseated { session: Vec<RecentKey> },
    /// A profile is seated: the door's list as of the last input, oldest first.
    Seated { seat: Seat, stored: Vec<RecentChannelEntry> },
}

impl Default for Recent {
    fn default() -> Self {
        Recent::Unseated { session: Vec::new() }
    }
}

impl Recent {
    /// Follow the seat input. A seat arriving replaces the session's tunes (they were never anyone's),
    /// a seat leaving starts the next session empty (the profile's tunes are the profile's), and a
    /// seated store simply holds what the door holds now.
    pub(super) fn drive(&mut self, input: RecentSeat<'_>) {
        match (&mut *self, input) {
            (Recent::Unseated { .. }, RecentSeat::Unseated) => {}
            (Recent::Seated { seat, stored }, RecentSeat::Seated { seat: now, stored: held }) => {
                if seat != now { seat.clone_from(now); }
                if stored != held {
                    stored.clear();
                    stored.extend_from_slice(held);
                }
            }
            (_, RecentSeat::Seated { seat, stored }) => {
                *self = Recent::Seated { seat: seat.clone(), stored: stored.to_vec() };
            }
            (_, RecentSeat::Unseated) => *self = Recent::default(),
        }
    }

    /// A tune with nobody seated: session-only. A channel already there stays where it is, a new one
    /// joins at the front and the last drops past [`MAX_RECENT`]. A seated tune is the door's (the app
    /// writes it, and the next input carries it), so it is not noted here. Whether the list changed.
    pub(super) fn note_unseated(&mut self, key: RecentKey) -> bool {
        let Recent::Unseated { session } = self else { return false };
        if session.contains(&key) { return false; }
        session.insert(0, key);
        session.truncate(MAX_RECENT);
        true
    }

    /// The row: at most [`MAX_RECENT`] lineup channels, newest first.
    pub(super) fn row(&self, keys: &ClientKeys, lineup: &[Channel]) -> Vec<Channel> {
        match self {
            Recent::Unseated { session } => session.iter()
                .filter_map(|key| lineup.iter().find(|ch| key.names(ch)).cloned()).take(MAX_RECENT).collect(),
            Recent::Seated { stored, .. } => resolve(stored, keys, lineup),
        }
    }
}

/// **The seated row, a pure function of what is stored, the roster and the lineup.** `stored` is the
/// door's list, oldest first; the row is newest first, at most [`MAX_RECENT`], each entry the lineup's
/// own [`Channel`]. An entry whose server is not in the roster now (offline, signed out of, not yet
/// registered after a start) or whose channel the lineup does not list is skipped here and stays stored.
pub fn resolve(stored: &[RecentChannelEntry], keys: &ClientKeys, lineup: &[Channel]) -> Vec<Channel> {
    stored.iter().rev().filter_map(|entry| {
        let sid = keys.sid_of_machine(&entry.machine_id)?;
        lineup.iter().find(|ch| ch.sid == sid && ch.dvr == entry.dvr && ch.channel == entry.channel).cloned()
    }).take(MAX_RECENT).collect()
}

/// The durable form of a tuned channel: its server's machine identifier from the roster the app
/// captured. `None` for a server the roster does not hold (or one with no machine identifier):
/// there is nothing durable to write for it.
pub fn entry_of(sid: ServerId, dvr: &str, channel: &str, keys: &ClientKeys) -> Option<RecentChannelEntry> {
    let entry = RecentChannelEntry::new(keys.machine_of(sid)?, dvr, channel);
    (!entry.is_blank()).then_some(entry)
}
