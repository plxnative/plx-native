//! **Library order** (Settings, LO-2): a per-profile order of the favourite libraries across servers,
//! applied as a projection over the section table. The rules are `plex::library_order` (graded in
//! `plx_plex`); what is graded here is the plumbing: the deterministic default, the commands, the
//! presentation order the Library header reads, per-profile isolation and the republish.

use super::*;
use plx_plex::plex::session::Session;
use crate::stores::browse::DirectorySnapshot;
use plx_machine::job::Port;

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
        assert_eq!(state.order_state(), OrderState::Server, "and it is the server order");
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

/// A Plex Home managed profile's family server is the household's though plex.tv sends it a handle: it
/// leads the default over a friend's share that has the lower slot, in the section order, the tab
/// fallback and (the same `source_rank`) Home's roster (`pms::library_order_tests`).
#[test]
fn a_managed_profiles_family_server_leads_a_friends_share_in_the_default() {
    let mut state = BrowseState::default();
    state.sources = vec![a_source("nas", "anna", true), a_source("family", "mum", true)];
    state.sources[0].household = false;
    state.sources[1].household = true;
    let session = signed_in();
    state.append_sections_with(0, vec![(2, "Films".into(), SecKind::Movie)], &session, "");
    state.append_sections_with(1, vec![(4, "Movies".into(), SecKind::Movie)], &session, "");
    for section in &mut state.sections { section.pinned = true; }
    assert_eq!(titles(&state, &state.default_section_order()), vec!["Movies", "Films"]);
    assert_eq!(state.section_of_kind(SecKind::Movie), Some(1), "the tab with nothing remembered opens the family's");
    assert!(source_rank(true, true, 5) < source_rank(false, true, 0), "the household before a share, whatever the slots");
    assert!(source_rank(false, false, 3) < source_rank(false, true, 0), "a share with no owner named before one that has");
    assert!(source_rank(true, false, 1) < source_rank(true, true, 0), "and within the household, no owner before one");
}

