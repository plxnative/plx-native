//! **Settings > Libraries > Library order**: the order of the favorite libraries on Home and in the
//! Library header, moved one library at a time.
//!
//! The page lists the favourites in the profile's order (the Browse publication's
//! `DirectoryView::library_order`) and a Reset row. OK on a library PICKS IT UP ([`Mode::Moving`]):
//! Up and Down carry it through the list (the table glides the rows, `FormTable`), OK puts it down
//! (one `BrowseCmd::SetLibraryOrder` with the whole shown order), Back puts it back and writes
//! nothing. OK on Reset asks once more ([`Mode::ConfirmReset`]); a second OK resets
//! (`BrowseCmd::ResetLibraryOrder`), any other key keeps the order. Left and Right are swallowed
//! while a library is held (`TableScreen::uncommitted`), so a stray press cannot leave the page with
//! the order half-moved.
//!
//! **One lifecycle, one enum.** [`Mode`] is everything the page is doing: nothing, holding a library,
//! asking whether to reset, and the two sentences that follow a finished action. The sentence beside
//! the table is a pure function of the mode and the focused row ([`PageState::description`]); the
//! rows' move marks are a function of the mode ([`move_of`]); nothing else keeps a flag.
//!
//! **What the store says and what the page shows are two things.** The page reads the publication
//! through ONE adapter ([`Published`]) and keeps the order it SHOWS until the store publishes
//! something different from what it last read: after a drop the page does not snap back to the
//! stale publication while the write is in flight. Reset is declared `inert` while the profile has
//! no record (`OrderState::is_recorded`), so focus skips it; it stays offered while a failed write
//! leaves a record standing, and the failed write says so in words ([`PageState::description`]).
//!
//! The picture under the sentence is the header preview ([`Preview`]): the real Library header's
//! names row for the kind of the library in view, laid out by `plx_ui::page_header::names_row` against
//! the controls the real header passes ([`crate::registry::header_controls_at_rest`]), so the ⋯ and
//! its note appear exactly when the real header would fold names behind it.

use super::*;
use plx_data::browse::{LibraryOrderRow, OrderState, SecKind, WriteState};
use plx_data::stores::browse::{BrowseCmd, DirectoryView};
use plx_data::stores::{StoreCmd, StoreId};
use plx_machine::machine::{Edge, InputEvent, InputKind};
use plx_ui::follow_copy::{FollowCopy, PictureSlot};
use plx_ui::page_header::{NamesRow, PageHeader};
use plx_ui::table::RowMove;
use plx_ui::widgets::{CtlPop, TabStrip};

// ---------------------------------------------------------------------------------------------
// the adapter: every read of the Library order publication
// ---------------------------------------------------------------------------------------------

/// **What the page reads of the Browse publication, in one place.** The rows are the favourites in
/// the profile's order, `state` is what the profile's record is (Reset is offered while it is
/// recorded) and `write` where the last write stands (`Failed`: the order applies until restart).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Published {
    rows: Vec<LibraryOrderRow>,
    state: OrderState,
    write: WriteState,
}

impl Published {
    fn read(directory: DirectoryView<'_>) -> Self {
        Self { rows: rows_of(directory).to_vec(), state: directory.library_order_state(), write: directory.library_order_write() }
    }
    /// Does `directory` publish exactly this (no clone, for the per-frame question)?
    fn is_current(&self, directory: DirectoryView<'_>) -> bool {
        self.rows == rows_of(directory)
            && self.state == directory.library_order_state()
            && self.write == directory.library_order_write()
    }
    fn reset_offered(&self) -> bool {
        self.state.is_recorded()
    }
    fn write_failed(&self) -> bool {
        self.write == WriteState::Failed
    }
}

/// The favourites in the profile's order, from the publication.
pub(super) fn rows_of<'a>(directory: DirectoryView<'a>) -> &'a [LibraryOrderRow] {
    directory.library_order()
}

/// **The Settings root's value for the Library order row**: "Server order" unless a saved order is in
/// effect, else the library it puts first ("Anime first").
pub(super) fn root_value(directory: DirectoryView<'_>) -> String {
    use plx_os::i18n::msg;
    match (directory.library_order_state().in_effect(), rows_of(directory).first()) {
        (true, Some(first)) => msg::library_order_value_first(&first.title),
        _ => msg::library_order_value_server().to_string(),
    }
}

