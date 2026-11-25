//! **Audio & Subtitles before playback** (#1532): the panel a viewer opens to choose the tracks a
//! title starts with. A `Style::Compact` surface anchored to what opened it (the hero's disc, the
//! `…` menu's row, the failure screen's entry), `versions_panel`'s shape. The panel's whole state is
//! `plx_appkit::preplay_tracks::PickerState`; this screen only hosts it (focus stops, input, draw).
//!
//! **A pick is two writes**, both emitted from here the moment OK lands on a track: the loaded
//! page's copy of the item's tracks is patched (`AppFx::Store(Metadata, PatchTracks)`, so a movie's
//! first start plans the pick, and an Off outranks the account's subtitle rule), and the server
//! gets the selection PUT plus the show's track memory ([`ContentReq::CommitPreplayPick`], run by
//! the loop). The panel stays up with the check moved; BACK pops a page before it dismisses.
//!
//! **Entry point.** Anything that wants the panel sends
//! `ContentReq::Panel(ContentPanel::PreplayTracks { anchor })` from a Detail page: the panel's
//! subject is the page's item, and the tracks are the ones of the item the hero PLAYS
//! (`detail::play_subject`: on a show page the hero episode, never the show). The playback failure
//! screen opens the same surface with [`Source::Failed`], which reads the tracks of the item that
//! was playing instead of a page's.

use std::borrow::Cow;

use plx_appkit::preplay_tracks::{Back, Outcome, PickerState};
use plx_data::metadata::PreplayTracks;
use plx_data::stores::{StoreCmd, StoreId};
use plx_machine::machine::{
    Canon, Cx, Edge, Effects, EntryId, FocusKey, Fx, GroupId, Handled, InputKind, InstanceId, Key, LogicalState,
    Machine, NavOp,
};
use plx_plex::plex::ServerId;
use plx_ui::a11y::{A11y, A11yPath};
use plx_ui::frame::Budget;
use plx_ui::form::RowKey;
use plx_ui::page_stack;
use plx_ui::screen::{
    At, AxisMask, Dir, DrawFrame, EdgeRule, ElemKind, FocusSource, Focusable, GroupKind, GroupSpec, HitSource, Placed,
    RenderStrategy, Screen, ScreenEvent, Scrim, Seat, Step,
};

use crate::registry::{AppFx, AppLike, ContentReq, MetadataLike, PageMemory};

/// The fields [`PreplayPanelScreen::write`] canonicalises, for the recorder's shape pin (§5.4).
pub const SHAPE: &str = "PreplayPanel{arg:PreplayArg{host:u32,sid:u32,rk:str,anchor:[u32;4],source:{Page,Failed{part_id:u64}}},audio:u32,sub:Option<u32>,depth:u32}";

/// Where the panel's tracks come from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Source {
    /// The Detail page the panel is over: `PreplayTracks` of the item its hero plays.
    Page,
    /// The playback failure screen: the item that was playing (`metadata.playing()`) and the part of
    /// the failed attempt, which is the part the selection PUT addresses.
    ///
    /// `pick` seeds the rows: the session's held pick, else the attempt's standing selection
    /// (`route::failed_pick`), so a reopen shows what Try again would play. A seed like the page's
    /// store pick, not part of the surface's identity (the canon below does not write it).
    Failed { part_id: i64, pick: Option<plx_data::metadata::PreplayPick> },
}

/// What the container is asked to present: the host instance, the item the page stands on, the drawn
/// rect of what opened the panel (bit-preserving, like every panel argument) and where the tracks
/// are read from.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct PreplayArg {
    pub host: InstanceId,
    pub sid: ServerId,
    pub rk: String,
    pub anchor: [u32; 4],
    pub source: Source,
}

impl LogicalState for PreplayArg {
    fn write(&self, c: &mut Canon) {
        c.u32(self.host.0).u32(u32::from(self.sid.raw())).str(&self.rk);
        for value in self.anchor {
            c.u32(value);
        }
        match self.source {
            Source::Page => {
                c.u32(0);
            }
            Source::Failed { part_id, .. } => {
                c.u32(1).u64(part_id as u64);
            }
        }
    }
    fn probe(&self, out: &mut String) {
        out.push_str("preplay_arg");
    }
}

pub struct PreplayPanelScreen {
    entry: EntryId,
    arg: PreplayArg,
    picker: PickerState,
}

