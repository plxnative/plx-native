use super::*;

use plx_data::livetv::{Channel, LiveTvView};
use plx_data::stores::livetv::LiveTvStore;
use plx_machine::machine::{Chrome, FocusRead, Host, InputOwner, InstanceId, PressId, PressRead, ScreenId, Stamped};
use plx_ui::fixture::FixtureMeasure;
use plx_ui::fmt::Wall;
use plx_ui::screen::ScreenArg;

use crate::registry::PlayIntent;

#[derive(Clone, PartialEq, Eq)]
struct TestArg;

impl LogicalState for TestArg {
    fn write(&self, c: &mut Canon) {
        c.u32(0);
    }
    fn probe(&self, _: &mut String) {}
}

impl ScreenArg for TestArg {
    fn chrome(&self) -> Chrome {
        Chrome::TabBar
    }
    fn id(&self) -> ScreenId {
        ScreenId(801)
    }
    fn title(&self) -> Option<&str> {
        None
    }
    fn same_instance(&self, other: &Self) -> bool {
        self == other
    }
}

struct TestHost;

impl Host for TestHost {
    type Arg = TestArg;
    type Fx = AppFx;
    type Msg = crate::registry::AppMsg;
    type Elem = u32;
    type Views<'a> = LiveTvView<'a>;
    type Init = crate::registry::NoInit;
    type Memory = crate::registry::PageMemory;
}

thread_local! {
    /// The wall time the test host publishes (`None`: the frame has no clock).
    static WALL: std::cell::Cell<Option<Wall>> = const { std::cell::Cell::new(None) };
}

impl LiveTvLike for TestHost {
    fn livetv<'a>(cx: &Cx<'a, Self>) -> LiveTvView<'a> {
        cx.views
    }
    fn wall(_cx: &Cx<'_, Self>) -> Option<Wall> {
        WALL.with(std::cell::Cell::get)
    }
}

/// 2026-11-25 20:30:00 UTC, the wall time the caption tests run at.
const NOW: i64 = 1_795_638_600;
const MIN: i64 = 60;

const ENTRY: EntryId = EntryId(7);

fn cx<'a>(view: LiveTvView<'a>, focus: Option<u32>) -> Cx<'a, TestHost> {
    static MEASURE: FixtureMeasure = FixtureMeasure;
    Cx {
        views: view,
        tick: Tick::default(),
        measure: &MEASURE,
        press: PressRead::default(),
        modal: Default::default(), page_image: Default::default(), hold_hint: Default::default(),
        focus: FocusRead { current: focus.map(|elem| FocusKey { entry: ENTRY, elem }), ..Default::default() },
        owner: InputOwner::Entry(ENTRY),
    }
}

fn step(s: &mut LiveTvScreen, view: LiveTvView<'_>, focus: Option<u32>, ev: &ScreenEvent<TestHost>) -> Vec<Stamped<TestHost>> {
    step_handled(s, view, focus, ev).1
}

/// [`step`], also answering whether the screen handled the event.
fn step_handled(s: &mut LiveTvScreen, view: LiveTvView<'_>, focus: Option<u32>, ev: &ScreenEvent<TestHost>) -> (Handled, Vec<Stamped<TestHost>>) {
    let context = cx(view, focus);
    let mut out = Vec::new();
    let mut present = plx_machine::present::Present::new();
    let mut fx = Effects::new(&mut out, plx_machine::machine::MachineId::Instance(InstanceId(9)), &mut present);
    let handled = Machine::<TestHost>::step(s, ev, &context, &mut fx);
    (handled, out)
}

fn channel(vcn: &str, name: &str) -> Channel {
    Channel { sid: plx_plex::plex::ServerId::UNSET, dvr: "7".into(), channel: format!("C{vcn}"),
        vcn: vcn.into(), name: name.into(), thumb: format!("/logos/{vcn}.jpg"), now: "Family Show".into(),
        now_thumb: format!("/programmes/{vcn}.jpg"), now_summary: String::new(), begins_at: 20, ends_at: 100 }
}

/// A channel whose programme began `ago` minutes before [`NOW`] and runs `mins` minutes.
fn airing(vcn: &str, name: &str, ago: i64, mins: i64) -> Channel {
    Channel { begins_at: NOW - ago * MIN, ends_at: NOW - ago * MIN + mins * MIN, ..channel(vcn, name) }
}

