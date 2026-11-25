//! **Choose audio and subtitle tracks before playback starts** (#1532): the rows, the picks and the
//! commit of the *Audio & Subtitles* panel, with no session and no screen.
//!
//! The Detail hero's disc, the `…` menu's row and the playback failure screen's entry all open the
//! same two-level panel, so the rows live here once and each host owns only its surface. A host
//! pushes the pages it is asked for (`PickPage`), shows [`root_form`] / [`audio_form`] /
//! [`subtitle_form`] on its own `FormTable`, resolves a focused row to a [`Pick`] ([`pick_of`]),
//! and commits with [`commit`]. BACK pops a page before it dismisses, and a push or pop slides the
//! two pages, exactly as the in-player menus do (`track_menu`, `more_menu`).
//!
//! **Not a second track list.** Audio rows are [`crate::track_menu::audio_track_row`], the
//! in-player Audio tab's own row; the Subtitles page is `metadata::sub_layout::sub_sections`
//! grouped the same way (Off, your languages, one section per multi-track language) with the
//! Subtitles tab's own row builders. Differences, each forced by there being no playback yet: no
//! Timing / Style / Forced only (they act on a drawn subtitle or on the audio that plays), no
//! "converted by the server" sub-line (no route), and the Other languages drill-in is flattened
//! into one trailing section, A-Z (a second slide level for a rarely used list).
//!
//! **A pick is the user pick the in-player menu makes**: [`commit`] is
//! [`plx_media::route::commit_preplay_pick`], which queues `PUT /library/parts/{id}?audioStreamID=..
//! &subtitleStreamID=..` (Off = `subtitleStreamID=0`) and, for an episode, writes the show's track
//! memory, so the start's plan reads the pick instead of the old selection.
//!
//! **A movie's first start plans the pick.** A movie has no show memory, so besides the PUT the
//! host sends [`patch_command`] (`MetadataCmd::PatchTracks`) to the metadata store: the loaded
//! page's `selected` flags, which `metadata::cached_playing` and so the plan read, then say what was
//! picked. [`PickerState`] is the panel's whole state (pages, slide, carried choice) and a screen
//! only hosts it.
//!
//! **Host contract.** [`select`] writes a memo only for the half that differs from `now`, so a host
//! carries the last [`Selection::chosen`] forward as the next `now` rather than re-deriving it from
//! the server's flags; otherwise a pick back to the original leaves the intermediate pick in the
//! show's memory.

use plx_data::metadata::sub_layout::{self, SubHeader, SubRow};
use plx_data::metadata::track_names::TrackNames;
use plx_data::metadata::{Stream, SubChoice};
use plx_plex::plex::session::prefs::{AudioMemo, SubtitleMemo};
use plx_machine::machine::Measure;
use plx_ui::form::{Form, FormId, FormSection, FormTable, RowKey, RowKind};
use plx_ui::page_stack::{self, PageStack, PopoverPanel};
use plx_ui::panel_motion::PanelMotion;
use plx_ui::table::{Row, Section};
use plx_ui::{Painter, Rect};

use crate::track_menu::{audio_track_row, flat_row, in_lang_row};

/// The panel's pages. The root is the two rows; the other two are pushed by OK on one.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PickPage {
    Root,
    Audio,
    Subtitles,
}

/// A row's identity: a family base plus the track's index in the item's list, never a list
/// position of the page (the in-player menu's rule), so a rebuild lands on the same row.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PickRow {
    OpenAudio,
    OpenSubtitles,
    Audio(usize),
    SubOff,
    Sub(usize),
}

impl FormId for PickRow {
    fn key(&self) -> RowKey {
        RowKey(match self {
            PickRow::OpenAudio => 1,
            PickRow::OpenSubtitles => 2,
            PickRow::SubOff => 3,
            PickRow::Audio(i) => 1000 + *i as u32,
            PickRow::Sub(i) => 2000 + *i as u32,
        })
    }
}

pub type PickForm = Form<PickRow, (), PickPage>;

