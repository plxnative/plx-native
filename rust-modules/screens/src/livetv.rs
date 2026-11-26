//! The What's on page (Live TV's landing page, the handoff's `SPEC-1` / `SPEC-2`): the profile's Recent
//! channels on top, the programmes airing now below, two adjacent shared card shelves over the store's one
//! lineup. A Recent tile and a programme poster both ask the content owner to play their channel.
//!
//! **On now has one fixed order**: the most recent start first, a tie in lineup order. **Recent never
//! reorders** (the store keeps it, newest first) and is hidden while it is empty. A lineup with neither a
//! Recent channel nor a programme (no guide data, nothing tuned yet) keeps the page's earlier shelf, the
//! whole lineup under "Channels", in the first row's place; a lineup with no channel at all is the
//! store's empty read-out. The page reads the frame's wall time ([`LiveTvLike::wall`]) for its
//! captions and progress bars, and asks for a frame when the words of any airing change.
use plx_data::livetv::{Channel, LiveTvStatus, LiveTvView};
use plx_data::stores::{StoreCmd, StoreId};
use plx_data::stores::livetv::LiveTvCmd;
use plx_machine::machine::{Canon, Cx, Delivery, Edge, Effects, Enter, EntryId, FocusKey, FocusTarget,
    Fx, GroupId, Handled, InputEvent, InputKind, Key, LogicalState, Machine, Tick};
use plx_machine::present::Provenance;
use plx_ui::cards::{CardEvent, CardSource, Kind, RowStyle, SectionSpec, Stack, StackEvent, StackPage, TileLabel};
use plx_ui::consts::*;
use plx_ui::page_header::{HeaderItem, HeaderRow, PageHeader, PageHeaderSpec, SharedRow};
use plx_ui::page_regions::{Arrival, BackStep, PageRegions};
use plx_ui::screen::{AxisMask, DrawFrame, EdgeRule, ElemKind, FocusSource, Focusable, GroupKind,
    GroupSpec, HitSource, Link, RenderStrategy, Screen, ScreenEvent, Seat};
use plx_ui::theme;
use plx_ui::fmt::{self, Airing, AiringPhase, Wall};
use plx_ui::tile::Resume;
use plx_ui::widgets::{AmbientWash, Art, PageGround, StatusKind, StatusOverlay};
use plx_ui::{Env, Rect};
use crate::registry::{captions, tile_facts, AppFx, ContentReq, HomeReq, HomeTab, LiveTvLike, PlayIntent};

#[cfg(test)]
pub(crate) mod cards_harness;

const FIRST_CARD_ELEM: u32 = 0x1000;
const RETRY_ELEM: u32 = 1;
const RECENT_GROUP: GroupId = GroupId(0);
const NOW_GROUP: GroupId = GroupId(1);
const HEAD_GROUP: GroupId = GroupId(2);
const STATUS_GROUP: GroupId = GroupId(3);
/// The handoff sets a heading in a 40 px line box whose top is the y it names (Recent at 244, On now at
/// 488) and centres the 32 px type in it, which puts the cap-top this much below that y; a shelf here
/// draws its heading at the cap-top, so every section starts this much lower and gives its heading
/// this much less (the tiles stay where the board has them).
const HEAD_INK_DROP: f32 = 8.0;
/// A shelf's heading to its tiles: the handoff's 56 less [`HEAD_INK_DROP`] (a Home shelf keeps
/// `TITLE_DY + CARD_DY`, 60).
const SHELF_HEADING: f32 = 56.0 - HEAD_INK_DROP;
const HEAD_H: f32 = plx_ui::page_header::GRID_TOP + HEAD_INK_DROP;
/// Recent's tiles: a logo plate, 250 x 140 on the poster pitch, so a poster stands under each tile.
const RECENT_STYLE: RowStyle = RowStyle::CHANNEL;
const NOW_STYLE: RowStyle = RowStyle::HOME;
/// The air from the bottom of Recent's tiles to the heading under them at rest (the handoff's 48): the
/// collapsed label band plus [`RECENT_FOOT_REST`]. With a tile focused the band opens and the foot runs
/// down to [`UNDER_LABEL_AIR`], which is the 552 the handoff gives the heading then.
const RECENT_GAP_AT_REST: f32 = 48.0 + HEAD_INK_DROP;
const RECENT_FOOT_REST: f32 = RECENT_GAP_AT_REST - plx_ui::cards::LABEL_BAND_COLLAPSED;
const STATUS_FRAME: Rect = Rect { x: MARGIN_X, y: plx_ui::page_header::GRID_TOP, w: SCR_W - 2.0 * MARGIN_X, h: SCR_H - plx_ui::page_header::GRID_TOP - MARGIN_Y };

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Sec { Head, Recent, Now, Status }