/// The command a drop sends: the WHOLE shown order, by `(machine id, section key)`. A library whose
/// server's machine id the registry has not learned yet cannot be named in a record and is left out
/// (it falls where the server order puts it).
fn order_command(shown: &[LibraryOrderRow]) -> BrowseCmd {
    BrowseCmd::SetLibraryOrder(
        shown.iter().filter(|row| !row.machine_id.is_empty()).map(|row| (row.machine_id.clone(), row.key)).collect(),
    )
}

// ---------------------------------------------------------------------------------------------
// the rows
// ---------------------------------------------------------------------------------------------

/// A row's identity: a library by the table index it opens (identity inside one table epoch), or Reset.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum LibId {
    Lib(usize),
    Reset,
}

/// The first focus key of a library row: clear of Reset's.
const LIB_KEY_BASE: u32 = 100;

impl FormId for LibId {
    fn key(&self) -> RowKey {
        match self {
            LibId::Reset => RowKey(1),
            LibId::Lib(section) => RowKey(LIB_KEY_BASE + *section as u32),
        }
    }
    fn a11y(&self) -> Option<&'static str> {
        match self {
            LibId::Reset => Some("reset"),
            // one row per library: the position names each (`row.<i>`)
            LibId::Lib(_) => None,
        }
    }
}

/// **What the page is doing**: the one lifecycle (no flag beside it).
#[derive(Clone, Debug, PartialEq, Eq)]
enum Mode {
    /// Nothing held: the rows read as pickable, the sentence is the focused row's.
    Idle,
    /// `lifted` (a library, by its table index) is held; `before` is the shown order it was picked
    /// up from, which Back restores and a drop that moved nothing is compared with.
    Moving { lifted: usize, before: Vec<LibraryOrderRow> },
    /// OK was pressed on Reset: a second OK resets, any other key keeps the order.
    ConfirmReset,
    /// `.0` (a library) was just put down: the page says where it is now, until the next key.
    Dropped(usize),
    /// The order was just reset: the page says so, until the next key.
    ResetDone,
}

/// A library row's part in the reorder, from the mode ([`RowMove`] is the table's one language for it).
fn move_of(mode: &Mode, index: usize, count: usize, section: usize) -> RowMove {
    let (can_up, can_down) = (index > 0, index + 1 < count);
    match mode {
        Mode::Moving { lifted, .. } if *lifted == section => RowMove::Lifted { can_up, can_down },
        Mode::Moving { .. } => RowMove::Waiting,
        _ => RowMove::Pickable { can_up, can_down },
    }
}

/// The page's rows: the shown favourites, then Reset (inert while there is no record to delete, and
/// while a library is held, when it is no action at all).
fn order_form(shown: &[LibraryOrderRow], mode: &Mode, reset_offered: bool) -> Form<LibId, (), SettingsPage> {
    use plx_os::i18n::msg;
    let mut libraries = FormSection::new(msg::settings_libraries_title()).visible(!shown.is_empty());
    for (index, row) in shown.iter().enumerate() {
        let label = Row::new(&row.title).moving(move_of(mode, index, shown.len(), row.section));
        let label = if row.handle.is_empty() { label } else { label.value(&row.handle) };
        libraries = libraries.item(LibId::Lib(row.section), RowKind::Button, (), label);
    }
    let reset = FormSection::new("")
        .item(LibId::Reset, RowKind::Button, (), Row::new(msg::library_order_reset()))
        .inert(!reset_offered || matches!(mode, Mode::Moving { .. }));
    Form::new().section(libraries).section(reset)
}

/// The plural kind word of a library's header ("Movies", "TV Shows").
fn kind_word(kind: SecKind) -> &'static str {
    match kind {
        SecKind::Movie => plx_os::i18n::msg::browse_kind_movies(),
        SecKind::Show => plx_os::i18n::msg::browse_kind_tv_shows(),
    }
}