/// The item's tracks the picker shows: the playing-item store's own lists (`metadata::playing()`),
/// in the server's order.
#[derive(Clone, Copy)]
pub struct TrackSet<'a> {
    pub audio: &'a [Stream],
    pub subs: &'a [Stream],
}

/// What is chosen, as indices into [`TrackSet`]'s lists; `sub: None` is Off.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Chosen {
    pub audio: usize,
    pub sub: Option<usize>,
}

/// A subtitle the picker offers: before playback there is no route to burn a sidecar, so only the
/// embedded tracks and the sidecars the app can draw itself are listed.
fn offered(s: &Stream) -> bool {
    !s.external || s.sidecar_renderable()
}

/// What the server has selected right now (`Stream.selected`), falling back to the file's default
/// audio and to Off, which is what the plan starts from.
pub fn chosen(set: TrackSet<'_>) -> Chosen {
    let audio = set
        .audio
        .iter()
        .position(|s| s.selected)
        .or_else(|| set.audio.iter().position(|s| s.default))
        .unwrap_or(0);
    // The server's selected subtitle, even a sidecar the app cannot draw: Off must not read as
    // checked while the server still has a track selected (the page then lists it, see
    // `subtitle_form`).
    let sub = set.subs.iter().position(|s| s.selected);
    Chosen { audio, sub }
}

/// The audio list's row label for track `i`, the word the root's Audio row reads out.
pub fn audio_label(set: TrackSet<'_>, i: usize) -> String {
    set.audio.get(i).map_or_else(String::new, |s| {
        if s.lang.is_empty() { plx_os::i18n::msg::widgets_tracks_unknown().to_string() } else { s.lang.clone() }
    })
}

/// The word the root's Subtitles row reads out: the chosen track's language, or Off.
pub fn subtitle_label(set: TrackSet<'_>, sub: Option<usize>) -> String {
    match sub.and_then(|i| set.subs.get(i)) {
        Some(s) if !s.lang.is_empty() => s.lang.clone(),
        Some(_) => plx_os::i18n::msg::widgets_tracks_unknown().to_string(),
        None => plx_os::i18n::msg::widgets_tracks_off().to_string(),
    }
}

/// The panel's title band and the first page's header: the catalog's existing *Audio & subtitles*.
pub fn title() -> &'static str {
    plx_os::i18n::msg::settings_audio_title()
}

/// The root page: Audio and Subtitles, each a drill-in whose value is the current choice. A page
/// with nothing to choose from (one audio track, no subtitles) still lists its row, reading what
/// it has; the pages themselves are the place to see there is nothing else.
pub fn root_form(set: TrackSet<'_>, now: Chosen) -> PickForm {
    Form::new().section(
        FormSection::from_head(Section::new(""))
            .item(
                PickRow::OpenAudio,
                RowKind::Nav(PickPage::Audio),
                (),
                Row::new(plx_os::i18n::msg::widgets_tracks_audio()).value(audio_label(set, now.audio)),
            )
            .item(
                PickRow::OpenSubtitles,
                RowKind::Nav(PickPage::Subtitles),
                (),
                Row::new(plx_os::i18n::msg::widgets_tracks_subtitles()).value(subtitle_label(set, now.sub)),
            ),
    )
}

/// The Audio page: the in-player Audio tab's rows, the chosen one checked.
pub fn audio_form(set: TrackSet<'_>, now: Chosen) -> PickForm {
    let names = TrackNames::new();
    let mut sec = FormSection::new(plx_os::i18n::msg::widgets_tracks_audio());
    for i in 0..set.audio.len() {
        sec = sec.item(PickRow::Audio(i), RowKind::Choice, (), audio_track_row(set.audio, i, i == now.audio, &names, |s| s));
    }
    Form::new().section(sec)
}