fn seeded() -> (LiveTvStore, LiveTvScreen) {
    let mut store = LiveTvStore::default();
    let mut silent = channel("103", "Music");
    silent.now.clear();
    store.install_for_test(vec![channel("101", "Family Channel"), channel("102", "Cartoons"), silent]);
    let mut screen = LiveTvScreen::new(ENTRY);
    step(&mut screen, store.view(), None, &ScreenEvent::Enter(Enter::Fresh { focus: FocusTarget::ContainerGroup(RECENT_GROUP) }));
    (store, screen)
}

/// `store`'s view with `recent` as the profile's Recent row.
fn with_recent<'a>(store: &'a LiveTvStore, recent: &'a [Channel]) -> LiveTvView<'a> {
    store.view().for_test_with_recent(recent)
}

fn asked_plays(out: &[Stamped<TestHost>]) -> Vec<PlayIntent> {
    out.iter().filter_map(|s| match &s.fx {
        Fx::App(AppFx::Content(ContentReq::Play { play, resume_ns: 0 })) => Some(play.clone()), _ => None,
    }).collect()
}

#[test]
fn own_store_supplies_adjacent_recent_and_on_now_shelves_with_distinct_art() {
    let _policy = captions::PolicyPin::default_setting();
    let _language = plx_os::i18n::language_on_this_thread_for_test(plx_os::i18n::Preference::En);
    let (store, mut screen) = seeded();
    let recent = [channel("102", "Cartoons"), channel("101", "Family Channel")];
    let view = with_recent(&store, &recent);
    step(&mut screen, view, None, &ScreenEvent::StoreChanged(StoreId::LiveTv.ord(), 1));
    let context = cx(view, None);
    let mut sections = Vec::new();
    StackPage::<TestHost>::sections(&screen.page, &context, &mut sections);
    assert_eq!(sections.iter().map(|s| s.key).collect::<Vec<_>>(), [Sec::Head, Sec::Recent, Sec::Now, Sec::Status]);
    assert!(matches!(sections[1].kind, Kind::Shelf { .. }) && matches!(sections[2].kind, Kind::Shelf { .. }));
    // the air under Recent: 30 at rest (the handoff's 48 from the tiles to On now's heading line, less the 18 of
    // the collapsed label band, plus the ink drop), 22 with a tile focused (its 112), each plus the ink drop
    assert_eq!(StackPage::<TestHost>::shelf_foot(&screen.page, &context, Sec::Recent), 30.0 + HEAD_INK_DROP);
    assert_eq!(StackPage::<TestHost>::shelf_foot_open(&screen.page, &context, Sec::Recent), UNDER_LABEL_AIR + HEAD_INK_DROP);
    let recent_cards = StackPage::<TestHost>::cards(&screen.page, &context, Sec::Recent).unwrap();
    let now = StackPage::<TestHost>::cards(&screen.page, &context, Sec::Now).unwrap();
    assert_eq!(CardSource::<TestHost>::len(&recent_cards), 2, "the profile's own channels, newest first");
    assert_eq!(CardSource::<TestHost>::len(&now), 2, "the channel without programme data has no poster");
    let Art::Channel { logo, number, name, .. } = CardSource::<TestHost>::art(&recent_cards, 0) else { panic!("a logo plate") };
    assert_eq!((logo, number, name), ("/logos/102.jpg", "102", "Cartoons"));
    let Art::Poster(Some(airing)) = CardSource::<TestHost>::art(&now, 0) else { panic!("programme artwork") };
    assert_eq!((airing.thumb, airing.title), ("/programmes/101.jpg", "Family Show"));
    assert_ne!(CardSource::<TestHost>::elem(&recent_cards, 0), CardSource::<TestHost>::elem(&now, 0), "one channel, two shelves, two elements");
}

/// On now's one order: the most recent start first, a tie in lineup order; Recent is the store's order.
#[test]
fn on_now_lists_the_most_recent_start_first_and_a_tie_in_lineup_order() {
    let (mut store, mut screen) = seeded();
    let mut silent = channel("109", "Silent");
    silent.now.clear();
    store.install_for_test(vec![
        airing("101", "Old", 50, 60), airing("102", "Newest", 2, 60), airing("103", "Tie A", 20, 60),
        silent, airing("105", "Tie B", 20, 30), airing("106", "Mid", 35, 60),
    ]);
    step(&mut screen, store.view(), None, &ScreenEvent::Tick(Tick { ms: 16, dt_us: 16_667 }));
    let names: Vec<&str> = screen.page.airings.iter().map(|&i| store.view().channels[i].name.as_str()).collect();
    assert_eq!(names, ["Newest", "Tie A", "Tie B", "Mid", "Old"]);
}