// ---------------------------------------------------------------------------------------------
// the page state
// ---------------------------------------------------------------------------------------------

struct PageState {
    mode: Mode,
    /// The focused row's key — an identity, not a position.
    sel: RowKey,
    /// The order the page SHOWS: the publication's until a drop, then the dropped order until the
    /// store publishes something else.
    shown: Vec<LibraryOrderRow>,
    /// The last publication the page took its rows from.
    seen: Published,
}

impl PageState {
    fn position(&self, section: usize) -> Option<usize> {
        self.shown.iter().position(|row| row.section == section)
    }

    /// The library the focus is on, `None` on Reset.
    fn focused(&self) -> Option<&LibraryOrderRow> {
        self.shown.iter().find(|row| LibId::Lib(row.section).key() == self.sel)
    }

    /// **The library the preview follows**: the one held, else the one just put down, else the one in
    /// focus; with Reset in focus, the last library.
    fn previewed(&self) -> Option<&LibraryOrderRow> {
        let section = match &self.mode {
            Mode::Moving { lifted, .. } => Some(*lifted),
            Mode::Dropped(section) => Some(*section),
            _ => self.focused().map(|row| row.section),
        };
        section.and_then(|s| self.shown.iter().find(|row| row.section == s)).or_else(|| self.shown.last())
    }

    /// **The sentence beside the table**: a pure function of the mode, the focused row and (for the
    /// one thing it adds) whether the last write failed.
    fn description(&self) -> String {
        use plx_os::i18n::{msg, ordinal};
        let row_of = |section: usize| self.shown.iter().find(|row| row.section == section);
        let said = match &self.mode {
            Mode::ConfirmReset => return msg::library_order_state_confirm().to_string(),
            Mode::Moving { lifted, .. } => {
                return row_of(*lifted).map_or_else(String::new, |row| msg::library_order_state_moving(&row.title));
            }
            Mode::ResetDone => msg::library_order_state_reset_done().to_string(),
            Mode::Dropped(section) => match (row_of(*section), self.position(*section)) {
                (Some(row), Some(at)) => {
                    let place = self.shown[..at].iter().filter(|other| other.kind == row.kind).count();
                    msg::library_order_state_dropped(&row.title, &ordinal(at + 1), &ordinal(place + 1), kind_word(row.kind))
                }
                _ => String::new(),
            },
            Mode::Idle => match self.focused() {
                Some(row) => msg::library_order_state_idle(&row.title, kind_word(row.kind)),
                None => msg::library_order_state_reset().to_string(),
            },
        };
        if self.seen.write_failed() { format!("{said} {}", msg::library_order_state_unsaved()) } else { said }
    }
}

impl LogicalState for PageState {
    fn write(&self, w: &mut Canon) {
        w.u32(self.sel.0);
        match &self.mode {
            Mode::Idle => { w.u8(0); }
            Mode::Moving { lifted, before } => {
                w.u8(1).u32(*lifted as u32).seq(before.len());
                for row in before { w.u32(row.section as u32); }
            }
            Mode::ConfirmReset => { w.u8(2); }
            Mode::Dropped(section) => { w.u8(3).u32(*section as u32); }
            Mode::ResetDone => { w.u8(4); }
        }
        w.seq(self.shown.len());
        for row in &self.shown { w.u32(row.section as u32); }
        w.bool(self.seen.reset_offered()).bool(self.seen.write_failed());
    }
    fn probe(&self, out: &mut String) {
        let order: Vec<usize> = self.shown.iter().map(|row| row.section).collect();
        out.push_str(&format!(
            "library_order sel={} mode={} order={order:?} recorded={} write_failed={}",
            self.sel.0,
            match &self.mode {
                Mode::Idle => "idle".to_string(),
                Mode::Moving { lifted, .. } => format!("moving({lifted})"),
                Mode::ConfirmReset => "confirm-reset".to_string(),
                Mode::Dropped(section) => format!("dropped({section})"),
                Mode::ResetDone => "reset-done".to_string(),
            },
            self.seen.reset_offered(), self.seen.write_failed(),
        ));
    }
}

// ---------------------------------------------------------------------------------------------
// the preview
// ---------------------------------------------------------------------------------------------