/// The Subtitles page: `sub_layout::sub_sections`' grouping with the Subtitles tab's rows. `yours`
/// is "your languages" in preference order (the plan's account preference, then the playing
/// audio's language; callers pass what they know). The model's Timing / Style / Forced only rows
/// and its Other languages drill-in are not drawn (see the module doc): the languages behind that
/// row follow as one section, A-Z.
pub fn subtitle_form(set: TrackSet<'_>, now: Chosen, yours: &[String]) -> PickForm {
    let names = TrackNames::new();
    let shown: Vec<usize> =
        (0..set.subs.len()).filter(|i| offered(&set.subs[*i]) || now.sub == Some(*i)).collect();
    let model = sub_layout::sub_sections(set.subs, &shown, &names, yours, false);
    let active = now.sub.map_or(-1, |i| i as i32);
    let mut form = Form::new();
    for sec in &model.sections {
        let head = match &sec.header {
            SubHeader::Subtitles => Section::new(plx_os::i18n::msg::widgets_tracks_subtitles()),
            SubHeader::Language { name, tracks } => {
                Section::new(name.clone()).accessory(plx_os::i18n::msg::widgets_tracks_count(*tracks as i64))
            }
            SubHeader::Bare => Section::new(""),
        };
        let mut out = FormSection::from_head(head);
        for row in &sec.rows {
            out = match row {
                SubRow::Off => out.item(
                    PickRow::SubOff,
                    RowKind::Choice,
                    (),
                    Row::new(plx_os::i18n::msg::widgets_tracks_off()).checked(now.sub.is_none()),
                ),
                SubRow::Flat(t) => out.item(PickRow::Sub(t.i), RowKind::Choice, (), flat_row(t, active)),
                SubRow::InLanguage(t) => out.item(PickRow::Sub(t.i), RowKind::Choice, (), in_lang_row(t, active)),
                SubRow::ForcedOnly | SubRow::OtherLanguages { .. } | SubRow::Timing | SubRow::Style => out,
            };
        }
        form = form.section(out);
    }
    if !model.other.is_empty() {
        let mut out = FormSection::new(plx_os::i18n::msg::widgets_tracks_other_languages());
        for lang in &model.other {
            for t in &lang.tracks {
                out = out.item(PickRow::Sub(t.i), RowKind::Choice, (), flat_row(t, active));
            }
        }
        form = form.section(out);
    }
    form
}

/// What a focused row means when OK lands on it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Pick {
    /// A drill-in: push this page.
    Open(PickPage),
    Audio(usize),
    /// A subtitle track, or `None` for Off.
    Subtitle(Option<usize>),
}

/// A row's meaning, by identity.
pub fn pick_of(row: PickRow) -> Pick {
    match row {
        PickRow::OpenAudio => Pick::Open(PickPage::Audio),
        PickRow::OpenSubtitles => Pick::Open(PickPage::Subtitles),
        PickRow::Audio(i) => Pick::Audio(i),
        PickRow::SubOff => Pick::Subtitle(None),
        PickRow::Sub(i) => Pick::Subtitle(Some(i)),
    }
}

/// A pick, resolved to the stream ids the PUT carries and the descriptors the show's memory keeps.
#[derive(Clone, PartialEq, Debug)]
pub struct Selection {
    pub chosen: Chosen,
    pub audio_sid: i64,
    /// `0` is Off.
    pub subtitle_sid: i64,
    /// Whether the viewer has chosen Off, which the plan ranks above the account's subtitle rule
    /// ([`SubChoice`]). Not derivable from `subtitle_sid == 0`: an audio-only pick on an item whose
    /// subtitle was never touched also sends `0`.
    pub sub_choice: SubChoice,
    /// Only for the half the viewer changed.
    pub audio_memo: Option<AudioMemo>,
    pub subtitle_memo: Option<SubtitleMemo>,
}

impl Selection {
    /// The subtitle half a retry should carry: `Some(0)` for the viewer's Off, `Some(id)` for a track,
    /// `None` when the viewer left subtitles alone (an audio pick on an item whose subtitle was never
    /// touched keeps what the attempt stood on).
    pub fn held_subtitle(&self) -> Option<i64> {
        match self.sub_choice {
            SubChoice::Off => Some(0),
            SubChoice::Server => (self.subtitle_sid != 0).then_some(self.subtitle_sid),
        }
    }
}

