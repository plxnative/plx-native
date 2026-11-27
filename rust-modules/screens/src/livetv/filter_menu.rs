//! **Live TV Browse's filter menu** (PR7; handoff `SPEC-6`): the surface the filter control opens. A `Style::Compact` panel
//! presented over the page, a glass `Ground::Menu` [`TableView`] of the four filters that MORPHS out of the control
//! (`Popover::morphing` + `open_from`, the path every header menu takes), the one the checked row marks. It is the Watchlist's
//! Sort & Filter menu's shape (`watchlist::menu`): every row is a focus stop keyed by its position in [`Filter::ALL`], the
//! control that opened it is the host's opener lift (`Screen::opener`), and BACK or a pick drains the glass back into it.
//!
//! **A pick is REPORTED, not applied**: OK on a row delivers [`AppMsg::LiveTvFilterPicked`] to the Live TV instance named
//! on the argument and closes the menu. The page owns its filter (`Scope::Browse(Filter)`) and changes it on that message
//! alone, so the filter on show and the set under it cannot disagree. OK on the row that is already checked closes the menu and
//! reports nothing the page would act on.
use std::borrow::Cow;

use plx_machine::machine::{
    Canon, Cx, Delivery, Edge, Effects, EntryId, FocusKey, Fx, GroupId, Handled, InputKind, InstanceId, Key,
    LogicalState, Machine, MachineId, NavOp,
};
use plx_ui::a11y::{A11y, A11yPath, Role};
use plx_ui::frame::Budget;
use plx_ui::popover::Popover;
use plx_ui::screen::{
    Activate, At, AxisMask, Dir, DrawFrame, EdgeRule, ElemKind, Focusable, GroupKind, GroupSpec, Hover, Placed,
    RenderStrategy, Screen, ScreenEvent, Scrim, Seat, Step, Stop,
};
use plx_ui::table::{Row, Section, TableView};
use plx_ui::widgets::Ground;
use plx_ui::{Painter, Rect};

use super::browse::Filter;
use crate::registry::{AppLike, AppMsg, PageMemory};

pub const SHAPE: &str =
    "LiveTvFilter{arg:LiveTvFilterArg{host:u32,chosen:u32,anchor:[u32;4],opener:{entry:u32,elem:u32}},sel:u32,table:TableViewMotion}";

const GROUP: GroupId = GroupId(0);

/// The panel's corner radius: `--radius-panel`, as the Watchlist's menu and the account menu pin it.
const PANEL_RAD: f32 = 24.0;

/// **What the container is asked to present**: the Live TV instance the pick is reported to, the filter in force (the row
/// the menu opens on, wearing the check mark), the drawn rect of the control it grows out of (bit-preserving, so a canonical
/// argument holds it without float equality) and that control: the page's entry and its element, which the page leaves out of its
/// own pass and the opener lift draws at the morph's alpha.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LiveTvFilterArg {
    pub host: InstanceId,
    pub chosen: Filter,
    pub anchor: [u32; 4],
    pub opener: (EntryId, u32),
}

impl LogicalState for LiveTvFilterArg {
    fn write(&self, c: &mut Canon) {
        c.u32(self.host.0).u32(self.chosen.code());
        for value in self.anchor {
            c.u32(value);
        }
        c.u32(self.opener.0 .0).u32(self.opener.1);
    }
    fn probe(&self, out: &mut String) {
        out.push_str("livetv_filter_arg");
    }
}

/// The menu's rows, the filter in force checked.
pub fn sections(chosen: Filter) -> Vec<Section> {
    let mut section = Section::new("");
    for filter in Filter::ALL {
        section = section.row(Row::new(super::filter_label(filter)).checked(filter == chosen));
    }
    vec![section]
}

/// The row's accessibility name (`livetv_filter.<name>`): what it chooses, never its place.
const fn a11y_name(filter: Filter) -> &'static str {
    match filter {
        Filter::OnNow => "on_now",
        Filter::JustStarted => "just_started",
        Filter::EndingSoon => "ending_soon",
        Filter::LaterToday => "later_today",
    }
}

pub struct LiveTvFilterScreen {
    entry: EntryId,
    arg: LiveTvFilterArg,
    popover: Popover,
    table: TableView,
    /// The highlighted row (an index into [`Filter::ALL`]).
    sel: usize,
}

impl LiveTvFilterScreen {
    pub fn new(entry: EntryId, arg: LiveTvFilterArg) -> Self {
        let mut table = TableView::new();
        // a menu of short choices: BODY regular labels, as the Library's sort menu does
        table.compact = true;
        let sel = arg.chosen.code() as usize;
        Self {
            entry,
            arg,
            // the control's focused face is the accent pill: the close's blob whitens toward it
            popover: Popover::new().morphing().opener_fill(plx_ui::page_header::OPENER_FILL),
            table,
            sel,
        }
    }