/// **The Library header's names row for one kind, as a picture.** Built from the kind's favourites in
/// the order shown (title and owner as the real header words them), laid out by the shared
/// `names_row` against the controls the real header passes, so it folds the names the real header
/// folds; the held [`TabStrip`] glides the plate between libraries as focus moves.
pub(super) struct Preview {
    /// What `row` was built from: the pills' labels and which one is plated.
    built: Option<(Vec<(String, usize)>, usize)>,
    kind: SecKind,
    row: Option<NamesRow<usize>>,
    strip: TabStrip,
    pop: CtlPop<1>,
}

impl Preview {
    pub(super) fn new() -> Self {
        Self { built: None, kind: SecKind::Movie, row: None, strip: TabStrip::new(), pop: CtlPop::new() }
    }

    /// Show `of` (a library among `rows`, the shown order) in the header of its kind. A change of
    /// pills or plate rebuilds the row; the same ones keep it.
    pub(super) fn sync(&mut self, rows: &[LibraryOrderRow], of: Option<&LibraryOrderRow>, measure: &dyn plx_machine::machine::Measure) {
        let Some(of) = of else {
            self.built = None;
            self.row = None;
            return;
        };
        let pills: Vec<(String, usize)> = rows.iter().filter(|row| row.kind == of.kind)
            .map(|row| (crate::registry::joined_name(&row.title, &row.handle), row.section)).collect();
        let selected = pills.iter().position(|(_, section)| *section == of.section).unwrap_or(0);
        if self.built.as_ref().is_some_and(|(held, at)| *held == pills && *at == selected) {
            return;
        }
        let (right, widest) = crate::registry::header_controls_at_rest(measure);
        self.row = Some(plx_ui::page_header::names_row(&pills, selected, right, &widest, crate::registry::MAX_LIBRARY_PILLS, measure));
        self.built = Some((pills, selected));
        self.kind = of.kind;
    }

    /// One frame of the plate's motion.
    pub(super) fn step(&mut self, dt: f32) {
        if let Some(row) = &self.row {
            let selected = row.page.selected(&row.layout);
            PageHeader::step(&row.layout, selected, -1, &mut self.strip, &mut self.pop, dt);
        }
    }

    /// Did the row the page would draw fold any name behind the ⋯?
    #[cfg(test)]
    pub(super) fn overflowed(&self) -> bool {
        self.row.as_ref().is_some_and(NamesRow::overflowed)
    }

    /// The labels of the names the row draws, in order (the ⋯ has none).
    #[cfg(test)]
    pub(super) fn shown_names(&self) -> Vec<String> {
        self.row.as_ref().map_or_else(Vec::new, |row| {
            row.page.spec().left.iter().filter(|item| item.glyph.is_none()).map(|item| item.label.clone()).collect()
        })
    }

    #[cfg(test)]
    pub(super) fn plated(&self) -> Option<String> {
        self.row.as_ref().and_then(|row| row.page.spec().left.iter().find(|item| item.selected_plate).map(|item| item.label.clone()))
    }

    pub(super) fn slot(&self) -> Option<PreviewSlot<'_>> {
        self.row.as_ref().map(|row| PreviewSlot { row, strip: &self.strip, kind: self.kind })
    }
}

/// The preview drawn into the picture slot: the kind's caption, the names row, and (only when the
/// real header would fold names) the note about the ⋯.
pub(super) struct PreviewSlot<'a> {
    row: &'a NamesRow<usize>,
    strip: &'a TabStrip,
    kind: SecKind,
}

/// The names row's top under the slot's caption (the mockup: a 24 px caption, then `--space-md`).
const CAPTION_H: f32 = 24.0;