/// Apply `pick` over `now`: the new choice and everything [`commit`] writes. `None` for a drill-in
/// or an index off the list. `choice` is the viewer's standing subtitle decision so far: a subtitle
/// row (Off or a track) replaces it, an audio pick keeps it.
pub fn select(set: TrackSet<'_>, now: Chosen, choice: SubChoice, pick: Pick) -> Option<Selection> {
    let mut next = now;
    let mut sub_choice = choice;
    let (mut audio_memo, mut subtitle_memo) = (None, None);
    match pick {
        Pick::Open(_) => return None,
        Pick::Audio(i) => {
            set.audio.get(i)?;
            next.audio = i;
            if i != now.audio {
                audio_memo = plx_media::route::memo_of_audio(set.audio, i);
            }
        }
        Pick::Subtitle(sub) => {
            if let Some(i) = sub {
                set.subs.get(i)?;
            }
            next.sub = sub;
            sub_choice = if sub.is_none() { SubChoice::Off } else { SubChoice::Server };
            if sub != now.sub {
                subtitle_memo = Some(sub.map_or(SubtitleMemo::Off, |i| plx_media::route::memo_of_subtitle(&set.subs[i])));
            }
        }
    }
    Some(Selection {
        chosen: next,
        audio_sid: set.audio.get(next.audio).map_or(0, |s| s.id),
        subtitle_sid: next.sub.map_or(0, |i| set.subs[i].id),
        sub_choice,
        audio_memo,
        subtitle_memo,
    })
}

/// Write a pick: the in-player user-pick door, session-free ([`plx_media::route::commit_preplay_pick`]).
pub fn commit(item: &plx_media::route::PreplayItem<'_>, sel: &Selection) -> bool {
    plx_media::route::commit_preplay_pick(item, sel.audio_sid, sel.subtitle_sid, sel.audio_memo.clone(), sel.subtitle_memo.clone())
}

// ---- the panel's state: pages, slide, carried choice -------------------------------------------

/// What BACK did.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Back {
    /// A page was popped; the panel stays.
    Popped,
    /// At the root: the host dismisses.
    Dismiss,
}

/// What OK on a row did.
#[derive(Clone, PartialEq, Debug)]
pub enum Outcome {
    /// A drill-in: the page was pushed and the two pages slide.
    Pushed,
    /// A track was picked. The host commits it ([`commit`]) and patches the loaded page
    /// ([`patch_command`]); the panel has already carried [`Selection::chosen`] forward as its own
    /// `now` and rebuilt the page.
    Picked(Selection),
    /// A row with nothing to do (an index off the list).
    Nothing,
}

/// **The Audio & Subtitles panel's state, with no screen**: the item's tracks, the choice made so
/// far, the pages pushed over the root and the slide between them. The page showing is the
/// stack's top (the root when empty), never a field of its own.
pub struct PickerState {
    tracks: plx_data::metadata::PreplayTracks,
    /// What the panel reads as chosen: the server's selection at open, then each pick's
    /// [`Selection::chosen`] (the host contract in the module doc).
    now: Chosen,
    /// The viewer's Off, once chosen ([`SubChoice`]); the panel's own record, since `now.sub: None`
    /// reads "Off" for an item whose subtitle the viewer never touched too.
    sub_choice: SubChoice,
    pages: PageStack<PickPage, PickRow>,
    pub form: FormTable<PickRow, (), PickPage>,
    pub motion: PanelMotion,
    /// The drawn rect of what opened the panel: `[x, y, w, h]` as bits, like every panel argument.
    anchor: [u32; 4],
}

impl PickerState {
    pub fn new(tracks: plx_data::metadata::PreplayTracks, anchor: [u32; 4]) -> Self {
        let mut s = Self {
            now: chosen(TrackSet { audio: &tracks.audio, subs: &tracks.subs }),
            tracks,
            sub_choice: SubChoice::Server,
            pages: PageStack::new(),
            form: FormTable::new(plx_ui::table_screen::BAND_BASE),
            motion: PanelMotion::new(),
            anchor,
        };
        s.form.table.compact = false;
        let root = s.page_form(PickPage::Root);
        s.form.set(root, None);
        s
    }