#[test]
fn recent_keeps_its_order_hides_while_empty_and_never_leaves_the_page_without_a_row() {
    let (mut store, mut screen) = seeded();
    let recent = [channel("102", "Cartoons"), channel("101", "Family Channel")];
    step(&mut screen, with_recent(&store, &recent), None, &ScreenEvent::StoreChanged(StoreId::LiveTv.ord(), 1));
    assert_eq!(screen.page.recent, [1, 0], "newest first, as the store keeps it, by lineup position");
    assert_eq!(screen.page.regions().content, &[RECENT_GROUP, NOW_GROUP]);
    // no Recent channel and a programme on: the row is hidden
    store.install_for_test(vec![channel("101", "Family Channel"), channel("102", "Cartoons")]);
    step(&mut screen, store.view(), None, &ScreenEvent::StoreChanged(StoreId::LiveTv.ord(), 1));
    assert!(screen.page.recent.is_empty(), "Recent is empty while there are programmes to show");
    assert_eq!(screen.page.regions().content, &[NOW_GROUP]);
    // nothing recent and nothing on: still no Recent row, no row stands in, and the page reads out its
    // existing empty state
    let mut dark = channel("101", "Family Channel");
    dark.now.clear();
    let mut dark2 = channel("102", "Cartoons");
    dark2.now.clear();
    store.install_for_test(vec![dark, dark2]);
    step(&mut screen, store.view(), None, &ScreenEvent::StoreChanged(StoreId::LiveTv.ord(), 1));
    assert!(screen.page.recent.is_empty() && screen.page.airings.is_empty());
    assert!(screen.page.regions().content.is_empty());
    assert_eq!(screen.page.status, LiveTvStatus::Empty);
}

/// An empty Recent means no Recent shelf: with programmes on now the page is On now alone, and with
/// nothing on either it is the empty read-out, never a Recent row of lineup channels.
#[test]
fn an_empty_recent_is_no_recent_shelf_with_programmes_on_or_with_none() {
    let _policy = captions::PolicyPin::default_setting();
    for programmes in [true, false] {
        let (mut store, mut screen) = seeded();
        let mut lineup = vec![channel("101", "Family Channel"), channel("102", "Cartoons")];
        if !programmes { for c in &mut lineup { c.now.clear(); } }
        store.install_for_test(lineup);
        step(&mut screen, store.view(), None, &ScreenEvent::StoreChanged(StoreId::LiveTv.ord(), 1));
        let context = cx(store.view(), None);
        let mut specs = Vec::new();
        StackPage::<TestHost>::sections(&screen.page, &context, &mut specs);
        assert_eq!(specs.iter().map(|s| s.key).collect::<Vec<_>>(), [Sec::Head, Sec::Recent, Sec::Now, Sec::Status], "the section list is fixed; a shelf without cards is nothing");
        let recent = StackPage::<TestHost>::cards(&screen.page, &context, Sec::Recent);
        assert_eq!(recent.as_ref().map_or(0, |c| CardSource::<TestHost>::len(c)), 0, "programmes on: {programmes}");
        let now = StackPage::<TestHost>::cards(&screen.page, &context, Sec::Now);
        assert_eq!(now.as_ref().map_or(0, |c| CardSource::<TestHost>::len(c)), if programmes { 2 } else { 0 });
        assert_eq!(screen.page.regions().content.to_vec(), if programmes { vec![NOW_GROUP] } else { Vec::new() }, "no group for a Recent row");
        assert_eq!(screen.page.status, if programmes { LiveTvStatus::Ready } else { LiveTvStatus::Empty });
    }
}