    fn pill(&self) -> Rect {
        let [x, y, w, h] = self.arg.anchor.map(f32::from_bits);
        Rect::new(x, y, w, h)
    }

    fn key(&self, row: usize) -> FocusKey<u32> {
        FocusKey { entry: self.entry, elem: row as u32 }
    }

    fn row_of(&self, elem: u32) -> Option<usize> {
        ((elem as usize) < Filter::ALL.len()).then_some(elem as usize)
    }

    fn select(&mut self, row: usize) {
        self.sel = row;
        self.table.sel = i32::try_from(row).unwrap_or(-1);
    }

    /// The settled panel: laid over the control (the shared rule, [`plx_ui::popover::cover_opener`]), as wide as its longest
    /// label and never narrower than the control, held inside the safe frame.
    fn frame(&self, measure: &dyn plx_machine::machine::Measure) -> Rect {
        let safe = plx_ui::consts::SAFE;
        let w = self.table.menu_panel_width_from(measure, self.pill());
        let h = self.table.measured_height().min(safe.h);
        plx_ui::popover::cover_opener(self.pill(), (w, h), safe)
    }

    fn row_frame(&self, row: usize, measure: &dyn plx_machine::machine::Measure) -> Option<Rect> {
        self.table.row_frame(self.frame(measure), i32::try_from(row).ok()?)
    }

    /// Close; and report the row's filter unless it is the one in force.
    fn pick<H: AppLike<Memory = PageMemory>>(&mut self, elem: u32, fx: &mut Effects<'_, H>) {
        let Some(row) = self.row_of(elem) else { return };
        self.select(row);
        self.popover.dismiss();
        fx.push(Fx::Nav(NavOp::Dismiss(self.entry)));
        if let Some(filter) = Filter::ALL.get(row).copied().filter(|filter| *filter != self.arg.chosen) {
            fx.push(Fx::Deliver(
                MachineId::Instance(self.arg.host),
                Delivery::Screen(ScreenEvent::App(AppMsg::LiveTvFilterPicked(filter))),
            ));
        }
    }
}

impl<H: AppLike<Memory = PageMemory>> Machine<H> for LiveTvFilterScreen {
    type Ev = ScreenEvent<H>;

    fn step(&mut self, ev: &Self::Ev, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) -> Handled {
        match ev {
            ScreenEvent::Mount => {
                self.table.open_sections(sections(self.arg.chosen));
                self.select(self.sel);
                self.popover.open_from(Some(self.pill()));
            }
            // however the surface was dismissed, the glass runs the morph back into the control
            ScreenEvent::Closing => self.popover.dismiss(),
            ScreenEvent::Tick(tick) => {
                if let Some(row) = cx.focus.current.filter(|key| key.entry == self.entry).and_then(|key| self.row_of(key.elem)) {
                    self.select(row);
                }
                self.popover.update(tick.dt());
                self.table.update(tick.dt(), self.frame(cx.measure).h);
            }
            ScreenEvent::FocusMoved { to, .. } => {
                if let Some(row) = self.row_of(to.elem) {
                    self.select(row);
                }
            }
            ScreenEvent::Activate(elem) => self.pick(*elem, fx),
            ScreenEvent::PressCommit(_) => {
                if let Some(key) = cx.focus.current.filter(|key| key.entry == self.entry) {
                    self.pick(key.elem, fx);
                }
            }
            ScreenEvent::Input(input) => {
                if let InputKind::Key { key: Key::Back, edge: Edge::Down, .. } = input.kind {
                    self.popover.dismiss();
                    fx.push(Fx::Nav(NavOp::Dismiss(self.entry)));
                    return Handled::Yes;
                }
            }
            _ => {}
        }
        Handled::No
    }
}

