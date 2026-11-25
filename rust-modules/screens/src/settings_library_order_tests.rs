//! Settings > Libraries > Library order: the lifecycle ([`Mode`]) and the sentence each state says,
//! the one write a drop sends, Back and the keys that must not leak while a library is held, Reset
//! (inert without a record, confirmed by a second OK), the header preview's overflow, the root's
//! value, and the fit of every string in every shipped language.
use super::*;
use super::super::test_support::*;
use plx_data::browse::{OrderState, SecKind, WriteState};
use plx_data::stores::browse::DirectorySnapshot;
use plx_machine::present::Present;
use plx_os::i18n::{language_on_this_thread_for_test, SHIPPED};

const RECORDED: OrderState = OrderState::Recorded { in_effect: true };

fn row(section: usize, title: &str, kind: SecKind, handle: &str) -> LibraryOrderRow {
    LibraryOrderRow { section, machine_id: format!("m{}", if handle.is_empty() { 0 } else { 1 }), key: section as i64 + 1, title: title.into(), kind, handle: handle.into() }
}

/// Six favourites across the two kinds, the server's order: Movies, TV Shows, 4K Movies, Kids, Anime (a
/// friend's), Documentaries.
fn six() -> Vec<LibraryOrderRow> {
    vec![
        row(0, "Movies", SecKind::Movie, ""), row(1, "TV Shows", SecKind::Show, ""), row(2, "4K Movies", SecKind::Movie, ""),
        row(3, "Kids", SecKind::Show, ""), row(4, "Anime", SecKind::Show, "anna"), row(5, "Documentaries", SecKind::Show, ""),
    ]
}

fn snap(rows: Vec<LibraryOrderRow>, state: OrderState, write: WriteState) -> DirectorySnapshot {
    DirectorySnapshot::default().with_library_order(rows, state, write)
}

/// What a step sent to the Browse store.
#[derive(Debug, PartialEq)]
enum Sent {
    Set(Vec<(String, i64)>),
    Reset,
}

fn sent(out: &[Stamped<InnerHost>]) -> Vec<Sent> {
    out.iter().filter_map(|e| match &e.fx {
        Fx::App(AppFx::Store(StoreId::Browse, StoreCmd::Browse(BrowseCmd::SetLibraryOrder(libs)))) => Some(Sent::Set(libs.clone())),
        Fx::App(AppFx::Store(StoreId::Browse, StoreCmd::Browse(BrowseCmd::ResetLibraryOrder))) => Some(Sent::Reset),
        _ => None,
    }).collect()
}

fn step_page(page: &mut LibraryOrderPage, snap: &DirectorySnapshot, ev: ScreenEvent<InnerHost>) -> (Handled, Vec<Stamped<InnerHost>>) {
    let mut out = Vec::new();
    let mut present = Present::new();
    let mut fx = Effects::new(&mut out, MachineId::Instance(InstanceId(0)), &mut present);
    let handled = page.step(&ev, &cx_with(None, snap.view()), &mut fx);
    (handled, out)
}

fn key_ev(key: Key) -> ScreenEvent<InnerHost> {
    ScreenEvent::Input(InputEvent {
        at: plx_machine::machine::Tick::default(),
        source: plx_machine::machine::Source::Sdl,
        kind: InputKind::Key { key, sym: 0, wcode: 0, edge: Edge::Down, at_edge: false },
    })
}

fn press(page: &mut LibraryOrderPage, snap: &DirectorySnapshot, key: Key) -> (Handled, Vec<Stamped<InnerHost>>) {
    step_page(page, snap, key_ev(key))
}

/// OK on a row: the activation a press commit delivers.
fn ok_on(page: &mut LibraryOrderPage, snap: &DirectorySnapshot, elem: u32) -> Vec<Stamped<InnerHost>> {
    step_page(page, snap, ScreenEvent::Activate(elem)).1
}

fn focus_on(page: &mut LibraryOrderPage, snap: &DirectorySnapshot, elem: u32) {
    let to = FocusKey { entry: page.entry, elem };
    step_page(page, snap, ScreenEvent::FocusMoved { from: None, to, by: plx_ui::screen::By::Dir });
}

fn order(page: &LibraryOrderPage) -> Vec<usize> {
    page.state.shown.iter().map(|r| r.section).collect()
}

fn page_of(snap: &DirectorySnapshot) -> LibraryOrderPage {
    LibraryOrderPage::new(EntryId(0), snap.view())
}