#[test]
fn a_recent_tile_says_what_is_on_and_when_it_started_and_a_poster_says_which_channel_and_how_long_is_left() {
    let _policy = captions::PolicyPin::default_setting();
    let _language = plx_os::i18n::language_on_this_thread_for_test(plx_os::i18n::Preference::En);
    let (mut store, mut screen) = seeded();
    let mut silent = channel("103", "Music");
    silent.now.clear();
    // started 3 min ago; started 40 min ago (an hour long); no programme
    store.install_for_test(vec![airing("101", "Family Channel", 3, 60), airing("102", "Cartoons", 40, 60), silent]);
    let recent = store.view().channels.to_vec();
    let view = with_recent(&store, &recent);
    step(&mut screen, view, None, &ScreenEvent::StoreChanged(StoreId::LiveTv.ord(), 1));
    WALL.with(|w| w.set(Some(Wall::from_secs(NOW, 0))));
    let context = cx(view, None);
    let words = |label: &TileLabel| (label.title.as_ref().map(|t| t.to_str().unwrap().to_owned()), label.caption.as_ref().map(|c| c.to_str().unwrap().to_owned()));
    let recent_cards = StackPage::<TestHost>::cards(&screen.page, &context, Sec::Recent).unwrap();
    // just started: how long ago, then what is left
    assert_eq!(words(&CardSource::<TestHost>::label(&recent_cards, 0)),
        (Some("On now: Family Show".into()), Some("Started 3 min ago \u{b7} 57m left".into())));
    // later: the clock times, then what is left
    let (title, caption) = words(&CardSource::<TestHost>::label(&recent_cards, 1));
    assert_eq!(title.as_deref(), Some("On now: Family Show"));
    let caption = caption.unwrap();
    assert!(caption.contains('\u{2013}') && caption.ends_with(" \u{b7} 20m left"), "{caption}");
    // a channel with no programme is its name alone
    assert_eq!(words(&CardSource::<TestHost>::label(&recent_cards, 2)), (Some("Music".into()), None));
    // the poster: the programme over `Ch <number> · <station> · <left>`
    let now = StackPage::<TestHost>::cards(&screen.page, &context, Sec::Now).unwrap();
    assert_eq!(words(&CardSource::<TestHost>::label(&now, 0)),
        (Some("Family Show".into()), Some("Ch 101 \u{b7} Family Channel \u{b7} 57m left".into())));
    // and the bar: how much has aired, from the same airing
    let Art::Poster(Some(facts)) = CardSource::<TestHost>::art(&now, 1) else { panic!("poster") };
    let resume = facts.resume.expect("a programme on air is in progress");
    assert!((resume.frac - 40.0 / 60.0).abs() < 1e-3 && resume.left_ms == 20 * MIN * 1_000, "{resume:?}");
    // the shelf paints that bar: the card source's own progress, and none on the Recent tiles
    assert!((CardSource::<TestHost>::progress(&now, 1).unwrap() - 40.0 / 60.0).abs() < 1e-3);
    assert_eq!(CardSource::<TestHost>::progress(&recent_cards, 1), None);
    // no wall time (a replay): no clock words and no bar, the channel's identity alone
    WALL.with(|w| w.set(None));
    let context = cx(view, None);
    let now = StackPage::<TestHost>::cards(&screen.page, &context, Sec::Now).unwrap();
    assert_eq!(words(&CardSource::<TestHost>::label(&now, 0)), (Some("Family Show".into()), Some("Ch 101 \u{b7} Family Channel".into())));
    let Art::Poster(Some(facts)) = CardSource::<TestHost>::art(&now, 0) else { panic!("poster") };
    assert_eq!(facts.resume, None);
}

#[test]
fn the_page_asks_for_a_frame_when_the_words_of_an_airing_change_and_not_before() {
    let (store, mut screen) = seeded();
    let view = store.view();
    // One tick; whether the page asked for a frame because of the clock (the page's own ground
    // animates for a while after its first tick, so the clock stamp is what is read).
    let tick = |screen: &mut LiveTvScreen, ms: u32| {
        let context = cx(view, None);
        let mut out = Vec::new();
        let mut present = plx_machine::present::Present::new();
        let mut fx = Effects::new(&mut out, plx_machine::machine::MachineId::Instance(InstanceId(9)), &mut present);
        let before = screen.page.clock;
        Machine::<TestHost>::step(screen, &ScreenEvent::Tick(Tick { ms, dt_us: 16_667 }), &context, &mut fx);
        screen.page.clock != before
    };
    WALL.with(|w| w.set(None));
    assert!(!tick(&mut screen, 16), "a page with no wall time has no clock words to redraw");
    assert!(!tick(&mut screen, 32));
    // the airings begin at 20 s and end at 100 s: at 0 they have not begun, at 30 they are on air
    WALL.with(|w| w.set(Some(Wall::from_secs(0, 0))));
    assert!(tick(&mut screen, 48), "the first wall time is read");
    assert!(!tick(&mut screen, 64), "the same words: nothing to draw");
    WALL.with(|w| w.set(Some(Wall::from_secs(30, 0))));
    assert!(tick(&mut screen, 80), "they began: the words and the bar change");
    WALL.with(|w| w.set(Some(Wall::from_secs(31, 0))));
    assert!(!tick(&mut screen, 96), "the same minute: the same words");
    WALL.with(|w| w.set(None));
}