impl PreplayPanelScreen {
    /// `tracks` is what the item offers, read by the registry's mount from [`Source`]; a page that
    /// cannot say gets an empty set, which lists its two rows with nothing behind them.
    pub fn new(entry: EntryId, arg: PreplayArg, tracks: PreplayTracks) -> Self {
        let anchor = arg.anchor;
        Self { entry, arg, picker: PickerState::new(tracks, anchor) }
    }

    pub fn picker(&self) -> &PickerState {
        &self.picker
    }

    fn dismiss<H: AppLike<Memory = PageMemory>>(&self, fx: &mut Effects<'_, H>) {
        fx.push(Fx::Nav(NavOp::Dismiss(self.entry)));
    }

    /// BACK, or a click on the title band: pop a page, else close.
    fn back<H: AppLike<Memory = PageMemory>>(&mut self, fx: &mut Effects<'_, H>) {
        if self.picker.back() == Back::Dismiss {
            self.dismiss(fx);
        }
    }

    fn activate<H: AppLike<Memory = PageMemory>>(&mut self, elem: u32, fx: &mut Effects<'_, H>) {
        if page_stack::is_title_key(elem) {
            return self.back(fx);
        }
        let Some(id) = self.picker.row_at(RowKey(elem)) else { return };
        if let Outcome::Picked(sel) = self.picker.ok(id) {
            let tracks = self.picker.tracks().clone();
            fx.push(Fx::App(AppFx::Store(
                StoreId::Metadata,
                StoreCmd::Metadata(plx_appkit::preplay_tracks::patch_command(&tracks, &sel)),
            )));
            fx.push(Fx::App(AppFx::Content(ContentReq::CommitPreplayPick { tracks, sel, source: self.arg.source })));
        }
    }
}

impl<H: AppLike<Memory = PageMemory>> Machine<H> for PreplayPanelScreen {
    type Ev = ScreenEvent<H>;
    fn step(&mut self, ev: &Self::Ev, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) -> Handled {
        match ev {
            ScreenEvent::Tick(tick) => {
                let focused = cx.focus.current.filter(|key| key.entry == self.entry);
                if let Some(i) = focused.and_then(|key| self.picker.form.index_of_key(RowKey(key.elem))) {
                    self.picker.form.table.sel = i as i32;
                }
                self.picker.update(tick.dt(), cx.measure);
                Handled::Yes
            }
            ScreenEvent::FocusMoved { to, .. } => {
                if let Some(i) = self.picker.form.index_of_key(RowKey(to.elem)) {
                    self.picker.form.table.sel = i as i32;
                }
                fx.invalidate(plx_machine::present::Provenance::Input);
                Handled::Yes
            }
            ScreenEvent::Activate(elem) => {
                self.activate(*elem, fx);
                Handled::Yes
            }
            ScreenEvent::PressCommit(_) => {
                if let Some(key) = cx.focus.current {
                    self.activate(key.elem, fx);
                }
                Handled::Yes
            }
            ScreenEvent::Input(input) => match input.kind {
                InputKind::Key { key: Key::Back, edge: Edge::Down, .. } => {
                    self.back(fx);
                    Handled::Yes
                }
                _ => Handled::No,
            },
            _ => Handled::No,
        }
    }
}

const GROUP: GroupId = GroupId(0);