struct Page {
    entry: EntryId,
    /// The lineup, one `(server, key)` per channel in the store's order (the cards' stable identities).
    channels: Vec<(plx_plex::plex::ServerId, String)>,
    /// The channels with a programme on, as indexes into the lineup, in On now's one order: the most recent
    /// start first, a tie in lineup order.
    airings: Vec<usize>,
    /// Recent's channels, as indexes into the lineup, in the store's order (newest first).
    recent: Vec<usize>,
    status: LiveTvStatus,
    synced: Option<u32>,
    /// What the airings' words and bars were last drawn from ([`Page::clock_stamp`]); not state, a
    /// frame's reason.
    clock: u64,
    header_capsules: plx_ui::widgets::TabStrip,
    header_pop: plx_ui::widgets::CtlPop<1>,
    handoff: plx_ui::scroll_chrome::Handoff,
    header_rows: SharedRow<HeaderRow>,
}

impl Page {
    fn sync(&mut self, view: LiveTvView<'_>) {
        if self.synced == Some(view.revision()) { return; }
        self.synced = Some(view.revision());
        self.channels = view.tiles.iter().map(|m| (m.sid, m.rk.clone())).collect();
        self.airings = on_now_order(view.channels);
        // An empty Recent is a row to hide, whatever else the page holds.
        self.recent = view.recent.iter().filter_map(|r| lineup_index(view.channels, r)).collect();
        self.status = view.status;
    }

    /// Nothing recent and nothing on, but a lineup: the page shows that lineup as its earlier "Channels"
    /// shelf (plan D5) in the first row's place, so a lineup without guide data still reaches its channels.
    /// A lineup with no channel at all is the store's `Empty` status instead.
    fn lineup_only(&self) -> bool {
        self.status == LiveTvStatus::Ready && !self.channels.is_empty() && self.recent.is_empty() && self.airings.is_empty()
    }

    /// One number that moves exactly when the words or the bar of any airing the page shows does
    /// ([`AiringPhase::stamp`]): what the page asks a frame for. `0` without a wall time.
    fn clock_stamp(&self, view: LiveTvView<'_>, wall: Option<Wall>) -> u64 {
        let Some(wall) = wall else { return 0 };
        self.recent.iter().chain(&self.airings).filter_map(|&i| view.channels.get(i))
            .fold(0xcbf2_9ce4_8422_2325_u64, |acc, c| {
                (acc ^ fmt::phase(c.begins_at, c.ends_at, wall.unix_s()).stamp()).wrapping_mul(0x0100_0000_01b3)
            })
    }

    fn regions(&self) -> PageRegions<'static> {
        PageRegions { header: None, content: if self.status == LiveTvStatus::Ready && !self.channels.is_empty() {
            match (self.recent.is_empty(), self.airings.is_empty()) {
                (false, false) => &[RECENT_GROUP, NOW_GROUP],
                (false, true) => &[RECENT_GROUP],
                (true, false) => &[NOW_GROUP],
                (true, true) => &[RECENT_GROUP],
            }
        } else if self.status == LiveTvStatus::Failed { &[STATUS_GROUP] } else { &[] },
            rail: None, back_walks_header: false, arrival: Arrival::Content, pending: None }
    }

    fn header_row(&self, measure: &dyn plx_machine::machine::Measure) -> std::rc::Rc<HeaderRow> {
        let key = std::ptr::from_ref(plx_os::i18n::current()) as usize as u64;
        self.header_rows.get(key, || {
            let left = vec![HeaderItem { label: plx_os::i18n::msg::browse_livetv_whats_on().to_owned(), title: true, ..Default::default() }];
            let page = PageHeader::new(PageHeaderSpec { left, ..Default::default() }, &[], measure);
            let layout = page.layout(SCR_W, measure);
            HeaderRow::new(page, layout, Vec::new())
        })
    }

    fn status_overlay(&self, tick: u32) -> StatusOverlay<'static> {
        match self.status {
            // placeholder-exempt: built here, counted by StatusOverlay's Working draw.
            LiveTvStatus::Loading => StatusOverlay::new(STATUS_FRAME, plx_os::i18n::msg::browse_livetv_loading_c(), StatusKind::Working).phase(tick),
            LiveTvStatus::Empty => StatusOverlay::new(STATUS_FRAME, plx_os::i18n::msg::browse_livetv_empty_c(), StatusKind::Empty),
            LiveTvStatus::Failed => StatusOverlay::new(Rect::FULL, plx_os::i18n::msg::browse_home_failed_c(), StatusKind::Failed)
                .page(plx_ui::icons::Icon::ServerBadgeMinus).action(plx_os::i18n::msg::browse_action_retry_c()),
            LiveTvStatus::Ready => StatusOverlay::new(STATUS_FRAME, c"", StatusKind::Empty),
        }
    }
}