impl PictureSlot for PreviewSlot<'_> {
    fn draw(&self, p: Painter, slot: Rect, measure: &dyn plx_machine::machine::Measure) {
        use plx_os::i18n::msg;
        use plx_ui::label::HAlign;
        use plx_ui::text_view::TextView;
        let size = theme::size::CAPTION;
        let caption_cap_top = plx_gfx::text::css_cap_drop(size, CAPTION_H, measure.cap_h(size));
        plx_ui::table::draw_caps_label_aligned(
            p, Rect::new(slot.x, slot.y + caption_cap_top, slot.w, plx_ui::table::caps_band_h()),
            &msg::library_order_preview_caption(kind_word(self.kind)), slot.w,
            theme::TEXT_TERTIARY, plx_ui::table::BOARD_CAPS_TRACK_PX, HAlign::Left, measure,
        );
        // the row stands where the real header stands: its layout is absolute, so slide it under the caption
        let layout = &self.row.layout;
        let Some(first) = layout.items.first() else { return };
        let row_top = slot.y + CAPTION_H + theme::space::MD;
        let moved = p.translate(slot.x - first.plate.x, row_top - layout.top());
        self.row.page.draw(moved, layout, self.strip, 1.0, None);
        if self.row.overflowed() {
            let note = msg::library_order_preview_more();
            let top = row_top + plx_ui::page_header::HEIGHT + theme::space::MD;
            TextView::new(note, size, theme::TEXT_TERTIARY).with_measure(measure).h(HAlign::Left).leading(32.0).max_lines(2)
                .draw(p, Rect::new(slot.x, top, slot.w, 64.0));
        }
    }
}

// ---------------------------------------------------------------------------------------------
// the page
// ---------------------------------------------------------------------------------------------

pub(super) struct LibraryOrderPage {
    entry: EntryId,
    form: FormTable<LibId, (), SettingsPage>,
    state: PageState,
    /// The sentence beside the table, which fades to the mode's and the focused row's.
    follow: FollowCopy,
    preview: Preview,
}

impl LibraryOrderPage {
    pub(super) fn new(entry: EntryId, directory: DirectoryView<'_>) -> Self {
        let seen = Published::read(directory);
        let shown = seen.rows.clone();
        let initial = shown.first().map_or(LibId::Reset, |row| LibId::Lib(row.section));
        let mut page = Self {
            entry,
            form: FormTable::page(super::super::registry::BAND),
            state: PageState { mode: Mode::Idle, sel: initial.key(), shown, seen },
            follow: FollowCopy::new(""),
            preview: Preview::new(),
        };
        page.form.open(order_form(&page.state.shown, &page.state.mode, page.state.seen.reset_offered()), Some(&initial));
        page.form.table.list_focused = true;
        page.follow = FollowCopy::new(&page.state.description());
        page
    }

    /// Re-derive the rows from the mode and the shown order, keeping the cursor on its row by
    /// identity (the held library's while it is held), and the sentence with them.
    fn rebuild(&mut self) {
        let keep = match &self.state.mode {
            Mode::Moving { lifted, .. } => Some(LibId::Lib(*lifted)),
            _ => self.form.selected_id().copied(),
        };
        self.form.set_sliding(order_form(&self.state.shown, &self.state.mode, self.state.seen.reset_offered()), keep.as_ref());
        self.form.table.list_focused = true;
        if let Some(key) = self.form.key_at(self.form.table.sel.max(0) as usize) {
            self.state.sel = key;
        }
        self.follow.follow(&self.state.description());
    }