fn pick_up(page: &mut LibraryOrderPage, snap: &DirectorySnapshot, section: usize) {
    focus_on(page, snap, lib_elem_for_test(section));
    assert!(sent(&ok_on(page, snap, lib_elem_for_test(section))).is_empty(), "picking up writes nothing");
}

// ---- the lifecycle ----------------------------------------------------------------------------

#[test]
fn pickup_move_and_drop_sends_exactly_one_set_with_the_whole_shown_order() {
    let s = snap(six(), OrderState::Server, WriteState::Idle);
    let mut page = page_of(&s);
    pick_up(&mut page, &s, 4);
    assert!(matches!(page.state.mode, Mode::Moving { lifted: 4, .. }));
    for _ in 0..3 {
        let (handled, out) = press(&mut page, &s, Key::Up);
        assert_eq!(handled, Handled::Yes, "the page takes Up while a library is held");
        assert!(sent(&out).is_empty(), "moving writes nothing");
    }
    press(&mut page, &s, Key::Down);
    assert_eq!(order(&page), [0, 1, 4, 2, 3, 5], "Anime went up three and down one");
    let out = ok_on(&mut page, &s, lib_elem_for_test(4));
    assert_eq!(sent(&out), [Sent::Set(vec![
        ("m0".into(), 1), ("m0".into(), 2), ("m1".into(), 5), ("m0".into(), 3), ("m0".into(), 4), ("m0".into(), 6),
    ])], "one command, the whole shown order, by (machine, section key)");
    assert_eq!(page.state.mode, Mode::Dropped(4));
}

#[test]
fn a_drop_that_moved_nothing_writes_nothing() {
    let s = snap(six(), OrderState::Server, WriteState::Idle);
    let mut page = page_of(&s);
    pick_up(&mut page, &s, 2);
    press(&mut page, &s, Key::Up);
    press(&mut page, &s, Key::Down);
    assert!(sent(&ok_on(&mut page, &s, lib_elem_for_test(2))).is_empty());
    assert_eq!(page.state.mode, Mode::Dropped(2));
}

#[test]
fn a_library_whose_server_has_no_machine_id_yet_is_left_out_of_the_command() {
    let mut rows = six();
    rows[1].machine_id.clear();
    let s = snap(rows, OrderState::Server, WriteState::Idle);
    let mut page = page_of(&s);
    pick_up(&mut page, &s, 3);
    press(&mut page, &s, Key::Up);
    let Some(Sent::Set(libs)) = sent(&ok_on(&mut page, &s, lib_elem_for_test(3))).pop() else { panic!("a set") };
    assert_eq!(libs.len(), 5);
    assert!(libs.iter().all(|(machine, _)| !machine.is_empty()), "never an empty machine id: {libs:?}");
}

#[test]
fn the_ends_are_walls_for_the_held_library() {
    let s = snap(six(), OrderState::Server, WriteState::Idle);
    let mut page = page_of(&s);
    pick_up(&mut page, &s, 0);
    press(&mut page, &s, Key::Up);
    assert_eq!(order(&page), [0, 1, 2, 3, 4, 5]);
    ok_on(&mut page, &s, lib_elem_for_test(0));
    pick_up(&mut page, &s, 5);
    press(&mut page, &s, Key::Down);
    assert_eq!(order(&page), [0, 1, 2, 3, 4, 5]);
}

#[test]
fn back_while_moving_restores_sends_nothing_and_leaves_the_page_where_it_is() {
    let s = snap(six(), OrderState::Server, WriteState::Idle);
    let mut page = page_of(&s);
    pick_up(&mut page, &s, 3);
    press(&mut page, &s, Key::Up);
    press(&mut page, &s, Key::Up);
    assert_ne!(order(&page), [0, 1, 2, 3, 4, 5]);
    let (handled, out) = press(&mut page, &s, Key::Back);
    assert_eq!(handled, Handled::Yes, "Back is the page's while a library is held");
    assert!(sent(&out).is_empty() && !out.iter().any(|e| matches!(e.fx, Fx::Nav(_))), "nothing written, nothing popped");
    assert_eq!(order(&page), [0, 1, 2, 3, 4, 5], "the library is back where it was picked up");
    assert_eq!(page.state.mode, Mode::Idle);
    assert_eq!(press(&mut page, &s, Key::Back).0, Handled::No, "the next Back is the surface's");
}