/// On now's one order, as indexes into `channels` (the lineup, in number order): the channels with a
/// programme, the most recent start first; a tie keeps the lineup's order (the sort is stable).
fn on_now_order(channels: &[Channel]) -> Vec<usize> {
    let mut order: Vec<usize> = channels.iter().enumerate().filter_map(|(i, c)| (!c.now.is_empty()).then_some(i)).collect();
    order.sort_by_key(|&i| std::cmp::Reverse(channels[i].begins_at));
    order
}

/// Where a Recent channel stands in the lineup: a channel is its server, DVR and key, never its number.
fn lineup_index(channels: &[Channel], of: &Channel) -> Option<usize> {
    channels.iter().position(|c| c.sid == of.sid && c.dvr == of.dvr && c.channel == of.channel)
}

/// Which shelf a [`Cards`] serves: it decides the cards' art, words and element ids.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Row {
    Recent,
    Now,
    /// The whole lineup as the page's earlier "Channels" shelf, shown when nothing is recent or on.
    Lineup,
}

struct Cards<'a> { view: LiveTvView<'a>, order: &'a [usize], row: Row, wall: Option<Wall> }
impl Cards<'_> {
    /// The lineup index of card `i`: the shelf's own order, or every channel for [`Row::Lineup`].
    fn index(&self, i: usize) -> Option<usize> {
        if self.row == Row::Lineup { (i < self.view.channels.len()).then_some(i) } else { self.order.get(i).copied() }
    }
    fn channel(&self, i: usize) -> Option<(usize, &Channel)> {
        let index = self.index(i)?;
        Some((index, self.view.channels.get(index)?))
    }
    /// The amber bar: how much of the programme has aired. `Resume`'s own rule (present exactly while
    /// in progress) is the airing's phase.
    fn resume(&self, channel: &Channel) -> Option<Resume> {
        let w = self.wall?;
        if !matches!(fmt::phase(channel.begins_at, channel.ends_at, w.unix_s()), AiringPhase::JustStarted { .. } | AiringPhase::Running { .. }) {
            return None;
        }
        Some(Resume { frac: fmt::frac(channel.begins_at, channel.ends_at, w)?, left_ms: (channel.ends_at - w.unix_s()).max(0) * 1_000 })
    }
    /// The low bit of a card's element id: Recent's tiles are even, On now's posters odd.
    fn parity(&self) -> u32 { u32::from(self.row == Row::Now) }
}
impl<H: LiveTvLike> CardSource<H> for Cards<'_> {
    fn len(&self) -> usize { if self.row == Row::Lineup { self.view.channels.len() } else { self.order.len() } }
    fn elem(&self, i: usize) -> u32 { FIRST_CARD_ELEM + self.index(i).unwrap_or(0) as u32 * 2 + self.parity() }
    fn index_of(&self, e: &u32) -> Option<usize> {
        let offset = e.checked_sub(FIRST_CARD_ELEM)?;
        if offset % 2 != self.parity() { return None; }
        let i = offset as usize / 2;
        if self.row == Row::Lineup { (i < self.view.channels.len()).then_some(i) } else { self.order.iter().position(|&n| n == i) }
    }
    fn art(&self, i: usize) -> Art<'_> {
        let Some((index, channel)) = self.channel(i) else { return Art::Poster(None) };
        if self.row == Row::Lineup { return Art::Poster(self.view.tiles.get(index).map(|tile| tile_facts::of(tile))); }
        if self.row == Row::Recent {
            return Art::Channel { sid: channel.sid.raw(), logo: &channel.thumb, number: &channel.vcn, name: &channel.name };
        }
        Art::Poster(self.view.tiles.get(index).map(|tile| {
            let mut facts = tile_facts::of(tile);
            facts.owner = channel as *const _ as usize;
            facts.title = &channel.now;
            facts.show_title = "";
            facts.thumb = &channel.now_thumb;
            facts.still = "";
            facts.art = "";
            facts.resume = self.resume(channel);
            facts
        }))
    }
    /// The poster's bar; a channel tile has none.
    fn progress(&self, i: usize) -> Option<f32> {
        if self.row != Row::Now { return None; }
        self.channel(i).and_then(|(_, c)| self.resume(c)).map(|r| r.frac)
    }
    fn label(&self, i: usize) -> TileLabel {
        let Some((_, channel)) = self.channel(i) else { return TileLabel::default() };
        use plx_os::i18n::msg;
        match self.row {
            Row::Lineup => match self.view.tiles.get(self.channel(i).map_or(usize::MAX, |(index, _)| index)) {
                Some(item) => captions::shelf_label(&captions::live_tv_policy(), item, false),
                None => TileLabel::default(),
            },
            // "On now: <programme>" over when it started and how long is left; a channel with no
            // programme is its name alone.
            Row::Recent if channel.now.is_empty() => captions::live_label(channel.name.clone(), None),
            Row::Recent => captions::live_label(msg::browse_livetv_recent_on_now(&channel.now),
                self.wall.and_then(|w| fmt::airing_line(Airing::Tile, channel.begins_at, channel.ends_at, w))),
            // The programme over its channel, number first ("Ch 2009 · Station · 42m left").
            Row::Now => {
                let identity = if channel.vcn.is_empty() { channel.name.clone() } else { msg::browse_livetv_channel(&channel.name, &channel.vcn) };
                let line = match self.wall.and_then(|w| fmt::left(channel.begins_at, channel.ends_at, w)) {
                    Some(left) => msg::core_airing_pair(&identity, &left),
                    None => identity,
                };
                captions::live_label(channel.now.clone(), Some(line))
            }
        }
    }

    fn subject(&self, i: usize) -> Option<std::borrow::Cow<'_, str>> {
        use std::fmt::Write as _;
        let tile = self.view.tiles.get(self.channel(i)?.0)?;
        // Two servers may advertise the same DVR and channel. A press names the exact source.
        // Hex preserves that key without punctuation the remote subject grammar rejects.
        let mut subject = format!("live-{}-", tile.sid.raw());
        for byte in tile.rk.bytes() {
            write!(subject, "{byte:02x}").expect("writing to a String succeeds");
        }
        Some(subject.into())
    }
}