#[test]
fn channel_and_airing_ok_use_the_same_content_play_without_a_page_tuning_guard() {
    let (store, mut screen) = seeded();
    let recent = store.view().channels.to_vec();
    let view = with_recent(&store, &recent);
    step(&mut screen, view, None, &ScreenEvent::StoreChanged(StoreId::LiveTv.ord(), 1));
    for (press, elem) in [FIRST_CARD_ELEM + 2, FIRST_CARD_ELEM + 3, FIRST_CARD_ELEM + 3].into_iter().enumerate() {
        let focus = FocusKey { entry: ENTRY, elem };
        step(&mut screen, view, Some(elem), &ScreenEvent::FocusMoved { from: None, to: focus, by: plx_ui::screen::By::Dir });
        let out = step(&mut screen, view, Some(elem), &ScreenEvent::PressCommit(PressId(press as u32 + 1)));
        assert_eq!(asked_plays(&out), [PlayIntent::Live { sid: store.view().channels[1].sid, dvr: "7".into(), channel: "C102".into(), title: "Cartoons".into() }]);
    }
}

#[test]
fn strip_and_profile_use_home_requests_and_back_at_the_top_selects_home() {
    use plx_ui::widgets::{TAB_KEY_CHIP, TAB_KEY_HOME, TAB_KEY_LIVETV, TAB_KEY_WATCHLIST};
    let (store, mut screen) = seeded();
    for (elem, expected) in [(TAB_KEY_HOME, HomeReq::Tab(HomeTab::Home)), (TAB_KEY_WATCHLIST, HomeReq::Tab(HomeTab::Watchlist)), (TAB_KEY_CHIP, HomeReq::Account)] {
        let out = step(&mut screen, store.view(), Some(elem), &ScreenEvent::Activate(elem));
        assert!(out.iter().any(|s| matches!(&s.fx, Fx::App(AppFx::Home(req)) if *req == expected)));
    }
    assert_eq!(LiveTvScreen::strip_press(TAB_KEY_LIVETV), Some(None));
    let back = ScreenEvent::Input(InputEvent { at: Tick::default(), source: plx_machine::machine::Source::Script,
        kind: InputKind::Key { key: Key::Back, sym: 0, wcode: 0, edge: Edge::Down, at_edge: false } });
    let out = step(&mut screen, store.view(), Some(FIRST_CARD_ELEM), &back);
    assert!(out.iter().any(|s| matches!(&s.fx, Fx::App(AppFx::Home(HomeReq::Tab(HomeTab::Home))))));
}

#[test]
fn store_revisions_and_notices_refresh_programmes_and_status_retry_targets_live_tv() {
    let (mut store, mut screen) = seeded();
    let mut changed = channel("101", "Family Channel");
    changed.now.clear();
    store.install_for_test(vec![changed]);
    step(&mut screen, store.view(), None, &ScreenEvent::Tick(Tick { ms: 16, dt_us: 16_667 }));
    assert!(screen.page.airings.is_empty(), "the store revision is read on Tick");
    assert!(screen.page.recent.is_empty(), "a page with nothing on has no Recent row of lineup channels");
    assert_eq!(screen.page.status, LiveTvStatus::Empty, "and reads out the empty state");
    for status in [LiveTvStatus::Loading, LiveTvStatus::Empty, LiveTvStatus::Failed] {
        let view = LiveTvView::for_test(status, &[], &[], 100 + status as u32);
        step(&mut screen, view, None, &ScreenEvent::StoreChanged(StoreId::LiveTv.ord(), 1));
        assert_eq!(screen.page.status, status);
        let overlay = screen.page.status_overlay(0);
        assert_eq!(overlay.kind, match status { LiveTvStatus::Loading => StatusKind::Working, LiveTvStatus::Empty => StatusKind::Empty, _ => StatusKind::Failed });
        if status == LiveTvStatus::Empty { assert_eq!(overlay.caption, plx_os::i18n::msg::browse_livetv_empty_c()); }
        assert_eq!(overlay.action.is_some(), status == LiveTvStatus::Failed);
    }
    let failed = LiveTvView::for_test(LiveTvStatus::Failed, &[], &[], 200);
    let out = step(&mut screen, failed, Some(RETRY_ELEM), &ScreenEvent::Activate(RETRY_ELEM));
    assert!(out.iter().any(|s| matches!(&s.fx, Fx::App(AppFx::Store(StoreId::LiveTv, StoreCmd::LiveTv(LiveTvCmd::Retry))))));
}
