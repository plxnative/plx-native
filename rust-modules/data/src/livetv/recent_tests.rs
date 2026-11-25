//! Recent channels in the Live TV store: the SPEC-1 rule (join at the front, a known channel never
//! moves, five at most and the last drops), the read-time skip of a channel the lineup does not
//! list, and the durable-key edge the app converts through.
use super::recent::{entry_of, keys_of, RecentSync};
use super::*;
use crate::stores::Stores;
use plx_plex::plex::session::prefs::RecentChannelEntry;
use plx_plex::plex::ClientFacts;

fn sid(n: u16) -> ServerId { ServerId::from_raw(n) }

fn channel(server: u16, number: &str) -> Channel {
    Channel { sid: sid(server), dvr: "7".into(), channel: format!("C{number}"), vcn: number.into(),
        name: format!("Channel {number}"), thumb: String::new(), now: String::new(), now_thumb: String::new(),
        now_summary: String::new(), begins_at: 0, ends_at: 0 }
}
fn key(server: u16, number: &str) -> RecentKey {
    RecentKey { sid: sid(server), dvr: "7".into(), channel: format!("C{number}") }
}
fn lineup(server: u16, numbers: &[&str]) -> Vec<Channel> { numbers.iter().map(|n| channel(server, n)).collect() }

/// A registry as the Bridge captures it: slot `n` holds the server whose machine id is `machines[n]`.
fn roster(roster_gen: u32, machines: &[(u16, &str)]) -> ClientKeys {
    ClientKeys::captured(ServerId::UNSET, roster_gen, 0, machines.iter().map(|(n, machine)| {
        (sid(*n), Some(ClientKey { instance: u32::from(*n) + 1, token: 1, grant: 1 }),
            ClientFacts { machine_id: (*machine).into(), described: None, source: None, stream_allowed: false })
    }))
}

fn note(stores: &mut Stores, key: RecentKey) -> bool {
    stores.livetv_run(LiveTvCmd::Noted(key), &ClientKeys::default(), 0)
}
fn row(stores: &Stores) -> Vec<String> {
    stores.livetv_view().recent.iter().map(|c| c.vcn.clone()).collect()
}
fn with_lineup(numbers: &[&str]) -> Stores {
    let mut stores = Stores::default();
    stores.livetv.install_for_test(lineup(0, numbers));
    stores
}

#[test]
fn an_empty_recent_is_published_as_an_empty_list() {
    let stores = with_lineup(&["1", "2", "3"]);
    assert!(stores.livetv_view().recent.is_empty(), "PR3 hides the row on an empty list");
    assert!(LiveTvView::LOADING.recent.is_empty());
}

#[test]
fn a_newly_tuned_channel_joins_at_the_front() {
    let mut stores = with_lineup(&["1", "2", "3", "4"]);
    for n in ["1", "2", "3"] { assert!(note(&mut stores, key(0, n))); }
    assert_eq!(row(&stores), ["3", "2", "1"]);
}

#[test]
fn a_channel_already_there_never_moves_and_changes_nothing() {
    let mut stores = with_lineup(&["1", "2", "3", "4"]);
    for n in ["1", "2", "3"] { note(&mut stores, key(0, n)); }
    let revision = stores.livetv_view().revision();
    let _ = stores.livetv.take_fx();
    for n in ["1", "3", "2"] { assert!(!note(&mut stores, key(0, n)), "re-tuning {n} is no change"); }
    assert_eq!(row(&stores), ["3", "2", "1"]);
    assert_eq!(stores.livetv_view().revision(), revision, "no publication for a no-op");
    assert!(stores.livetv.take_fx().is_empty(), "and nothing to persist");
}

#[test]
fn a_sixth_channel_drops_the_oldest_which_is_the_last() {
    let mut stores = with_lineup(&["1", "2", "3", "4", "5", "6", "7"]);
    for n in ["1", "2", "3", "4", "5"] { note(&mut stores, key(0, n)); }
    assert_eq!(row(&stores), ["5", "4", "3", "2", "1"]);
    assert!(note(&mut stores, key(0, "6")));
    assert_eq!(row(&stores), ["6", "5", "4", "3", "2"], "1, the last, went");
    assert_eq!(MAX_RECENT, 5);
}

#[test]
fn a_new_channel_is_one_effect_to_persist_and_a_known_one_is_none() {
    let mut stores = with_lineup(&["1", "2"]);
    note(&mut stores, key(0, "1"));
    note(&mut stores, key(0, "2"));
    note(&mut stores, key(0, "1"));
    assert_eq!(stores.livetv.take_fx(), [LiveTvFx::RememberRecent(key(0, "1")), LiveTvFx::RememberRecent(key(0, "2"))]);
    assert!(stores.livetv.take_fx().is_empty(), "taken once");
}