    fn set(&self) -> TrackSet<'_> {
        TrackSet { audio: &self.tracks.audio, subs: &self.tracks.subs }
    }

    pub fn tracks(&self) -> &plx_data::metadata::PreplayTracks {
        &self.tracks
    }

    pub fn now(&self) -> Chosen {
        self.now
    }

    /// The page showing.
    pub fn page(&self) -> PickPage {
        self.pages.top().unwrap_or(PickPage::Root)
    }

    pub fn depth(&self) -> usize {
        self.pages.len()
    }

    /// "Your languages" for the Subtitles page: the language of the audio that will play.
    fn yours(&self) -> Vec<String> {
        self.tracks.audio.get(self.now.audio).map(|a| a.lang_code.clone()).filter(|c| !c.is_empty()).into_iter().collect()
    }

    fn page_form(&self, page: PickPage) -> PickForm {
        match page {
            PickPage::Root => root_form(self.set(), self.now),
            PickPage::Audio => audio_form(self.set(), self.now),
            PickPage::Subtitles => subtitle_form(self.set(), self.now, &self.yours()),
        }
    }

    /// Where focus opens on a page: the checked row, so the viewer sees what is chosen.
    fn page_initial(&self, page: PickPage) -> Option<PickRow> {
        match page {
            PickPage::Root => None,
            PickPage::Audio => Some(PickRow::Audio(self.now.audio)),
            PickPage::Subtitles => Some(self.now.sub.map_or(PickRow::SubOff, PickRow::Sub)),
        }
    }

    fn title_of(page: PickPage) -> Option<String> {
        match page {
            PickPage::Root => None,
            PickPage::Audio => Some(plx_os::i18n::msg::widgets_tracks_audio().to_string()),
            PickPage::Subtitles => Some(plx_os::i18n::msg::widgets_tracks_subtitles().to_string()),
        }
    }

    /// OK on the row `id`.
    pub fn ok(&mut self, id: PickRow) -> Outcome {
        match pick_of(id) {
            Pick::Open(page) => {
                self.push(page, id);
                Outcome::Pushed
            }
            pick => match select(self.set(), self.now, self.sub_choice, pick) {
                Some(sel) => {
                    self.now = sel.chosen;
                    self.sub_choice = sel.sub_choice;
                    let form = self.page_form(self.page());
                    self.form.set_sliding(form, Some(&id));
                    Outcome::Picked(sel)
                }
                None => Outcome::Nothing,
            },
        }
    }

    /// Open `page` above the current one: remember the opener and its scroll, build the page, snap
    /// to its checked row and start the slide.
    fn push(&mut self, page: PickPage, opener: PickRow) {
        self.pages.push(page, opener, self.form.table.scroll_pos());
        let leaving = page_stack::leave_page(&mut self.form);
        let form = self.page_form(page);
        self.form.open(form, self.page_initial(page).as_ref());
        self.form.table.set_title(Self::title_of(page));
        self.motion.begin_slide(leaving, 1.0);
    }

    /// BACK: pop a page when there is one, else the host dismisses.
    pub fn back(&mut self) -> Back {
        let Some(saved) = self.pages.pop() else { return Back::Dismiss };
        let leaving = page_stack::leave_page(&mut self.form);
        self.motion.begin_slide(leaving, -1.0);
        let form = self.page_form(self.page());
        self.form.restore(form, Some(&saved.return_id), saved.scroll);
        self.form.table.set_title(Self::title_of(self.page()));
        Back::Popped
    }

    /// The panel's natural rect: under the opener when there is room, above it when there is not;
    /// cached against the table's layout revision (the slide springs toward it).
    pub fn panel_rect(&self, measure: &dyn Measure) -> Rect {
        self.motion.natural(self.form.table.layout_rev(), || {
            let [x, y, w, h] = self.anchor.map(f32::from_bits);
            let pw = self.form.table.menu_panel_width(measure);
            plx_ui::popover::under_or_above(Rect::new(x, y, w, h), pw, self.form.table.measured_height())
        })
    }

    pub fn shown_rect(&self, measure: &dyn Measure) -> Rect {
        self.motion.shown(self.panel_rect(measure))
    }

    pub fn update(&mut self, dt: f32, measure: &dyn Measure) {
        let natural = self.panel_rect(measure);
        self.form.table.update(dt, natural.h);
        self.motion.step(dt, natural);
    }

    pub fn draw(&self, appear: f32, measure: &dyn Measure) {
        let natural = self.panel_rect(measure);
        let p = Painter::root().alpha(appear).translate(0.0, plx_ui::popover::Popover::RISE * (1.0 - appear));
        self.motion.draw(p, natural, 20.0, &self.form.table, measure);
    }

    /// The row identity under a focus key.
    pub fn row_at(&self, key: RowKey) -> Option<PickRow> {
        self.form.index_of_key(key).and_then(|i| self.form.binding_at(i)).map(|b| b.id)
    }
}