impl<H: AppLike<Memory = PageMemory>> Focusable<H> for LiveTvFilterScreen {
    fn groups(&self, cx: &Cx<'_, H>, out: &mut Vec<GroupSpec>) {
        out.push(GroupSpec {
            id: GROUP,
            kind: GroupKind::Column,
            seat: Seat::Remembered,
            reachable: AxisMask::BOTH,
            edge: [EdgeRule::Stop; 4],
            extent: self.frame(cx.measure),
            len: Filter::ALL.len(),
            elem: ElemKind::Bare,
        });
    }
    fn group_of(&self, elem: &u32, _: &Cx<'_, H>) -> Option<GroupId> {
        self.row_of(*elem).map(|_| GROUP)
    }
    fn neighbour(&self, key: FocusKey<u32>, dir: Dir, _: &Cx<'_, H>) -> Step<u32> {
        let Some(row) = self.row_of(key.elem) else { return Step::Edge };
        let next = match dir {
            Dir::Up => row.checked_sub(1),
            Dir::Down => (row + 1 < Filter::ALL.len()).then_some(row + 1),
            _ => None,
        };
        next.map_or(Step::Edge, |next| Step::Move(self.key(next)))
    }
    fn place(&self, elem: &u32, cx: &Cx<'_, H>, _: At) -> Option<Placed> {
        let row = self.row_of(*elem)?;
        let rect = self.row_frame(row, cx.measure)?;
        Some(Placed { rect, rest_rect: rect, clip: self.frame(cx.measure), index: Some(row as u32) })
    }
    fn reconcile(&self, want: FocusKey<u32>, cx: &Cx<'_, H>) -> FocusKey<u32> {
        if self.group_of(&want.elem, cx).is_some() { want } else { self.key(self.sel) }
    }
    fn seat(&self, _: GroupId, _: Placed, _: &Cx<'_, H>) -> FocusKey<u32> {
        self.key(self.sel)
    }
    /// `livetv_filter.<name>`: each row is named by the filter it chooses, labelled with its drawn words.
    fn a11y(&self, key: &u32, _: &Cx<'_, H>) -> Option<A11y<'_>> {
        let filter = *Filter::ALL.get(self.row_of(*key)?)?;
        Some(A11y::new(A11yPath::new().name(a11y_name(filter)), Role::Row).with_label(super::filter_label(filter)))
    }
    fn a11y_elems(&self, g: GroupId, _: &Cx<'_, H>, out: &mut Vec<u32>) {
        if g == GROUP {
            out.extend((0..Filter::ALL.len()).map(|row| row as u32));
        }
    }
}

impl<H: AppLike<Memory = PageMemory>> Screen<H> for LiveTvFilterScreen {
    /// The control the panel grew out of: the page leaves it out of its pass and the host's opener lift is its only copy, lit
    /// above the scrim and the panel at the morph's `opener_alpha`.
    fn opener(&self) -> Option<(EntryId, Option<FocusKey<u32>>)> {
        Some((self.arg.opener.0, Some(FocusKey { entry: self.arg.opener.0, elem: self.arg.opener.1 })))
    }
    fn opener_alpha(&self) -> f32 {
        self.popover.opener_alpha()
    }
    /// The droplet has drained into the control: the surface, and the lift that holds the control above it, end on this frame.
    fn handed_back(&self) -> bool {
        self.popover.handed_back()
    }
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }
    fn name(&self) -> &'static str {
        "livetv_filter"
    }
    fn state(&self) -> &dyn LogicalState {
        self
    }
    fn crumb(&self, _: &Cx<'_, H>) -> Option<Cow<'_, str>> {
        None
    }
    /// The COMPACT role (DS6's 30% scrim): a chip-shaped control on a live page opening a list.
    fn scrim(&self) -> Scrim {
        self.popover.dim(plx_ui::theme::underlay::DIM_COMPACT)
    }
    fn prepare(&mut self, _: &mut Budget, _: &Cx<'_, H>) {}
    /// The panel, growing out of the control's rect; not faded by the container's appear (the morph is its own entrance).
    fn draw(&mut self, f: &mut DrawFrame<'_, '_, H>) {
        let p = f.painter.alpha(f.page_alpha);
        let panel = self.frame(f.measure);
        let base: Painter = f.painter.alpha(f.nav_page_alpha);
        let glass = self.popover.content_painter_on(base, 0.0);
        self.popover.panel(glass, panel, PANEL_RAD, f.underlay, Ground::Menu);
        self.popover.morph_rows(glass, panel, PANEL_RAD, |p| self.table.draw(p, panel, f.measure));
        for row in 0..Filter::ALL.len() {
            let Some(rect) = self.row_frame(row, f.measure) else { continue };
            f.stop(
                p,
                Stop { key: self.key(row), rect, rest_rect: rect, clip: panel, hover: Hover::Focus, activate: Activate::Immediate },
            );
        }
    }
    fn render(&self) -> RenderStrategy {
        RenderStrategy::Page
    }
}

impl LogicalState for LiveTvFilterScreen {
    fn write(&self, c: &mut Canon) {
        self.arg.write(c);
        c.u32(self.sel as u32);
        self.table.write_motion(c);
    }
    fn probe(&self, out: &mut String) {
        out.push_str(&format!("livetv_filter sel={}", self.sel));
    }
}

#[cfg(test)]
#[path = "filter_menu_tests.rs"]
pub(crate) mod tests;
