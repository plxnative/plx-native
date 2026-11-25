//! **Library order** (Settings, LO-2): a per-profile order of the favourite libraries across servers,
//! applied as a projection over the section table. The rules are `plex::library_order` (graded in
//! `plx_plex`); what is graded here is the plumbing: the deterministic default, the commands, the
//! presentation order the Library header reads, per-profile isolation and the republish.

use super::*;
use plx_plex::plex::session::Session;
use crate::stores::browse::DirectorySnapshot;

fn signed_in() -> Session {
    Session { client_id: "order-client".into(), account_token: "order-account".into(), ..Default::default() }
}

/// Own server `home` (libraries 4, 1, 5 in its own directory order: movies, anime, shows) and a
/// friend's `nas` (2, 9), every library a favourite. Sources land in `landing` order.
fn table(landing: &[usize]) -> BrowseState {
    let mut state = BrowseState::default();
    state.sources = vec![a_source("home", "", true), a_source("nas", "anna", true)];
    let session = signed_in();
    state.preference_writes.prepare(&session, 0);
    for &source in landing {
        let list = if source == 0 {
            vec![(4, "Movies".into(), SecKind::Movie), (1, "Anime".into(), SecKind::Show), (5, "TV".into(), SecKind::Show)]
        } else {
            vec![(2, "Films".into(), SecKind::Movie), (9, "Docs".into(), SecKind::Show)]
        };
        state.append_sections_with(source, list, &session, "");
    }
    for section in &mut state.sections { section.pinned = true; }
    state
}

fn titles(state: &BrowseState, order: &[usize]) -> Vec<String> {
    order.iter().map(|&index| state.sections[index].title.clone()).collect()
}

fn shown(state: &BrowseState) -> Vec<String> {
    state.library_order_rows().into_iter().map(|row| row.title).collect()
}

fn ids(list: &[(&str, i64)]) -> Vec<(String, i64)> {
    list.iter().map(|(machine, key)| ((*machine).to_owned(), *key)).collect()
}

/// The default order is the roster's, whichever source's directory landed first.
#[test]
fn the_default_order_does_not_depend_on_landing_order() {
    let want = vec!["Movies", "Anime", "TV", "Films", "Docs"];
    for landing in [[0, 1], [1, 0]] {
        let state = table(&landing);
        assert_eq!(titles(&state, &state.default_section_order()), want, "landing {landing:?}");
        assert_eq!(titles(&state, &state.section_order()), want, "no record: the default is what is shown");
        assert!(state.favorite_order().1, "and it is the server order");
    }
}

/// Own servers come before shares even when the share has the lower roster index.
#[test]
fn own_servers_lead_the_default_even_when_a_share_comes_first_in_the_roster() {
    let mut state = BrowseState::default();
    state.sources = vec![a_source("nas", "anna", true), a_source("home", "", true)];
    let session = signed_in();
    state.append_sections_with(0, vec![(2, "Films".into(), SecKind::Movie)], &session, "");
    state.append_sections_with(1, vec![(4, "Movies".into(), SecKind::Movie)], &session, "");
    assert_eq!(titles(&state, &state.default_section_order()), vec!["Movies", "Films"]);
}

/// A recorded order ranks across servers (a share between the owner's own libraries); the table
/// itself is not reordered, so every index that names a library stays valid.
#[test]
fn a_set_order_is_a_projection_and_leaves_the_table_alone() {
    let mut state = table(&[0, 1]);
    let before: Vec<_> = state.sections.iter().map(|s| (s.src, s.key)).collect();
    assert!(state.set_library_order_from(ids(&[("home", 1), ("nas", 2), ("home", 4), ("home", 5), ("nas", 9)]), &signed_in()));
    assert_eq!(shown(&state), vec!["Anime", "Films", "Movies", "TV", "Docs"]);
    assert!(!state.favorite_order().1);
    let after: Vec<_> = state.sections.iter().map(|s| (s.src, s.key)).collect();
    assert_eq!(before, after, "table indices are identities and never move");
    let rows = state.library_order_rows();
    assert_eq!((rows[1].machine_id.as_str(), rows[1].key, rows[1].handle.as_str()), ("nas", 2, "anna"), "the owner handle rides along");
}