impl<H: LiveTvLike> StackPage<H> for Page {
    type Key = Sec;
    type Cards<'a> = Cards<'a>;
    fn revision(&self, cx: &Cx<'_, H>) -> u64 { u64::from(H::livetv(cx).revision()) }
    fn sections(&self, cx: &Cx<'_, H>, out: &mut Vec<SectionSpec<Sec>>) {
        out.push(SectionSpec::new(Sec::Head, Kind::Custom { height: HEAD_H, focusable: false }, HEAD_GROUP));
        // The head of the content is seated by its own rule (`CONTENT_HEAD_SEAT`): the first row there is.
        let lineup = self.lineup_only();
        let recent = if lineup {
            SectionSpec::new(Sec::Recent, Kind::Shelf { style: &NOW_STYLE, heading: SHELF_HEADING }, RECENT_GROUP).named("channels")
        } else {
            SectionSpec::new(Sec::Recent, Kind::Shelf { style: &RECENT_STYLE, heading: SHELF_HEADING }, RECENT_GROUP).named("recent")
        };
        let now = SectionSpec::new(Sec::Now, Kind::Shelf { style: &NOW_STYLE, heading: SHELF_HEADING }, NOW_GROUP).named("on_now");
        let head_is_recent = lineup || !self.recent.is_empty();
        out.push(if head_is_recent { recent.seated(plx_ui::page_regions::CONTENT_HEAD_SEAT) } else { recent });
        out.push(if head_is_recent { now } else { now.seated(plx_ui::page_regions::CONTENT_HEAD_SEAT) });
        let retry = (self.status == LiveTvStatus::Failed).then(|| self.status_overlay(cx.tick.ms).action_frame_measured(cx.measure)).flatten();
        out.push(SectionSpec::new(Sec::Status, Kind::Overlay { rect: retry.unwrap_or(STATUS_FRAME), focusable: retry.is_some() }, STATUS_GROUP).named("status"));
    }
    /// Recent keeps the handoff's 48 from its tiles to On now's heading at rest and the label band's own
    /// room (the 552) with a tile focused: [`RECENT_FOOT_REST`] opening to [`UNDER_LABEL_AIR`].
    fn shelf_foot(&self, _cx: &Cx<'_, H>, k: Sec) -> f32 { if k == Sec::Recent && !self.airings.is_empty() { RECENT_FOOT_REST } else { 0.0 } }
    fn shelf_foot_open(&self, _cx: &Cx<'_, H>, k: Sec) -> f32 { if k == Sec::Recent && !self.airings.is_empty() { UNDER_LABEL_AIR + HEAD_INK_DROP } else { 0.0 } }
    fn fallback(&self, _cx: &Cx<'_, H>, out: &mut Vec<Sec>) { out.extend([Sec::Recent, Sec::Now, Sec::Status, Sec::Head]); }
    fn cards<'a>(&'a self, cx: &'a Cx<'_, H>, k: Sec) -> Option<Cards<'a>> {
        let view = H::livetv(cx);
        let (order, row) = match k { Sec::Recent if self.lineup_only() => (&self.recent, Row::Lineup), Sec::Recent => (&self.recent, Row::Recent), Sec::Now => (&self.airings, Row::Now), _ => return None };
        (view.status == LiveTvStatus::Ready).then_some(Cards { view, order, row, wall: H::wall(cx) })
    }
    fn card_has_menu(&self, _cx: &Cx<'_, H>, _k: Sec, _elem: &u32) -> bool { false }
    fn caption_policy(&self, _cx: &Cx<'_, H>, k: Sec) -> Option<plx_ui::caption_policy::CaptionPolicy> {
        matches!(k, Sec::Recent | Sec::Now).then(captions::live_tv_policy)
    }
    fn elem_of(&self, k: Sec) -> Option<u32> { (k == Sec::Status).then_some(RETRY_ELEM) }
    fn plain_name(&self, k: Sec, i: usize) -> Option<&'static str> { (k == Sec::Status && i == 0).then_some("retry") }
    fn plain_group(&self, _cx: &Cx<'_, H>, _k: Sec, id: GroupId, extent: Rect) -> GroupSpec {
        GroupSpec { id, kind: GroupKind::Free, seat: Seat::First, reachable: AxisMask::VERTICAL,
            edge: [EdgeRule::Geometric, EdgeRule::Geometric, EdgeRule::Stop, EdgeRule::Stop], extent, len: 1, elem: ElemKind::Bare }
    }
    fn draw_heading(&self, k: Sec, f: &mut DrawFrame<'_, '_, H>, r: Rect, lift: f32) {
        let title = match k { Sec::Recent if self.lineup_only() => plx_os::i18n::msg::browse_livetv_channels_c(), Sec::Recent => plx_os::i18n::msg::browse_livetv_recent_c(), Sec::Now => plx_os::i18n::msg::browse_livetv_on_now_c(), _ => return };
        plx_ui::cards::draw_heading_c(f.painter.alpha(f.page_alpha).translate(0.0, r.y), title, c"", c"", MARGIN_X, -lift, 1.0, f.measure);
    }
    fn custom_draw(&self, k: Sec, f: &mut DrawFrame<'_, '_, H>, r: Rect, _scroll: f32) {
        if k == Sec::Head {
            let p = f.painter.alpha(f.page_alpha).alpha(self.handoff.head_alpha(r.y));
            self.header_row(f.measure).draw(p, r.y, &self.header_capsules, self.header_pop.scale_with(0, f.press), None);
        } else if k == Sec::Status && self.status != LiveTvStatus::Ready {
            self.status_overlay(f.cx.tick.ms).focused(f.focus.current.is_some_and(|k| k.entry == self.entry && k.elem == RETRY_ELEM))
                .draw_measured(&Env::inert(), f.painter.alpha(f.page_alpha), f.measure);
        }
    }
}

pub struct LiveTvScreen {
    page: Page,
    stack: Stack<Sec>,
    ground: PageGround,
    ground_seeded: bool,
    /// The page's own arrival seat while nobody has chosen another (the Watchlist's rule): `None`
    /// once the reader acts or the page is restored.
    seat: Option<Option<GroupId>>,
    /// Seats this page asked for whose `Enter` has not come back yet.
    own_enters: u8,
    /// How many of those were queued before the reader acted.
    stale_enters: u8,
}

impl LiveTvScreen {
    /// The heartbeat word, byte-identical to the route word.
    pub const WORD: &'static str = "livetv";

    pub const SHAPE: &'static str = "LiveTvScreen{entry:u32,status:u32,channels:[{sid:u32,rk:str}],airings:[u32],recent:[u32],stack:Stack{scroll:{pos:f32,vel:f32},target:f32,sections:[Shelf{row,pop},Shelf{row,pop}]},handoff:Handoff{tucked:bool,chrome:f32,capsule:f32}}";

    pub fn new(entry: EntryId) -> Self {
        Self {
            page: Page {
                entry,
                channels: Vec::new(),
                airings: Vec::new(),
                recent: Vec::new(),
                status: LiveTvStatus::Loading,
                synced: None,
                clock: 0,
                header_capsules: plx_ui::widgets::TabStrip::new(),
                header_pop: plx_ui::widgets::CtlPop::new(),
                handoff: plx_ui::scroll_chrome::Handoff::new(),
                header_rows: SharedRow::new(),
            },
            stack: Stack::new(entry).home_when_unfocused(true),
            ground: PageGround::new(),
            ground_seeded: false,
            seat: None,
            own_enters: 0,
            stale_enters: 0,
        }
    }

    /// Both shelves name the channel that supplies their card (Recent's tile on the even elements, On now's
    /// poster on the odd ones).
    fn press<H: LiveTvLike>(&self, elem: u32, cx: &Cx<'_, H>) -> Option<ContentReq> {
        let index = elem.checked_sub(FIRST_CARD_ELEM)? as usize / 2;
        let view = H::livetv(cx);
        let channel = view.channels.get(index)?;
        if elem % 2 == 1 && channel.now.is_empty() { return None; }
        Some(ContentReq::Play { play: PlayIntent::Live {
            sid: channel.sid, dvr: channel.dvr.clone(), channel: channel.channel.clone(), title: channel.name.clone(),
        }, resume_ns: 0 })
    }

    fn tick<H: LiveTvLike>(&mut self, t: Tick, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) {
        self.page.header_row(cx.measure).step(self.page.entry, cx.focus.current, &mut self.page.header_capsules, &mut self.page.header_pop, t.dt());
        self.page.handoff.step(t.dt(), self.stack.scroll(), self.stack.target(), NOW_STYLE.pitch_fixed(), false);
        // The page's own haze, fixed: it does not follow the focused tile.
        let haze = AmbientWash::target(theme::WASH_LIVE_TV, [theme::WASH_LIVE_TV_W; 4]);
        if self.ground_seeded {
            self.ground.key_target(haze, t.dt());
        } else {
            self.ground.jump_target(haze);
            self.ground_seeded = true;
        }
        // A programme's words and bar move with the minute, with no event to say so.
        let stamp = self.page.clock_stamp(H::livetv(cx), H::wall(cx));
        if stamp != self.page.clock {
            self.page.clock = stamp;
            fx.invalidate(Provenance::Input);
        }
    }

    fn route<H: LiveTvLike>(&self, out: HomeReq, fx: &mut Effects<'_, H>) {
        fx.push(Fx::App(AppFx::Home(out)));
        fx.invalidate(Provenance::Input);
    }

    /// Seat the arrival in the first shelf once it has a channel; until then seat nothing (the
    /// Watchlist's `seat_first`).
    fn seat_first<H: LiveTvLike>(&mut self, fx: &mut Effects<'_, H>) {
        let regions = self.page.regions();
        let want = regions.first_seat();
        let target = want.map(FocusTarget::FirstInGroupAnimated).or_else(|| regions.unseat());
        self.seat = (want != Some(RECENT_GROUP)).then_some(want);
        if let Some(target) = target {
            self.seat_own(target, fx);
        }
    }

    fn follow_seat<H: LiveTvLike>(&mut self, fx: &mut Effects<'_, H>) {
        let Some(seated) = self.seat else { return };
        if seated != self.page.regions().first_seat() {
            self.seat_first(fx);
        }
    }

    fn seat_own<H: LiveTvLike>(&mut self, focus: FocusTarget<u32>, fx: &mut Effects<'_, H>) {
        self.own_enters = self.own_enters.saturating_add(1);
        fx.push(Fx::Deliver(fx.from(), Delivery::Screen(ScreenEvent::Enter(Enter::Fresh { focus }))));
    }

    /// BACK, decided by [`PageRegions::back`]: a scrolled page first returns to its top, on the
    /// first tile; from there it leaves for Home.
    fn back<H: LiveTvLike>(&mut self, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) -> Handled {
        let focus = cx.focus.current.filter(|key| key.entry == self.page.entry);
        let regions = self.page.regions();
        let head = regions.first_seat().unwrap_or(RECENT_GROUP);
        let at = focus.and_then(|key| regions.region_of(key.elem, Focusable::<H>::group_of(self, &key.elem, cx)));
        match regions.back(at, self.stack.target() > 0.5) {
            BackStep::Top => {
                self.seat_own(FocusTarget::FirstInGroupAnimated(head), fx);
            }
            BackStep::Content | BackStep::Header => self.seat_own(FocusTarget::ContainerGroup(head), fx),
            BackStep::Nav => self.seat_own(FocusTarget::ContainerGroup(plx_ui::containers::tabs::STRIP), fx),
            BackStep::Leave => self.route(HomeReq::Tab(HomeTab::Home), fx),
        }
        Handled::Yes
    }

    /// A non-card element committed: the strip's pills and the chip.
    fn activate<H: LiveTvLike>(&mut self, elem: u32, fx: &mut Effects<'_, H>) -> Handled {
        match Self::strip_press(elem) {
            Some(Some(out)) => self.route(out, fx),
            Some(None) => {}
            None => return Handled::No,
        }
        Handled::Yes
    }

    /// A press on the shared strip: the other tabs and the chip are requests; the Live TV pill is
    /// the page the reader is already on.
    fn strip_press(elem: u32) -> Option<Option<HomeReq>> {
        use plx_ui::widgets::{
            TAB_KEY_CHIP, TAB_KEY_HOME, TAB_KEY_LIVETV, TAB_KEY_MOVIES, TAB_KEY_SEARCH, TAB_KEY_SHOWS, TAB_KEY_WATCHLIST,
        };
        match elem {
            TAB_KEY_HOME => Some(Some(HomeReq::Tab(HomeTab::Home))),
            TAB_KEY_MOVIES => Some(Some(HomeReq::Tab(HomeTab::Movies))),
            TAB_KEY_SHOWS => Some(Some(HomeReq::Tab(HomeTab::Shows))),
            TAB_KEY_SEARCH => Some(Some(HomeReq::Tab(HomeTab::Search))),
            TAB_KEY_WATCHLIST => Some(Some(HomeReq::Tab(HomeTab::Watchlist))),
            TAB_KEY_CHIP => Some(Some(HomeReq::Account)),
            TAB_KEY_LIVETV => Some(None),
            _ => None,
        }
    }

    fn view<H: LiveTvLike>(&self) -> impl Focusable<H> + '_ {
        self.stack.view(&self.page)
    }

    /// The page's top edge once the chrome has tucked away (the Watchlist's).
    fn draw_chrome<H: LiveTvLike>(&self, f: &mut DrawFrame<'_, '_, H>) {
        let tuck = self.page.handoff.tuck();
        if tuck <= 0.0 {
            return;
        }
        let ground = self.ground.sample(Rect::FULL, SCR_W * 0.5, 0.0);
        plx_ui::scroll_capsule::draw_top_fade(f.painter.alpha(f.page_alpha), SCR_W, ground, tuck, None);
    }
}

plx_ui::focusable_via_view!(LiveTvScreen, H: [LiveTvLike], view);

impl LogicalState for LiveTvScreen {
    fn write(&self, c: &mut Canon) {
        c.u32(self.page.entry.0);
        c.u32(match self.page.status { LiveTvStatus::Loading => 0, LiveTvStatus::Ready => 1, LiveTvStatus::Empty => 2, LiveTvStatus::Failed => 3 });
        c.seq(self.page.channels.len());
        for (sid, rk) in &self.page.channels { c.u32(u32::from(sid.raw())).str(rk); }
        c.seq(self.page.airings.len());
        for &i in &self.page.airings { c.u32(i as u32); }
        c.seq(self.page.recent.len());
        for &i in &self.page.recent { c.u32(i as u32); }
        self.stack.write(c);
        self.page.handoff.write(c);
    }

    fn probe(&self, out: &mut String) {
        out.push_str(&format!("livetv channels={}", self.page.channels.len()));
    }
}

impl<H: LiveTvLike> Machine<H> for LiveTvScreen {
    type Ev = ScreenEvent<H>;

    fn step(&mut self, ev: &Self::Ev, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) -> Handled {
        match ev {
            ScreenEvent::Tick(_) | ScreenEvent::Enter(Enter::Fresh { .. }) => self.page.sync(H::livetv(cx)),
            ScreenEvent::StoreChanged(ord, _) if *ord == StoreId::LiveTv.ord() => {
                self.page.synced = None;
                self.page.sync(H::livetv(cx));
            }
            _ => {}
        }
        let card = match self.stack.on(&self.page, ev, cx, fx) {
            Some(StackEvent::Card(_, card)) => Some(card),
            _ => None,
        };
        // The reader acting makes the seat theirs (claimed on the event, as the Watchlist does).
        if let ScreenEvent::Input(InputEvent { kind, .. }) = ev {
            if matches!(kind, InputKind::Key { edge: Edge::Down, .. } | InputKind::Pointer { .. }
                | InputKind::Click { .. } | InputKind::Drag { .. } | InputKind::Wheel { .. }) {
                self.seat = None;
                self.stale_enters = self.own_enters;
            }
        }
        match ev {
            ScreenEvent::Tick(t) => {
                self.tick(*t, cx, fx);
                self.follow_seat(fx);
                Handled::Yes
            }
            ScreenEvent::Enter(enter) => {
                match enter {
                    Enter::Fresh { .. } if self.own_enters > 0 => {
                        self.own_enters -= 1;
                        if self.stale_enters > 0 {
                            self.stale_enters -= 1;
                            if let Some(held) = cx.focus.current.filter(|key| key.entry == self.page.entry) {
                                self.own_enters = self.own_enters.saturating_add(1);
                                fx.push(Fx::Deliver(fx.from(), Delivery::Screen(ScreenEvent::Enter(Enter::Fresh {
                                    focus: FocusTarget::Elem(held),
                                }))));
                            }
                        }
                    }
                    Enter::Fresh { .. } => {
                        self.seat_first(fx);
                    }
                    Enter::Restored => {
                        self.seat = None;
                        self.stale_enters = self.own_enters;
                    }
                }
                fx.invalidate(Provenance::Nav);
                Handled::Yes
            }
            ScreenEvent::Uncover => {
                fx.invalidate(Provenance::Nav);
                Handled::Yes
            }
            ScreenEvent::FocusMoved { .. } | ScreenEvent::StoreChanged(..) => {
                fx.invalidate(Provenance::Input);
                Handled::Yes
            }
            ScreenEvent::Activate(elem) => {
                if *elem == RETRY_ELEM && self.page.status == LiveTvStatus::Failed {
                    fx.push(Fx::App(AppFx::Store(StoreId::LiveTv, StoreCmd::LiveTv(LiveTvCmd::Retry))));
                    fx.invalidate(Provenance::Input);
                    Handled::Yes
                } else { self.activate(*elem, fx) }
            },
            ScreenEvent::PressCommit(_) => {
                if let Some(CardEvent::Activate(elem)) = card {
                    if let Some(out) = self.press(elem, cx) {
                        fx.push(Fx::App(AppFx::Content(out)));
                        fx.invalidate(Provenance::Input);
                    }
                } else if let Some(key) = cx.focus.current.filter(|key| key.entry == self.page.entry) {
                    if key.elem == RETRY_ELEM && self.page.status == LiveTvStatus::Failed {
                        fx.push(Fx::App(AppFx::Store(StoreId::LiveTv, StoreCmd::LiveTv(LiveTvCmd::Retry))));
                        fx.invalidate(Provenance::Input);
                    } else { self.activate(key.elem, fx); }
                }
                Handled::Yes
            }
            ScreenEvent::Input(InputEvent { kind: InputKind::Key { key: Key::Back, edge: Edge::Down, .. }, .. }) => {
                self.back(cx, fx)
            }
            _ => Handled::No,
        }
    }
}

impl<H: LiveTvLike> Screen<H> for LiveTvScreen {
    fn focused_card_rect(&self, cx: &Cx<'_, H>, focus: Option<FocusKey<u32>>, at: plx_ui::screen::At) -> Option<Rect> {
        let key = focus.filter(|k| k.entry == self.page.entry && k.elem >= FIRST_CARD_ELEM)?;
        Focusable::<H>::place(self, &key.elem, cx, at).map(|placed| placed.rect)
    }
    fn redraw_focused(&self, f: &mut DrawFrame<'_, '_, H>, focus: Option<FocusKey<u32>>) {
        self.stack.view(&self.page).redraw_focused(f, focus);
    }
    fn opener_departing(&self, key: FocusKey<u32>) -> bool {
        self.stack.view(&self.page).opener_departing::<H>(key)
    }
    fn name(&self) -> &'static str {
        Self::WORD
    }
    fn a11y_ns(&self) -> &'static str {
        Self::WORD
    }
    fn chrome_tuck(&self) -> f32 {
        self.page.handoff.tuck()
    }
    fn state(&self) -> &dyn LogicalState {
        self
    }
    fn crumb(&self, _cx: &Cx<'_, H>) -> Option<std::borrow::Cow<'_, str>> {
        None
    }
    fn prepare(&mut self, _budget: &mut plx_ui::frame::Budget, _cx: &Cx<'_, H>) {}
    fn draw(&mut self, f: &mut DrawFrame<'_, '_, H>) {
        plx_gfx::gfx::frame_clear(theme::CLEAR_RGB.0, theme::CLEAR_RGB.1, theme::CLEAR_RGB.2);
        let p = f.painter.alpha(f.page_alpha);
        self.ground.draw(p, Rect::FULL);
        self.stack.view(&self.page).paint(f);
        self.draw_chrome(f);
    }
    fn render(&self) -> RenderStrategy {
        RenderStrategy::Page
    }
    fn focus_source(&self) -> FocusSource {
        FocusSource::Engine
    }
    fn hit_source(&self) -> HitSource {
        HitSource::Engine
    }
    fn links(&self, out: &mut Vec<Link>) {
        self.page.regions().links(out);
    }
    fn holds_arrival(&self, dir: plx_ui::screen::Dir) -> bool {
        self.page.regions().holds(dir)
    }
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}

#[cfg(test)]
#[path = "livetv_tests.rs"]
mod tests;