/// A channel the lineup does not list is skipped when the row is read, and kept: it comes back
/// when the lineup lists it again (a server that was offline), and it still holds its place.
#[test]
fn an_unknown_channel_is_skipped_at_read_but_kept() {
    let mut stores = with_lineup(&["1", "2"]);
    note(&mut stores, key(0, "1"));
    note(&mut stores, key(0, "9"));
    note(&mut stores, key(0, "2"));
    assert_eq!(row(&stores), ["2", "1"], "9 is not in the lineup");
    stores.livetv.install_for_test(lineup(0, &["1", "2", "9"]));
    assert_eq!(row(&stores), ["2", "9", "1"], "kept, in its place, once the lineup lists it");
}

#[test]
fn a_restore_replaces_the_row_and_keeps_the_newest_five() {
    let mut stores = with_lineup(&["1", "2", "3", "4", "5", "6", "7"]);
    note(&mut stores, key(0, "1"));
    let _ = stores.livetv.take_fx();
    let restored = ["7", "6", "5", "4", "3", "2"].map(|n| key(0, n)).to_vec();
    assert!(stores.livetv_run(LiveTvCmd::Restore(restored), &ClientKeys::default(), 0));
    assert_eq!(row(&stores), ["7", "6", "5", "4", "3"]);
    assert!(stores.livetv.take_fx().is_empty(), "a restore is not a write");
    assert!(stores.livetv_run(LiveTvCmd::Restore(Vec::new()), &ClientKeys::default(), 0));
    assert!(row(&stores).is_empty(), "a profile with no history reads an empty row");
}

/// A server re-registration (`Reset`) is not a profile switch: Recent survives it.
#[test]
fn a_reset_keeps_recent() {
    let mut stores = Stores::default();
    note(&mut stores, key(0, "1"));
    stores.livetv.install_for_test(lineup(0, &["1"]));
    assert_eq!(row(&stores), ["1"]);
    stores.livetv_run(LiveTvCmd::Reset, &ClientKeys::default(), 0);
    stores.livetv.install_for_test(lineup(0, &["1"]));
    assert_eq!(row(&stores), ["1"]);
}

/// **The stored list survives a different sid assignment.** What is stored names the server by its
/// machine identifier; restoring under a registry that numbered the servers the other way round
/// still names the same channels.
#[test]
fn the_stored_list_survives_a_different_sid_assignment() {
    let stored = vec![RecentChannelEntry::new("machine-a", "7", "C1"), RecentChannelEntry::new("machine-b", "7", "C2")];
    for (a, b) in [(0u16, 1u16), (1, 0), (4, 2)] {
        let keys = roster(1, &[(a, "machine-a"), (b, "machine-b")]);
        let restored = keys_of(&stored, &keys);
        assert_eq!(restored, [key(b, "2"), key(a, "1")], "newest first: the list is stored oldest first");
        for k in &restored {
            let back = entry_of(k, &keys).expect("a registered server");
            assert!(stored.contains(&back), "{back:?} is what was stored");
        }
    }
}

/// An entry whose server is not registered is left out of the restored row, never rewritten, and
/// it has no durable form to write while the server is away.
#[test]
fn an_unregistered_server_is_skipped_at_restore_and_has_no_entry_to_write() {
    let stored = vec![RecentChannelEntry::new("machine-a", "7", "C1"), RecentChannelEntry::new("machine-gone", "7", "C2")];
    let keys = roster(1, &[(0, "machine-a")]);
    assert_eq!(keys_of(&stored, &keys), [key(0, "1")]);
    assert_eq!(entry_of(&key(3, "2"), &keys), None);
    assert_eq!(entry_of(&key(0, "1"), &roster(1, &[(0, "")])), None, "a slot with no machine id is no server");
}

/// **Restored before the server registered, visible once it does.** The seat arrives before the
/// servers register, so the first restore finds nothing; the roster moving asks for another.
#[test]
fn a_row_restored_before_its_server_registered_shows_once_it_does() {
    let stored = vec![RecentChannelEntry::new("machine-a", "7", "C1")];
    let mut sync = RecentSync::default();
    let mut stores = with_lineup(&["1", "2"]);

    let before = roster(1, &[]);
    assert!(sync.due(7, &before), "the first look restores");
    assert!(!stores.livetv_run(LiveTvCmd::Restore(keys_of(&stored, &before)), &ClientKeys::default(), 0),
        "an empty restore of an empty row");
    assert!(row(&stores).is_empty());
    assert!(!sync.due(7, &before), "nothing moved: no second restore");

    let after = roster(2, &[(0, "machine-a")]);
    assert!(sync.due(7, &after), "the roster moved");
    // The store's own pump would start reading the new roster's lineup; this test is about the row.
    stores.livetv_run(LiveTvCmd::Restore(keys_of(&stored, &after)), &ClientKeys::default(), 0);
    assert_eq!(row(&stores), ["1"]);
    assert!(!sync.due(7, &after));
    assert!(sync.due(8, &after), "a profile switch restores again");
}