impl PopoverPanel for PickerState {
    type Id = PickRow;
    type Act = ();
    type Dest = PickPage;
    fn form(&self) -> &FormTable<PickRow, (), PickPage> {
        &self.form
    }
    fn motion(&self) -> &PanelMotion {
        &self.motion
    }
    fn panel_rect(&self, measure: &dyn Measure) -> Rect {
        PickerState::panel_rect(self, measure)
    }
    fn shown_rect(&self, measure: &dyn Measure) -> Rect {
        PickerState::shown_rect(self, measure)
    }
}

/// The store command that makes the loaded page's copy of the item's tracks say what was picked, so
/// a movie's first start plans it (the plan reads those `selected` flags).
pub fn patch_command(tracks: &plx_data::metadata::PreplayTracks, sel: &Selection) -> plx_data::stores::metadata::MetadataCmd {
    plx_data::stores::metadata::MetadataCmd::PatchTracks {
        sid: tracks.sid,
        rk: tracks.rk.clone(),
        audio_sid: sel.audio_sid,
        subtitle_sid: sel.subtitle_sid,
        sub_choice: sel.sub_choice,
    }
}

/// [`commit`] for a picker's own item.
pub fn commit_for(tracks: &plx_data::metadata::PreplayTracks, sel: &Selection) -> bool {
    commit(&plx_media::route::PreplayItem::of(tracks), sel)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stream(id: i64, lang: &str, code: &str) -> Stream {
        Stream { id, lang: lang.into(), lang_code: code.into(), ..Stream::default() }
    }

    /// 2 audio (the second is the server's choice) + 2 subtitles (none selected).
    fn item() -> (Vec<Stream>, Vec<Stream>) {
        let mut a2 = stream(102, "French", "fra");
        a2.selected = true;
        (vec![stream(101, "English", "eng"), a2], vec![stream(201, "English", "eng"), stream(202, "French", "fra")])
    }

    /// How many selectable rows a page declares.
    fn rows(form: &PickForm) -> usize {
        form.slots().iter().flatten().count()
    }

    #[test]
    fn the_first_page_has_two_rows_reading_the_current_tracks() {
        let (audio, subs) = item();
        let set = TrackSet { audio: &audio, subs: &subs };
        let now = chosen(set);
        assert_eq!(now, Chosen { audio: 1, sub: None });
        let form = root_form(set, now);
        assert_eq!(rows(&form), 2);
        assert_eq!(audio_label(set, now.audio), "French");
        assert_eq!(subtitle_label(set, now.sub), plx_os::i18n::msg::widgets_tracks_off());
    }

    #[test]
    fn the_pages_list_every_track_and_off() {
        let (audio, subs) = item();
        let set = TrackSet { audio: &audio, subs: &subs };
        let now = chosen(set);
        assert_eq!(rows(&audio_form(set, now)), 2);
        // Off + the two subtitles
        assert_eq!(rows(&subtitle_form(set, now, &[])), 3);
    }

    #[test]
    fn a_subtitle_pick_carries_both_ids_and_off_is_zero() {
        let (audio, subs) = item();
        let set = TrackSet { audio: &audio, subs: &subs };
        let now = chosen(set);
        let on = select(set, now, SubChoice::Server, Pick::Subtitle(Some(1))).unwrap();
        assert_eq!((on.audio_sid, on.subtitle_sid), (102, 202), "the audio half is kept, not erased");
        assert!(on.audio_memo.is_none() && on.subtitle_memo.is_some());
        let off = select(set, on.chosen, on.sub_choice, Pick::Subtitle(None)).unwrap();
        assert_eq!((off.audio_sid, off.subtitle_sid), (102, 0));
        assert_eq!(off.subtitle_memo, Some(SubtitleMemo::Off));
        assert_eq!((on.sub_choice, off.sub_choice), (SubChoice::Server, SubChoice::Off));
        // Off sticks across an audio pick; a never-touched subtitle is not "Off chosen" by one.
        let audio = select(set, off.chosen, off.sub_choice, Pick::Audio(0)).unwrap();
        assert_eq!(audio.sub_choice, SubChoice::Off);
        assert_eq!(select(set, now, SubChoice::Server, Pick::Audio(0)).unwrap().sub_choice, SubChoice::Server);
    }

    #[test]
    fn an_audio_pick_changes_only_the_audio_and_a_drill_in_picks_nothing() {
        let (audio, subs) = item();
        let set = TrackSet { audio: &audio, subs: &subs };
        let now = chosen(set);
        let sel = select(set, now, SubChoice::Server, Pick::Audio(0)).unwrap();
        assert_eq!((sel.audio_sid, sel.subtitle_sid), (101, 0));
        assert!(sel.audio_memo.is_some() && sel.subtitle_memo.is_none());
        assert!(select(set, now, SubChoice::Server, pick_of(PickRow::OpenAudio)).is_none());
        assert!(select(set, now, SubChoice::Server, Pick::Audio(9)).is_none());
    }

    fn picker() -> PickerState {
        let (audio, subs) = item();
        let tracks = plx_data::metadata::PreplayTracks {
            sid: plx_plex::plex::ServerId::from_raw(0),
            rk: "m1".into(),
            kind: "movie".into(),
            show_rk: String::new(),
            part: "/library/parts/9001/1/f.mkv".into(),
            audio,
            subs,
        };
        PickerState::new(tracks, [0, 0, 0, 0])
    }

    /// OK on a drill-in pushes (the stack is the page), BACK pops to the root and only then dismisses.
    #[test]
    fn back_pops_a_page_before_it_dismisses() {
        let mut p = picker();
        assert_eq!((p.page(), p.depth()), (PickPage::Root, 0));
        assert_eq!(p.ok(PickRow::OpenSubtitles), Outcome::Pushed);
        assert_eq!((p.page(), p.depth()), (PickPage::Subtitles, 1));
        assert_eq!(p.form.focusable_len(), 3, "Off and the two tracks");
        assert_eq!(p.back(), Back::Popped);
        assert_eq!((p.page(), p.depth()), (PickPage::Root, 0));
        assert_eq!(p.back(), Back::Dismiss, "at the root BACK leaves the panel");
    }

    /// A pick carries its choice forward: the page is rebuilt checking it, the next pick is diffed
    /// against it (a pick back to the original writes no memory of the half it did not change), and
    /// the root reads it after the pop.
    #[test]
    fn a_pick_is_carried_forward_and_the_root_reads_it_after_the_pop() {
        let mut p = picker();
        assert_eq!(p.now(), Chosen { audio: 1, sub: None });
        p.ok(PickRow::OpenSubtitles);
        let Outcome::Picked(on) = p.ok(PickRow::Sub(1)) else { panic!("a track row picks") };
        assert_eq!((on.audio_sid, on.subtitle_sid), (102, 202));
        assert_eq!(p.now(), Chosen { audio: 1, sub: Some(1) });
        let Outcome::Picked(off) = p.ok(PickRow::SubOff) else { panic!("Off picks") };
        assert_eq!(off.subtitle_sid, 0);
        assert_eq!(off.subtitle_memo, Some(SubtitleMemo::Off), "diffed against the carried pick, not the server's");
        assert_eq!(p.back(), Back::Popped);
        assert_eq!(subtitle_label(p.set(), p.now().sub), plx_os::i18n::msg::widgets_tracks_off());
        assert_eq!(p.ok(PickRow::Audio(9)), Outcome::Nothing, "an index off the list picks nothing");
    }
}