/// The Library header reads the same order: `rows_for` walks the presentation order, per kind.
#[test]
fn the_library_header_follows_the_order() {
    let mut state = table(&[0, 1]);
    state.set_library_order_from(ids(&[("home", 5), ("nas", 9), ("home", 1), ("nas", 2), ("home", 4)]), &signed_in());
    let mut snapshot = DirectorySnapshot::default();
    snapshot.capture_from(&mut state, &plx_plex::plex::ClientKeys::default());
    let view = snapshot.view();
    let shows: Vec<String> = view.rows_for(1).map(|row| row.title.clone()).collect();
    assert_eq!(shows, vec!["TV", "Docs", "Anime"], "the shows, in the recorded order");
    let films: Vec<String> = view.rows_for(0).map(|row| row.title.clone()).collect();
    assert_eq!(films, vec!["Films", "Movies"]);
    let (indices, _): (Vec<usize>, Vec<_>) = view.favorite_sections_for(1).unzip();
    assert_eq!(indices, vec![2, 4, 1], "favorite_sections_for carries the same order, with table indices");
    assert!(!view.library_order_is_server());
    assert_eq!(view.library_order().len(), 5);
}

/// A library the record does not name falls after the one before it in the default order.
#[test]
fn a_library_that_arrives_after_the_order_falls_at_its_default_place() {
    let mut state = table(&[0]);
    state.set_library_order_from(ids(&[("home", 5), ("home", 4), ("home", 1)]), &signed_in());
    assert_eq!(shown(&state), vec!["TV", "Movies", "Anime"]);
    state.append_sections_with(1, vec![(2, "Films".into(), SecKind::Movie), (9, "Docs".into(), SecKind::Show)], &signed_in(), "");
    for section in &mut state.sections { section.pinned = true; }
    // the share's libraries follow `TV`, their predecessor in the default order, and not the end of
    // the list: Films right after TV, Docs right after Films
    assert_eq!(shown(&state), vec!["TV", "Films", "Docs", "Movies", "Anime"]);
}

/// Saving the server order records nothing: the record is deleted instead.
#[test]
fn an_order_equal_to_the_server_order_is_no_record() {
    let mut state = table(&[0, 1]);
    state.set_library_order_from(ids(&[("nas", 2), ("home", 4)]), &signed_in());
    assert!(state.order_record.is_some());
    assert!(state.set_library_order_from(ids(&[("home", 4), ("home", 1), ("home", 5), ("nas", 2), ("nas", 9)]), &signed_in()));
    assert!(state.order_record.is_none(), "the favourites as shown are the server order: the record goes");
    assert_eq!(shown(&state), vec!["Movies", "Anime", "TV", "Films", "Docs"]);
}

/// Reset deletes the profile's record and restores the server order.
#[test]
fn reset_restores_the_server_order() {
    let mut state = table(&[0, 1]);
    state.set_library_order_from(ids(&[("nas", 9), ("nas", 2)]), &signed_in());
    assert!(!state.favorite_order().1);
    assert!(state.reset_library_order_from(&signed_in()));
    assert!(state.order_record.is_none());
    assert!(state.favorite_order().1);
}

/// A profile's order is its own: another profile (and a switch back) reads what each recorded.
#[test]
fn the_order_is_per_profile() {
    let mut state = table(&[0, 1]);
    state.profile_key = "kid".into();
    assert!(state.set_library_order_from(ids(&[("nas", 9), ("home", 4), ("home", 1), ("home", 5), ("nas", 2)]), &signed_in()));
    assert!(state.order_record.is_some(), "the overlay holds the pending write for this profile");
    assert_eq!(shown(&state)[0], "Docs");
    // the owner (key "") has recorded nothing: the same table reads in the server order for them
    state.profile_key = String::new();
    state.load_remembered(&signed_in(), "");
    assert_eq!(shown(&state), vec!["Movies", "Anime", "TV", "Films", "Docs"]);
    state.load_remembered(&signed_in(), "kid");
    assert_eq!(shown(&state)[0], "Docs", "and the profile's own record is still there");
}

/// Without an identity to write under, the command is refused and nothing changes.
#[test]
fn an_order_command_without_a_session_identity_is_refused() {
    let mut state = table(&[0, 1]);
    assert!(!state.set_library_order_from(ids(&[("nas", 9), ("home", 4), ("home", 1), ("home", 5), ("nas", 2)]), &Session::default()));
    assert!(!state.reset_library_order_from(&Session::default()));
    assert!(state.order_record.is_none() && state.favorite_order().1);
}

/// The commands republish the table, so a captured directory moves with them.
#[test]
fn the_commands_republish_the_directory() {
    let mut state = table(&[0, 1]);
    let before = state.source_list_gen();
    state.set_library_order_from(ids(&[("nas", 9), ("home", 4), ("home", 1), ("home", 5), ("nas", 2)]), &signed_in());
    assert_ne!(state.source_list_gen(), before);
}

/// The sessionless entry refuses the order commands loudly, like the pin commit.
#[test]
#[should_panic(expected = "handed no session")]
fn an_order_command_sent_through_the_sessionless_entry_is_refused() {
    let mut store = crate::stores::browse::BrowseStore::default();
    store.run(crate::stores::browse::BrowseCmd::SetLibraryOrder(Vec::new()), &plx_plex::plex::ClientKeys::default());
}