/// A recorded order ranks across servers (a share between the owner's own libraries); the table
/// itself is not reordered, so every index that names a library stays valid.
#[test]
fn a_set_order_is_a_projection_and_leaves_the_table_alone() {
    let mut state = table(&[0, 1]);
    let before: Vec<_> = state.sections.iter().map(|s| (s.src, s.key)).collect();
    assert!(state.set_library_order_from(ids(&[("home", 1), ("nas", 2), ("home", 4), ("home", 5), ("nas", 9)]), &signed_in()));
    assert_eq!(shown(&state), vec!["Anime", "Films", "Movies", "TV", "Docs"]);
    assert_eq!(state.order_state(), OrderState::Saved { in_effect: true });
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
    assert_eq!(view.library_order_state(), OrderState::Saved { in_effect: true });
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
    assert!(state.order_state().in_effect());
    assert!(state.reset_library_order_from(&signed_in()));
    assert!(state.order_record.is_none());
    assert_eq!(state.order_state(), OrderState::Server);
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
    assert!(state.order_record.is_none() && state.order_state() == OrderState::Server);
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

// ---- review findings: carry-forward, tab landing, reload publication -----------------------------

/// A session whose record already names `ids` for the owner, the way a profile that reordered earlier
/// reads back (the share `nas` has since gone offline in the tests that use it).
fn session_with_record(ids: &[(&str, i64)]) -> Session {
    let mut session = signed_in();
    session.set_order_for("", &self::ids(ids));
    session
}

/// The recorded libraries of the owner's record, as `(machine, key)`.
fn recorded(state: &BrowseState) -> Vec<(String, i64)> {
    state.order_record.as_ref().map(|record| record.libs.iter().map(|lib| (lib.machine_id.clone(), lib.key)).collect()).unwrap_or_default()
}

/// F1: an offline server's recorded places are not lost when the profile reorders what is present.
#[test]
fn an_offline_servers_recorded_place_survives_an_order_write() {
    let mut state = table(&[0]);
    let session = session_with_record(&[("home", 5), ("nas", 2), ("home", 4), ("nas", 9), ("home", 1)]);
    state.load_remembered(&session, "");
    assert!(state.set_library_order_from(ids(&[("home", 1), ("home", 5), ("home", 4)]), &session));
    let kept = recorded(&state);
    assert!(kept.contains(&("nas".to_owned(), 2)) && kept.contains(&("nas".to_owned(), 9)), "nas is offline, not answered about: {kept:?}");
    let at = |lib: (&str, i64)| kept.iter().position(|k| k.0 == lib.0 && k.1 == lib.1).unwrap();
    assert!(at(("home", 1)) < at(("home", 5)) && at(("home", 5)) < at(("home", 4)), "the present arrangement is what was chosen");
    assert!(at(("home", 5)) < at(("nas", 2)) || at(("nas", 2)) < at(("home", 4)), "and nas keeps a place beside its recorded neighbour");
}

/// F1: choosing the default among the PRESENT libraries must not delete the absent server's entries.
#[test]
fn the_default_among_present_libraries_does_not_delete_an_absent_servers_entries() {
    let mut state = table(&[0]);
    let session = session_with_record(&[("home", 5), ("nas", 2), ("home", 4)]);
    state.load_remembered(&session, "");
    assert!(state.set_library_order_from(ids(&[("home", 4), ("home", 1), ("home", 5)]), &session));
    assert!(recorded(&state).iter().any(|lib| lib.0 == "nas"), "the record still names nas: {:?}", recorded(&state));
}

/// F6: with nothing visited or remembered a tab lands on the first-ordered favourite of its kind.
#[test]
fn a_tab_lands_on_the_first_ordered_library_of_its_kind() {
    let mut state = table(&[0]);
    assert_eq!(state.section_of_kind(SecKind::Show), Some(1), "no record: Anime (the directory's first show library)");
    state.set_library_order_from(ids(&[("home", 5), ("home", 4), ("home", 1)]), &signed_in());
    assert_eq!(state.section_of_kind(SecKind::Show), Some(2), "TV is the profile's first show library");
}

/// F6: a library that stops being a favourite re-points the page at the first-ordered favourite.
#[test]
fn repointing_follows_the_order() {
    let mut state = table(&[0]);
    state.set_library_order_from(ids(&[("home", 5), ("home", 4), ("home", 1)]), &signed_in());
    state.set_cur(0);
    state.sections[0].pinned = false;
    state.repoint_cur(false);
    assert_eq!(state.cur(), 2, "the first-ordered show favourite, not the first row of the table");
}

/// F10: a record that arrives through a reload (a profile switch, a session recovery) republishes.
#[test]
fn a_reload_that_changes_the_record_bumps_the_publication() {
    let mut state = table(&[0, 1]);
    let before = state.source_list_gen();
    state.load_remembered(&session_with_record(&[("nas", 9), ("home", 4)]), "");
    assert_ne!(state.source_list_gen(), before, "the presentation order moved with no command");
    let settled = state.source_list_gen();
    state.load_remembered(&session_with_record(&[("nas", 9), ("home", 4)]), "");
    assert_eq!(state.source_list_gen(), settled, "the same record again moves nothing");
}

/// F4/F8: one state for every surface. With the friend offline a record that only ranks the friend's
/// libraries is SAVED (Reset has something to delete) yet moves nothing, and Home sees no order.
#[test]
fn a_record_that_moves_nothing_present_is_saved_and_not_in_effect() {
    let mut state = table(&[0]);
    assert_eq!(state.order_state(), OrderState::Server);
    state.load_remembered(&session_with_record(&[("nas", 9), ("nas", 2)]), "");
    assert_eq!(state.order_state(), OrderState::Saved { in_effect: false });
    assert!(matches!(state.home_order(), crate::pms::HomeOrder::Natural), "Home reads the same state");
    let mut snapshot = DirectorySnapshot::default();
    snapshot.capture_from(&mut state, &plx_plex::plex::ClientKeys::default());
    assert!(snapshot.view().library_order_state().is_saved(), "Reset is offered: a record exists");
    // and the record is one entry kind of state on both sides of an empty machine id
    let mut anonymous = table(&[0]);
    anonymous.sources[0].machine_id = String::new();
    anonymous.load_remembered(&session_with_record(&[("home", 5), ("home", 4)]), "");
    assert_eq!(anonymous.order_state(), OrderState::Saved { in_effect: false }, "no library without an id can be named");
}

/// F9: the write's outcome is published, so Settings can tell saved from failed.
#[test]
fn the_order_write_lifecycle_is_published() {
    let mut state = table(&[0, 1]);
    assert_eq!(state.order_write(), WriteState::Idle);
    let moved = ids(&[("nas", 9), ("home", 4), ("home", 1), ("home", 5), ("nas", 2)]);
    assert!(state.set_library_order_from(moved.clone(), &signed_in()));
    assert_eq!(state.order_write(), WriteState::InFlight);
    let crate::stores::StoreJob::BrowsePreferences(job, port) = state.take_preference_jobs().remove(0) else { panic!("preference job") };
    port.refused(job.addr);
    state.observe_pin_write();
    assert_eq!(state.order_write(), WriteState::Failed);
    assert!(state.order_record.is_some(), "the order still shows for this run");
    let mut snapshot = DirectorySnapshot::default();
    snapshot.capture_from(&mut state, &plx_plex::plex::ClientKeys::default());
    assert_eq!(snapshot.view().library_order_write(), WriteState::Failed, "published on the directory");
    // the next attempt supersedes it
    assert!(state.set_library_order_from(moved, &signed_in()));
    assert_eq!(state.order_write(), WriteState::InFlight);
    let crate::stores::StoreJob::BrowsePreferences(job, port) = state.take_preference_jobs().remove(0) else { panic!("preference job") };
    port.done(job.addr, crate::browse::preferences_job::WriteOutcome::Finished(true));
    state.observe_pin_write();
    assert_eq!(state.order_write(), WriteState::Saved);
    // another profile never reads this profile's outcome
    state.profile_key = "kid".into();
    assert_eq!(state.order_write(), WriteState::Idle);
}

/// F11: a command naming no library is refused; Reset is the one explicit "no record".
#[test]
#[should_panic(expected = "naming no library")]
fn an_empty_order_is_refused_and_reset_stays_the_explicit_command() {
    let mut state = table(&[0, 1]);
    state.set_library_order_from(ids(&[("nas", 9), ("home", 4), ("home", 1), ("home", 5), ("nas", 2)]), &signed_in());
    assert!(state.reset_library_order_from(&signed_in()), "Reset deletes");
    state.set_library_order_from(Vec::new(), &signed_in());
}

/// A share plex.tv never named has no handle but is not the household's either (#68): the default
/// order, and so a tab with nothing remembered, puts the household's own server first.
#[test]
fn a_share_with_no_handle_that_is_not_the_households_sorts_behind_the_household() {
    let mut state = BrowseState::default();
    state.sources = vec![a_source("odd", "", true), a_source("home", "", true)];
    state.sources[0].household = false;
    state.sources[1].household = true;
    let session = signed_in();
    state.append_sections_with(0, vec![(2, "Films".into(), SecKind::Movie)], &session, "");
    state.append_sections_with(1, vec![(4, "Movies".into(), SecKind::Movie)], &session, "");
    for section in &mut state.sections { section.pinned = true; }
    assert_eq!(titles(&state, &state.default_section_order()), vec!["Movies", "Films"]);
    assert_eq!(state.section_of_kind(SecKind::Movie), Some(1), "the household's library, with nothing remembered");
}