impl<H: AppLike<Memory = PageMemory> + MetadataLike> Focusable<H> for PreplayPanelScreen {
    fn groups(&self, cx: &Cx<'_, H>, out: &mut Vec<GroupSpec>) {
        out.push(GroupSpec {
            id: GROUP,
            kind: GroupKind::Column,
            seat: Seat::Remembered,
            reachable: AxisMask::VERTICAL,
            edge: [EdgeRule::Stop; 4],
            extent: self.picker.panel_rect(cx.measure),
            len: self.picker.form.focusable_len(),
            elem: ElemKind::Bare,
        });
    }
    fn group_of(&self, elem: &u32, _cx: &Cx<'_, H>) -> Option<GroupId> {
        page_stack::popover_group_of(&self.picker, elem, GROUP)
    }
    fn neighbour(&self, key: FocusKey<u32>, dir: Dir, _cx: &Cx<'_, H>) -> Step<u32> {
        page_stack::popover_neighbour(&self.picker, self.entry, key, dir)
    }
    fn place(&self, elem: &u32, cx: &Cx<'_, H>, _at: At) -> Option<Placed> {
        page_stack::popover_place(&self.picker, elem, cx.measure)
    }
    fn reconcile(&self, want: FocusKey<u32>, cx: &Cx<'_, H>) -> FocusKey<u32> {
        if self.group_of(&want.elem, cx).is_some() {
            want
        } else {
            page_stack::popover_seat(&self.picker, self.entry)
        }
    }
    fn seat(&self, _g: GroupId, _from: Placed, _cx: &Cx<'_, H>) -> FocusKey<u32> {
        page_stack::popover_seat(&self.picker, self.entry)
    }
    /// `preplay.<row>`: the two root rows by name, every track row by its index in the item's list.
    fn a11y(&self, key: &u32, _cx: &Cx<'_, H>) -> Option<A11y<'_>> {
        use plx_appkit::preplay_tracks::PickRow;
        page_stack::popover_a11y(&self.picker.form, key, |id| match *id {
            PickRow::OpenAudio => A11yPath::new().name("audio"),
            PickRow::OpenSubtitles => A11yPath::new().name("subtitles"),
            PickRow::SubOff => A11yPath::new().name("subtitle").name("off"),
            PickRow::Audio(i) => A11yPath::new().name("audio").index(i),
            PickRow::Sub(i) => A11yPath::new().name("subtitle").index(i),
        })
    }
    fn a11y_elems(&self, g: GroupId, _cx: &Cx<'_, H>, out: &mut Vec<u32>) {
        page_stack::popover_a11y_elems(&self.picker.form, g, GROUP, out);
    }
}

impl LogicalState for PreplayPanelScreen {
    fn write(&self, c: &mut Canon) {
        self.arg.write(c);
        let now = self.picker.now();
        c.u32(now.audio as u32);
        match now.sub {
            Some(i) => c.u32(1).u32(i as u32),
            None => c.u32(0),
        };
        c.u32(self.picker.depth() as u32);
    }
    fn probe(&self, out: &mut String) {
        out.push_str("preplay");
    }
}

impl<H: AppLike<Memory = PageMemory> + MetadataLike> Screen<H> for PreplayPanelScreen {
    fn name(&self) -> &'static str {
        "preplay"
    }
    fn state(&self) -> &dyn LogicalState {
        self
    }
    fn crumb(&self, _cx: &Cx<'_, H>) -> Option<Cow<'_, str>> {
        None
    }
    fn prepare(&mut self, _b: &mut Budget, _cx: &Cx<'_, H>) {}
    fn scrim(&self) -> Scrim {
        Scrim::dim(plx_ui::theme::underlay::DIM_PANEL)
    }
    fn pointer_held(&self) -> bool {
        self.picker.motion.transitioning()
    }
    fn draw(&mut self, f: &mut DrawFrame<'_, '_, H>) {
        let measure = f.measure;
        plx_ui::profile::phase("dt.preplay", || {
            let appear = f.page_alpha;
            self.picker.draw(appear, measure);
        });
        page_stack::popover_register_stops(&self.picker, self.entry, f);
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
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::registry::AppMsg;
    use plx_data::metadata::Stream;
    use plx_machine::machine::{Host, MachineId};
    use plx_ui::screen::ScreenArg;

    #[derive(Clone)]
    struct Arg;
    impl LogicalState for Arg {
        fn write(&self, _: &mut Canon) {}
        fn probe(&self, _: &mut String) {}
    }
    impl ScreenArg for Arg {
        fn chrome(&self) -> plx_machine::machine::Chrome {
            plx_machine::machine::Chrome::None
        }
        fn id(&self) -> plx_machine::machine::ScreenId {
            plx_machine::machine::ScreenId(1)
        }
        fn title(&self) -> Option<&str> {
            None
        }
        fn same_instance(&self, _: &Self) -> bool {
            true
        }
    }
    struct HostFixture;
    impl Host for HostFixture {
        type Arg = Arg;
        type Fx = AppFx;
        type Msg = AppMsg;
        type Elem = u32;
        type Views<'a> = ();
        type Init = Arg;
        type Memory = PageMemory;
    }
    thread_local! {
        static TEST_METADATA: std::cell::UnsafeCell<plx_data::stores::metadata::MetadataStore> =
            std::cell::UnsafeCell::new(plx_data::stores::metadata::MetadataStore::default());
    }
    impl MetadataLike for HostFixture {
        fn metadata<'a>(_cx: &Cx<'a, Self>) -> plx_data::metadata::MetadataView<'a> {
            TEST_METADATA.with(|cell| unsafe { &mut *cell.get() }).view()
        }
    }

    fn stream(id: i64, lang: &str, code: &str) -> Stream {
        Stream { id, lang: lang.into(), lang_code: code.into(), ..Stream::default() }
    }

    fn tracks() -> PreplayTracks {
        PreplayTracks {
            sid: ServerId::from_raw(1),
            rk: "42".into(),
            kind: "movie".into(),
            show_rk: String::new(),
            part: "/library/parts/9001/1/file.mkv".into(),
            audio: vec![stream(101, "English", "eng"), stream(102, "French", "fra")],
            subs: vec![stream(201, "English", "eng")],
            pick: None,
        }
    }

    fn panel() -> PreplayPanelScreen {
        panel_from(Source::Page)
    }

    fn panel_from(source: Source) -> PreplayPanelScreen {
        let anchor = [500.0f32, 600.0, 64.0, 64.0].map(f32::to_bits);
        let arg = PreplayArg { host: InstanceId(9), sid: ServerId::from_raw(1), rk: "42".into(), anchor, source };
        PreplayPanelScreen::new(EntryId(3), arg, tracks())
    }

    fn cx() -> Cx<'static, HostFixture> {
        use plx_machine::machine::{FocusRead, InputOwner, PressRead, Tick};
        Cx {
            views: (),
            tick: Tick::default(),
            measure: &plx_ui::fixture::FixtureMeasure,
            focus: FocusRead::default(),
            press: PressRead::default(),
            modal: Default::default(), page_image: Default::default(), hold_hint: Default::default(),
            owner: InputOwner::Entry(EntryId(3)),
        }
    }