#[test]
fn left_and_right_are_swallowed_while_a_library_is_held_and_the_table_says_so() {
    let s = snap(six(), OrderState::Server, WriteState::Idle);
    let mut page = page_of(&s);
    assert!(!page.view().table.uncommitted);
    assert_eq!(press(&mut page, &s, Key::Left).0, Handled::No);
    pick_up(&mut page, &s, 1);
    assert!(page.view().table.uncommitted, "Left is a Stop at the table's edge, not a Back");
    for key in [Key::Left, Key::Right] {
        assert_eq!(press(&mut page, &s, key).0, Handled::Yes);
    }
    assert_eq!(order(&page), [0, 1, 2, 3, 4, 5]);
}

#[test]
fn the_back_key_through_the_surface_restores_while_moving_and_pops_when_idle() {
    let _g = plx_base::testlock::serial();
    let _sess = scratch_session("library-order-back");
    let s = snap(six(), OrderState::Server, WriteState::Idle);
    let mut surface = RouteSurface::new(EntryId(7), InstanceId(0), Family::Settings, SettingsPage::Root);
    let drive = |surface: &mut RouteSurface, ev: ScreenEvent<InnerHost>| {
        let mut out = Vec::new();
        let mut present = Present::new();
        let mut fx = Effects::new(&mut out, MachineId::Instance(InstanceId(0)), &mut present);
        let handled = <RouteSurface as Machine<InnerHost>>::step(surface, &ev, &cx_with(None, s.view()), &mut fx);
        (handled, out)
    };
    drive(&mut surface, ScreenEvent::Mount);
    let mut out = Vec::new();
    let mut present = Present::new();
    let mut sink = Effects::new(&mut out, MachineId::Instance(InstanceId(0)), &mut present);
    surface.forward(vec![Stamped { from: MachineId::Instance(InstanceId(1)), fx: Fx::Nav(NavOp::Push(SettingsPage::LibraryOrder)) }], &cx_with(None, s.view()), &mut sink);
    assert_eq!(surface.inner.depth(), 2, "the page is on the root");
    assert_eq!(name(&surface), crate::registry::word::LIBRARY_ORDER);
    drive(&mut surface, ScreenEvent::Mount);
    let elem = lib_elem_for_test(0);
    drive(&mut surface, ScreenEvent::Activate(elem));
    drive(&mut surface, key_ev(Key::Down));
    let (handled, out) = drive(&mut surface, back_key());
    assert_eq!(handled, Handled::Yes);
    assert_eq!(surface.inner.depth(), 2, "Back while a library is held does not pop");
    assert!(!out.iter().any(|e| matches!(e.fx, Fx::Nav(_))) && sent(&out).is_empty());
    drive(&mut surface, back_key());
    assert_eq!(surface.inner.depth(), 1, "and the next Back pops as usual");
}

// ---- Reset ------------------------------------------------------------------------------------

fn reset_stops(page: &LibraryOrderPage) -> bool {
    page.form.step_key(lib_key(5), 1) == Some(LibId::Reset.key())
}

fn lib_key(section: usize) -> RowKey {
    LibId::Lib(section).key()
}

#[test]
fn focus_skips_reset_when_nothing_is_recorded_and_stops_on_it_when_something_is() {
    let none = snap(six(), OrderState::Server, WriteState::Idle);
    assert!(!reset_stops(&page_of(&none)), "no record: Reset is inert");
    let recorded = snap(six(), RECORDED, WriteState::Saved);
    assert!(reset_stops(&page_of(&recorded)));
}

#[test]
fn reset_stays_offered_after_a_failed_write_and_a_recorded_order_that_moves_nothing() {
    let failed = snap(six(), OrderState::Recorded { in_effect: true }, WriteState::Failed);
    assert!(reset_stops(&page_of(&failed)), "a failed write leaves a record standing");
    let idle = snap(six(), OrderState::Recorded { in_effect: false }, WriteState::Saved);
    assert!(reset_stops(&page_of(&idle)), "a record that moves nothing now still has something to delete");
}