    fn view(&self) -> TableScreen<'_> {
        use plx_os::i18n::msg;
        TableScreen::new(
            Header::new(RouteLayout::screen(), Some(msg::settings_title()), msg::library_order_title(), msg::library_order_row_detail())
                .with_follow(&self.follow),
            &self.form.table, GroupId(0), self.entry,
        ).keyed(&self.form).uncommitted(matches!(self.state.mode, Mode::Moving { .. }))
    }

    /// Take a changed publication in: the order shown follows it (never while a library is held).
    fn adopt(&mut self, directory: DirectoryView<'_>) -> bool {
        if matches!(self.state.mode, Mode::Moving { .. }) || self.state.seen.is_current(directory) {
            return false;
        }
        self.state.seen = Published::read(directory);
        self.state.shown = self.state.seen.rows.clone();
        self.rebuild();
        true
    }

    fn set_mode(&mut self, mode: Mode) {
        self.state.mode = mode;
        self.rebuild();
    }

    /// OK (or a click) on a row, by what the page is doing.
    fn activate(&mut self, key: u32, fx: &mut Effects<'_, InnerHost>) {
        // an inert Reset (nothing recorded, or a library held) answers nothing
        if form_activate(&self.form, key, fx).is_none() {
            return;
        }
        let Some(id) = self.form.index_of_key(RowKey(key)).and_then(|i| self.form.id_at(i)).copied() else { return };
        match (&self.state.mode, id) {
            (Mode::Moving { .. }, _) => self.drop_held(fx),
            (Mode::ConfirmReset, LibId::Reset) => self.reset(fx),
            (Mode::ConfirmReset, _) => self.set_mode(Mode::Idle),
            (_, LibId::Lib(section)) => {
                let before = self.state.shown.clone();
                self.set_mode(Mode::Moving { lifted: section, before });
            }
            (_, LibId::Reset) => self.set_mode(Mode::ConfirmReset),
        }
    }

    /// Put the held library down: one write of the whole shown order, unless nothing moved.
    fn drop_held(&mut self, fx: &mut Effects<'_, InnerHost>) {
        let Mode::Moving { lifted, before } = std::mem::replace(&mut self.state.mode, Mode::Idle) else { return };
        if before != self.state.shown {
            fx.push(Fx::App(AppFx::Store(StoreId::Browse, StoreCmd::Browse(order_command(&self.state.shown)))));
        }
        self.set_mode(Mode::Dropped(lifted));
    }

    /// Put the held library back where it was picked up from and write nothing.
    fn restore_held(&mut self) {
        let Mode::Moving { before, .. } = std::mem::replace(&mut self.state.mode, Mode::Idle) else { return };
        self.state.shown = before;
        self.rebuild();
    }

    fn reset(&mut self, fx: &mut Effects<'_, InnerHost>) {
        fx.push(Fx::App(AppFx::Store(StoreId::Browse, StoreCmd::Browse(BrowseCmd::ResetLibraryOrder))));
        self.set_mode(Mode::ResetDone);
    }

    /// Move the held library one place (`-1` up, `1` down); at either end nothing moves.
    fn carry(&mut self, delta: isize) {
        let Mode::Moving { lifted, .. } = self.state.mode else { return };
        let Some(at) = self.state.position(lifted) else { return };
        let Some(to) = at.checked_add_signed(delta).filter(|to| *to < self.state.shown.len()) else { return };
        self.state.shown.swap(at, to);
        self.rebuild();
    }

    /// **The page's own answer to a key**, before the focus engine sees it. `Handled::Yes` keeps the
    /// engine (and the surface's Back) from acting on it.
    fn on_key(&mut self, key: Key, edge: Edge) -> Handled {
        match &self.state.mode {
            Mode::Moving { .. } => match key {
                Key::Up => { self.carry(-1); Handled::Yes }
                Key::Down => { self.carry(1); Handled::Yes }
                Key::Left | Key::Right => Handled::Yes,
                Key::Back if edge == Edge::Down => { self.restore_held(); Handled::Yes }
                Key::Back => Handled::Yes,
                _ => Handled::No,
            },
            // OK goes on to the press that activates Reset (the second OK); anything else keeps the order
            Mode::ConfirmReset if key == Key::Ok => Handled::No,
            Mode::ConfirmReset => {
                self.set_mode(Mode::Idle);
                if matches!(key, Key::Up | Key::Down | Key::Left | Key::Right | Key::Back) { Handled::Yes } else { Handled::No }
            }
            // the sentence about what just happened lasts until the next key
            Mode::Dropped(_) | Mode::ResetDone => {
                self.set_mode(Mode::Idle);
                Handled::No
            }
            Mode::Idle => Handled::No,
        }
    }
}