    /// The panel's root page, run through the id conformance (`AppArg::PreplayPanel`; the coverage
    /// table calls this).
    pub(crate) fn preplay_panel_conforms() -> plx_ui::a11y::Namespaces {
        vec![plx_ui::a11y::conform::<HostFixture, _>(&panel(), &cx())]
    }

    fn drive(
        p: &mut PreplayPanelScreen,
        f: impl FnOnce(&mut PreplayPanelScreen, &mut Effects<'_, HostFixture>),
    ) -> Vec<Fx<HostFixture>> {
        let mut out = Vec::new();
        let mut present = plx_machine::present::Present::new();
        let mut fx: Effects<'_, HostFixture> = Effects::new(&mut out, MachineId::Instance(InstanceId(4)), &mut present);
        f(p, &mut fx);
        out.into_iter().map(|e| e.fx).collect()
    }

    /// OK on a drill-in opens the page and writes nothing; OK on a track emits both writes (the
    /// store patch and the addressed commit); BACK pops a page before it dismisses.
    #[test]
    fn a_pick_emits_the_patch_and_the_commit_and_back_pops_before_it_dismisses() {
        use plx_appkit::preplay_tracks::PickRow;
        let mut p = panel();
        let elem = |row: PickRow| plx_ui::form::FormId::key(&row).0;
        let fx = drive(&mut p, |p, fx| p.activate(elem(PickRow::OpenSubtitles), fx));
        assert!(fx.is_empty(), "a drill-in picks nothing");
        assert_eq!(p.picker().depth(), 1);
        let fx = drive(&mut p, |p, fx| p.activate(elem(PickRow::SubOff), fx));
        let patched = fx.iter().any(|f| {
            matches!(
                f,
                Fx::App(AppFx::Store(
                    StoreId::Metadata,
                    StoreCmd::Metadata(plx_data::stores::metadata::MetadataCmd::PatchTracks {
                        pick: plx_data::metadata::PreplayPick { sub: plx_data::metadata::SubPick::Off, .. },
                        ..
                    })
                ))
            )
        });
        let committed = fx.iter().any(|f| {
            matches!(f, Fx::App(AppFx::Content(ContentReq::CommitPreplayPick { sel, .. })) if sel.subtitle_sid == 0)
        });
        assert!(patched && committed, "Off patches the page and commits the PUT");
        let fx = drive(&mut p, |p, fx| p.back(fx));
        assert!(fx.is_empty() && p.picker().depth() == 0, "BACK pops the subtitle page first");
        let fx = drive(&mut p, |p, fx| p.back(fx));
        assert!(matches!(fx.first(), Some(Fx::Nav(NavOp::Dismiss(EntryId(3))))), "BACK at the root closes");
    }
}