#[test]
fn ok_then_ok_resets_and_any_other_key_keeps_the_order() {
    let s = snap(six(), RECORDED, WriteState::Saved);
    // a second OK resets
    let mut page = page_of(&s);
    focus_on(&mut page, &s, reset_elem_for_test());
    assert!(sent(&ok_on(&mut page, &s, reset_elem_for_test())).is_empty(), "the first OK only asks");
    assert_eq!(page.state.mode, Mode::ConfirmReset);
    assert_eq!(press(&mut page, &s, Key::Ok).0, Handled::No, "the second OK reaches the press");
    assert_eq!(sent(&ok_on(&mut page, &s, reset_elem_for_test())), [Sent::Reset]);
    assert_eq!(page.state.mode, Mode::ResetDone);
    // every other key cancels
    for key in [Key::Up, Key::Down, Key::Left, Key::Right, Key::Back] {
        let mut page = page_of(&s);
        focus_on(&mut page, &s, reset_elem_for_test());
        ok_on(&mut page, &s, reset_elem_for_test());
        let (handled, out) = press(&mut page, &s, key);
        assert_eq!(handled, Handled::Yes, "{key:?} is swallowed, not acted on");
        assert!(sent(&out).is_empty());
        assert_eq!(page.state.mode, Mode::Idle, "{key:?} keeps the order");
    }
}

#[test]
fn after_a_reset_the_publication_takes_reset_away_and_focus_lands_on_the_last_library() {
    let recorded = snap(six(), RECORDED, WriteState::Saved);
    let mut page = page_of(&recorded);
    focus_on(&mut page, &recorded, reset_elem_for_test());
    ok_on(&mut page, &recorded, reset_elem_for_test());
    ok_on(&mut page, &recorded, reset_elem_for_test());
    let done = snap(six(), OrderState::Server, WriteState::Saved);
    step_page(&mut page, &done, ScreenEvent::Tick(plx_machine::machine::Tick { ms: 16, dt_us: 16_000 }));
    assert!(!reset_stops(&page), "the store has no record now: Reset is inert");
    assert_eq!(page.form.selected_key(), Some(lib_key(5)), "focus is on the last library");
}

// ---- what the page shows until the store says otherwise ---------------------------------------

#[test]
fn the_page_keeps_its_shown_order_until_the_store_publishes_something_new() {
    let s = snap(six(), OrderState::Server, WriteState::Idle);
    let mut page = page_of(&s);
    pick_up(&mut page, &s, 2);
    press(&mut page, &s, Key::Up);
    ok_on(&mut page, &s, lib_elem_for_test(2));
    let tick = || ScreenEvent::Tick(plx_machine::machine::Tick { ms: 16, dt_us: 16_000 });
    step_page(&mut page, &s, tick());
    assert_eq!(order(&page), [0, 2, 1, 3, 4, 5], "the stale publication does not snap it back");
    let mut moved = six();
    moved.swap(1, 2);
    let after = snap(moved, RECORDED, WriteState::InFlight);
    step_page(&mut page, &after, tick());
    assert_eq!(order(&page), [0, 2, 1, 3, 4, 5], "the new publication agrees");
    assert!(page.state.seen.reset_offered(), "and Reset is offered now");
}

#[test]
fn a_publication_that_lands_while_a_library_is_held_waits_for_the_drop() {
    let s = snap(six(), OrderState::Server, WriteState::Idle);
    let mut page = page_of(&s);
    pick_up(&mut page, &s, 1);
    press(&mut page, &s, Key::Down);
    let other = snap(six().into_iter().rev().collect(), RECORDED, WriteState::Saved);
    step_page(&mut page, &other, ScreenEvent::Tick(plx_machine::machine::Tick { ms: 16, dt_us: 16_000 }));
    assert_eq!(order(&page), [0, 2, 1, 3, 4, 5], "the held order is not replaced under the viewer's hands");
}

// ---- the sentences ----------------------------------------------------------------------------