impl Machine<InnerHost> for LibraryOrderPage {
    type Ev = ScreenEvent<InnerHost>;
    fn step(&mut self, ev: &Self::Ev, cx: &Cx<'_, InnerHost>, fx: &mut Effects<'_, InnerHost>) -> Handled {
        match ev {
            ScreenEvent::Mount => {
                self.preview.sync(&self.state.shown, self.state.previewed(), cx.measure);
                fx.push(Fx::Deliver(
                    MachineId::Instance(InstanceId(0)),
                    Delivery::Screen(ScreenEvent::Enter(Enter::Fresh {
                        focus: FocusTarget::Elem(FocusKey { entry: self.entry, elem: self.state.sel.0 }),
                    })),
                ));
                Handled::Yes
            }
            ScreenEvent::Tick(t) => {
                if self.adopt(cx.views.directory) {
                    fx.invalidate(Provenance::Input);
                }
                self.preview.sync(&self.state.shown, self.state.previewed(), cx.measure);
                self.form.table.update(t.dt(), RouteLayout::screen().sectioned_table().h);
                self.follow.update(t.dt());
                self.preview.step(t.dt());
                Handled::Yes
            }
            ScreenEvent::FocusMoved { to, .. } => {
                form_focus(&mut self.form, to.elem);
                if let Some(key) = self.form.key_at(self.form.table.sel.max(0) as usize) {
                    self.state.sel = key;
                }
                self.follow.follow(&self.state.description());
                Handled::Yes
            }
            ScreenEvent::Activate(key) => {
                self.activate(*key, fx);
                fx.invalidate(Provenance::Input);
                Handled::Yes
            }
            ScreenEvent::Input(InputEvent { kind: InputKind::Key { key, edge, .. }, .. }) if *edge != Edge::Up => {
                let handled = self.on_key(*key, *edge);
                fx.invalidate(Provenance::Input);
                handled
            }
            _ => Handled::No,
        }
    }
}

plx_ui::focusable_via_view!(LibraryOrderPage, InnerHost, view);

impl Screen<InnerHost> for LibraryOrderPage {
    fn name(&self) -> &'static str { crate::registry::word::LIBRARY_ORDER }
    fn state(&self) -> &dyn LogicalState { &self.state }
    fn crumb(&self, _cx: &Cx<'_, InnerHost>) -> Option<Cow<'_, str>> { Some(Cow::Borrowed(plx_os::i18n::msg::settings_title())) }
    fn prepare(&mut self, _b: &mut Budget, _cx: &Cx<'_, InnerHost>) {}
    fn draw(&mut self, f: &mut DrawFrame<'_, '_, InnerHost>) {
        let picture = self.preview.slot();
        let mut v = self.view();
        v.header.picture = picture.as_ref().map(|slot| slot as &dyn PictureSlot);
        plx_ui::screen::Part::<InnerHost>::draw(&mut v, f, Rect::FULL);
    }
    fn render(&self) -> RenderStrategy { RenderStrategy::Page }
    fn focus_source(&self) -> FocusSource { FocusSource::Engine }
    fn hit_source(&self) -> HitSource { HitSource::Engine }
}

/// The `(focus key, destination)` of every `Nav` row the page lists (the structural navigation
/// test's expectation, built by the page's own form builder): none.
#[cfg(test)]
pub(super) fn nav_items_for_test() -> Vec<(u32, SettingsPage)> {
    let rows = [("Movies", SecKind::Movie), ("TV Shows", SecKind::Show)].into_iter().enumerate()
        .map(|(section, (title, kind))| LibraryOrderRow { section, machine_id: "m".into(), key: section as i64, title: title.into(), kind, handle: String::new() })
        .collect::<Vec<_>>();
    let mut form: FormTable<LibId, (), SettingsPage> = FormTable::page(super::super::registry::BAND);
    form.set(order_form(&rows, &Mode::Idle, true), None);
    (0..form.table.n_rows() as usize)
        .filter_map(|i| match form.binding_at(i)?.kind.clone() {
            RowKind::Nav(dest) => Some((form.key_at(i)?.0, dest)),
            _ => None,
        })
        .collect()
}

/// The focus key of a library's row (the tests address rows by identity).
#[cfg(test)]
pub(super) fn lib_elem_for_test(section: usize) -> u32 {
    LibId::Lib(section).key().0
}

/// The focus key of the Reset row.
#[cfg(test)]
pub(super) fn reset_elem_for_test() -> u32 {
    LibId::Reset.key().0
}

#[cfg(test)]
#[path = "settings_library_order_tests.rs"]
mod tests;