#[test]
fn every_mode_says_its_sentence() {
    use plx_os::i18n::msg;
    let s = snap(six(), RECORDED, WriteState::Saved);
    let mut page = page_of(&s);
    let say = |page: &LibraryOrderPage| page.state.description();
    focus_on(&mut page, &s, lib_elem_for_test(0));
    assert_eq!(say(&page), msg::library_order_state_idle(msg::browse_kind_movies(), "Movies"), "idle, on a library");
    focus_on(&mut page, &s, lib_elem_for_test(4));
    assert_eq!(say(&page), msg::library_order_state_idle(msg::browse_kind_tv_shows(), "Anime"));
    assert!(say(&page).starts_with("Press OK to move Anime"), "the title is the focused library's: {}", say(&page));
    focus_on(&mut page, &s, reset_elem_for_test());
    assert_eq!(say(&page), msg::library_order_state_reset(), "idle, on Reset");
    ok_on(&mut page, &s, reset_elem_for_test());
    assert_eq!(say(&page), msg::library_order_state_confirm());
    press(&mut page, &s, Key::Up);
    assert_eq!(page.state.mode, Mode::Idle);
    pick_up(&mut page, &s, 4);
    assert_eq!(say(&page), msg::library_order_state_moving("Anime"));
    for _ in 0..3 {
        press(&mut page, &s, Key::Up);
    }
    ok_on(&mut page, &s, lib_elem_for_test(4));
    // Anime now stands second of all six on Home and first among the TV shows in the header
    assert_eq!(order(&page), [0, 4, 1, 2, 3, 5]);
    assert_eq!(say(&page), msg::library_order_state_dropped("first", "second", msg::browse_kind_tv_shows(), "Anime"));
    // the arguments land in their own places (the generated function orders them by name)
    let said = say(&page);
    let (title_at, home_at, header_at) = (said.find("Anime").unwrap(), said.find("second").unwrap(), said.find("first").unwrap());
    assert!(title_at < home_at && home_at < header_at, "title, then its place on Home, then in the header: {said}");
    press(&mut page, &s, Key::Down);
    assert_eq!(page.state.mode, Mode::Idle, "the sentence lasts until the next key");
    focus_on(&mut page, &s, reset_elem_for_test());
    ok_on(&mut page, &s, reset_elem_for_test());
    ok_on(&mut page, &s, reset_elem_for_test());
    assert_eq!(say(&page), msg::library_order_state_reset_done());
}

#[test]
fn a_failed_write_says_so_in_words_and_never_in_the_middle_of_a_move() {
    use plx_os::i18n::msg;
    let s = snap(six(), RECORDED, WriteState::Failed);
    let mut page = page_of(&s);
    let idle = page.state.description();
    assert!(idle.ends_with(msg::library_order_state_unsaved()), "{idle}");
    pick_up(&mut page, &s, 0);
    assert_eq!(page.state.description(), msg::library_order_state_moving("Movies"));
    let ok = snap(six(), RECORDED, WriteState::Saved);
    assert!(!page_of(&ok).state.description().contains(msg::library_order_state_unsaved()));
}

// ---- the preview ------------------------------------------------------------------------------

fn movies(n: usize) -> Vec<LibraryOrderRow> {
    (0..n).map(|i| row(i, &format!("Movie library number {i}"), SecKind::Movie, "")).collect()
}

fn tick_ev() -> ScreenEvent<InnerHost> {
    ScreenEvent::Tick(plx_machine::machine::Tick { ms: 16, dt_us: 16_000 })
}

#[test]
fn the_preview_overflow_is_what_names_row_says_for_the_same_pills() {
    for count in [1, 3, 5, 12] {
        let s = snap(movies(count), OrderState::Server, WriteState::Idle);
        let mut page = page_of(&s);
        step_page(&mut page, &s, tick_ev());
        let pills: Vec<(String, usize)> = page.state.shown.iter().map(|r| (r.title.clone(), r.section)).collect();
        let (right, widest) = crate::registry::header_controls_at_rest(&MEASURE);
        let expect = plx_ui::page_header::names_row(&pills, 0, right, &widest, crate::registry::MAX_LIBRARY_PILLS, &MEASURE);
        assert_eq!(page.preview.overflowed(), expect.overflowed(), "{count} libraries");
        assert_eq!(page.preview.slot().is_some(), true);
    }
    let few = snap(movies(1), OrderState::Server, WriteState::Idle);
    let mut one = page_of(&few);
    step_page(&mut one, &few, tick_ev());
    assert!(!one.preview.overflowed(), "one library never folds");
    let many = snap(movies(12), OrderState::Server, WriteState::Idle);
    let mut twelve = page_of(&many);
    step_page(&mut twelve, &many, tick_ev());
    assert!(twelve.preview.overflowed(), "twelve past the cap of eight always fold");
}

#[test]
fn the_preview_shows_the_kind_of_the_library_in_view_and_the_last_ones_with_reset_in_focus() {
    let s = snap(six(), RECORDED, WriteState::Saved);
    let mut page = page_of(&s);
    focus_on(&mut page, &s, lib_elem_for_test(2));
    step_page(&mut page, &s, tick_ev());
    assert_eq!(page.preview.shown_names().len(), 2, "the two Movies libraries");
    assert_eq!(page.preview.plated().as_deref(), Some("4K Movies"));
    focus_on(&mut page, &s, lib_elem_for_test(4));
    step_page(&mut page, &s, tick_ev());
    assert!(page.preview.shown_names().iter().any(|n| n == "Anime anna"), "the plated name is on the row: {:?}", page.preview.shown_names());
    assert!(page.preview.shown_names().iter().all(|n| n != "Movies"), "and no Movies library is in a TV shows header");
    assert_eq!(page.preview.plated().as_deref(), Some("Anime anna"), "an owner's credit travels with the name");
    focus_on(&mut page, &s, reset_elem_for_test());
    step_page(&mut page, &s, tick_ev());
    assert_eq!(page.preview.plated().as_deref(), Some("Documentaries"), "Reset: the last library's kind");
    focus_on(&mut page, &s, lib_elem_for_test(0));
    pick_up(&mut page, &s, 0);
    press(&mut page, &s, Key::Down);
    press(&mut page, &s, Key::Down);
    step_page(&mut page, &s, tick_ev());
    assert_eq!(page.preview.shown_names(), ["4K Movies", "Movies"], "the held library moves in the preview with it");
}

// ---- the root row -----------------------------------------------------------------------------

#[test]
fn the_root_row_reads_server_order_unless_a_saved_order_is_in_effect() {
    use plx_os::i18n::msg;
    let rows = || { let mut r = six(); r.swap(0, 4); r };
    let value = |state, rows: Vec<LibraryOrderRow>| root_value(snap(rows, state, WriteState::Saved).view());
    assert_eq!(value(OrderState::Server, six()), msg::library_order_value_server());
    assert_eq!(value(OrderState::Recorded { in_effect: false }, six()), msg::library_order_value_server(), "saved but not in effect");
    assert_eq!(value(RECORDED, rows()), msg::library_order_value_first("Anime"));
    assert_eq!(root_value(DirectorySnapshot::default().view()), msg::library_order_value_server(), "no libraries yet");
}

#[test]
fn the_root_lists_the_row_between_favorite_libraries_and_under_posters_and_opens_the_page() {
    let _g = plx_base::testlock::serial();
    let mut form = FormTable::<RootId, Action, SettingsPage>::page(super::super::super::registry::BAND);
    form.set(root_form(&super::super::text_fit_tests::base_root_inputs()), None);
    let at = |id: RootId| form.index_of(&id).expect("on the signed-in root");
    assert_eq!(at(RootId::LibraryOrder), at(RootId::Favourites) + 1);
    assert_eq!(at(RootId::UnderPosters), at(RootId::LibraryOrder) + 1);
    assert_eq!(form.activate(at(RootId::LibraryOrder)).map(|a| matches!(a, plx_ui::form::Activation::Push(SettingsPage::LibraryOrder))), Some(true));
    assert_eq!(sentence_for(Some(&RootId::LibraryOrder)), plx_os::i18n::msg::library_order_row_detail());
}

// ---- every string fits ------------------------------------------------------------------------

#[test]
fn every_library_order_string_fits_in_every_language() {
    let mut out = Vec::new();
    let long = || {
        let mut rows = six();
        rows[0].title = "Documentaries and concerts".into();
        rows
    };
    for language in SHIPPED {
        let _guard = language_on_this_thread_for_test(language);
        let tag = language.tag();
        for (state, name) in [(OrderState::Server, "server"), (RECORDED, "recorded")] {
            let s = snap(long(), state, WriteState::Saved);
            let page = page_of(&s);
            super::super::text_fit_tests::overflowing(&format!("{tag} library order ({name})"), &page.form.table, &mut out);
        }
        let value = root_value(snap(long(), RECORDED, WriteState::Saved).view());
        assert!(!value.is_empty(), "{tag}");
        for text in [
            plx_os::i18n::msg::library_order_state_confirm(), plx_os::i18n::msg::library_order_state_reset_done(),
            plx_os::i18n::msg::library_order_preview_more(), plx_os::i18n::msg::library_order_state_unsaved(),
        ] {
            assert!(!text.contains('\u{22ef}'), "{tag}: the ⋯ glyph is not in the shipped faces: {text}");
        }
    }
    assert!(out.is_empty(), "{out:#?}");
}
