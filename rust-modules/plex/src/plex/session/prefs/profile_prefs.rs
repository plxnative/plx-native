//! **Playback and display settings, per profile.**
//!
//! Quality, Direct Play, subtitle size, position and tone, the HDR overlay brightness, audio enhancements, the next-episode
//! mode, the skip interval, instant D-pad seeking, what OK does on a Continue Watching card, and (since P1c) whether the
//! detail page's hero autoplays its trailer used to be one value for
//! the whole television. Each is now a value of the SEATED profile ([`Seat`]), kept in
//! `profile_prefs`:
//!
//! ```json
//! "profile_prefs": [ { "user": "<home uuid, or \"\" for the account owner>", "account": "<account_stamp>",
//!                      "prefs": { "subtitle_size": "large", "playback_quality": "720p_4_mbps" } } ]
//! ```
//!
//! * **The record is the profile's own value and nothing else.** A [`ProfilePrefs`] field is `None`
//!   when the profile never picked that setting, and a profile that never picked anything has no
//!   record at all.
//! * **The migration is a FALLBACK, not a copy.** [`effective`] is the profile's own value, else the
//!   top-level key of the same name read through [`Setting::get`] (quality `None` is Original), else
//!   the type's default. The top-level keys are frozen: a build with per-profile settings never
//!   writes one (session creation is the exception for two of them, `fresh_default` quality and
//!   `trailer_autoplay` set on, each the install default for profiles with no value of their own). So "copy today's values to
//!   every profile" and "read the shared value until the profile picks its own" read the same for
//!   every profile, existing or future, and the fallback writes nothing, so the upgrade cannot fail
//!   half-way.
//! * **A garbage own value reads the legacy value, not the default.** Each field is parsed by
//!   [`super::de_soft_opt`], which turns anything it cannot read into `None` (the per-type `de_soft_*`
//!   functions of [`Preferences`] turn garbage into the DEFAULT, which would shadow the fallback).
//! * **Account-bound.** A record is visible only while its `account` equals the non-empty stamp of
//!   the signed-in account ([`crate::plex::session::account_stamp`]: the owner's identity when the
//!   roster names one, otherwise `grant::account_key` of the account token). The owner's `""`
//!   record of two accounts therefore never mixes. A re-login that replaces the token without a
//!   sign-out keeps the records when the roster names the same owner (the stamp does not move;
//!   a record an older build stamped with the token is adopted once,
//!   [`super::adopt_legacy_token_stamps`]). A known different owner drops the list. An unnamed
//!   roster leaves it. The next [`put_own`] still prunes every stamp but the one it writes, and
//!   leaves an owner-stamped record in place while the write cannot name an owner.
//! * **Written only through [`persist_profile_pref`] and [`persist_profile_hud`]** (one read-modify-write, `persist_profile_with`), which refuse without a seat, across a
//!   tenure or credential change, and with an empty stamp.
//! * **No `extensions`**: a field a later build adds is not carried by this one.
use super::{
    Fallback, Parse, ProfileKeyed, ProfileRecords, Preferences, Row, Scope, Setting, SettingsWrite,
    IntroSkipPref, CreditsSkipPref, DolbyVisionModePref, InstantDpadSeekingPref, UriEnginePref, AudioEnhancementsPref, DeckPressPref, DirectPlayModePref, HdrOverlayBrightnessPref, NextEpisodeModePref, QualityPref, SkipIntervalPref,
    HudSelectionPref, SubtitlePositionPref, SubtitleSizePref, SubtitleTonePref, TrailerAutoplayPref, HudEndTimePref, UnderPostersPref,
};
use crate::plex::session::{
    MarkerSkipMode, DolbyVisionMode, InstantDpadSeeking, UriEngine, DeckPress, DirectPlayMode, HdrOverlayBrightness, NextEpisodeMode, PlaybackQuality, Seat, Session, SkipInterval, SubtitlePosition,
    SubtitleSize, SubtitleTone, UnderPosters,
};
use crate::plex::AudioEnhancements;
use serde::{Deserialize, Serialize};

/// One profile's own setting values; `None` is "this profile has no value of its own".
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ProfilePrefs {
    #[serde(skip_serializing_if = "Option::is_none", deserialize_with = "super::de_soft_opt")]
    pub playback_quality: Option<PlaybackQuality>,
    #[serde(skip_serializing_if = "Option::is_none", deserialize_with = "super::de_soft_opt")]
    pub direct_play_mode: Option<DirectPlayMode>,
    #[serde(skip_serializing_if = "Option::is_none", deserialize_with = "super::de_soft_opt")]
    pub subtitle_tone: Option<SubtitleTone>,
    #[serde(skip_serializing_if = "Option::is_none", deserialize_with = "super::de_soft_opt")]
    pub subtitle_size: Option<SubtitleSize>,
    #[serde(skip_serializing_if = "Option::is_none", deserialize_with = "super::de_soft_opt")]
    pub subtitle_position: Option<SubtitlePosition>,
    #[serde(skip_serializing_if = "Option::is_none", deserialize_with = "super::de_soft_opt")]
    pub hdr_overlay_brightness: Option<HdrOverlayBrightness>,
    #[serde(skip_serializing_if = "Option::is_none", deserialize_with = "super::de_soft_opt")]
    pub next_episode_mode: Option<NextEpisodeMode>,
    #[serde(skip_serializing_if = "Option::is_none", deserialize_with = "super::de_soft_opt")]
    pub skip_interval: Option<SkipInterval>,
    #[serde(skip_serializing_if = "Option::is_none", deserialize_with = "super::de_soft_opt")]
    pub instant_dpad_seeking: Option<InstantDpadSeeking>,
    #[serde(skip_serializing_if = "Option::is_none", deserialize_with = "super::de_soft_opt")]
    pub uri_engine: Option<UriEngine>,
    #[serde(skip_serializing_if = "Option::is_none", deserialize_with = "super::de_soft_opt")]
    pub dolby_vision_mode: Option<DolbyVisionMode>,
    #[serde(skip_serializing_if = "Option::is_none", deserialize_with = "super::de_soft_opt")]
    pub intro_skip: Option<MarkerSkipMode>,
    #[serde(skip_serializing_if = "Option::is_none", deserialize_with = "super::de_soft_opt")]
    pub credits_skip: Option<MarkerSkipMode>,
    #[serde(skip_serializing_if = "Option::is_none", deserialize_with = "super::de_soft_opt")]
    pub deck_press: Option<DeckPress>,
    #[serde(skip_serializing_if = "Option::is_none", deserialize_with = "super::de_soft_opt")]
    pub under_posters: Option<UnderPosters>,
    #[serde(skip_serializing_if = "Option::is_none", deserialize_with = "super::de_soft_opt")]
    pub audio_enhancements: Option<AudioEnhancements>,
    /// Hero trailer autoplay (P1c). An absent value reads the legacy key, which itself reads ON when
    /// absent.
    #[serde(skip_serializing_if = "Option::is_none", deserialize_with = "super::de_soft_opt")]
    pub trailer_autoplay: Option<bool>,
    /// The profile's own estimated playback end-time HUD switch; absent reads the default ON.
    #[serde(skip_serializing_if = "Option::is_none", deserialize_with = "super::de_soft_opt")]
    pub hud_end_time: Option<bool>,
    /// The chosen player HUD's package id ([`HudSelectionPref`]); `None` reads the legacy install-wide key.
    #[serde(skip_serializing_if = "Option::is_none", deserialize_with = "super::de_soft_opt")]
    pub hud_selection: Option<String>,
}

impl ProfilePrefs {
    /// No setting has an own value.
    pub fn is_empty(&self) -> bool {
        *self == ProfilePrefs::default()
    }
}

/// **One profile's own values under one account stamp** — the record shape of `profile_prefs`, with
/// the same identity half as a memory-door [`ProfileList`](super::ProfileList).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ProfilePrefsRecord {
    /// The Plex Home user's `uuid`, or empty for the account owner.
    pub user: String,
    /// The owner stamp ([`crate::plex::session::account_stamp`]), or the token fingerprint while
    /// the roster names no owner. A record is visible only while this equals the signed-in
    /// account's non-empty stamp.
    #[serde(deserialize_with = "super::de_soft_string", skip_serializing_if = "String::is_empty")]
    pub account: String,
    pub prefs: ProfilePrefs,
}

impl ProfileKeyed for ProfilePrefsRecord {
    fn user(&self) -> &str {
        &self.user
    }
    fn account(&self) -> &str {
        &self.account
    }
    fn is_empty(&self) -> bool {
        self.prefs.is_empty()
    }
    fn slot(session: &Session) -> &Vec<Self> {
        &session.prefs.profile_prefs
    }
    fn slot_mut(session: &mut Session) -> &mut Vec<Self> {
        &mut session.prefs.profile_prefs
    }
}

/// The `profile_prefs` row: every profile's own values, kept across a profile switch, cleared with
/// the credentials at sign-out. A [`Row`] and not a [`Setting`]: its value is a list of records,
/// written only through [`persist_profile_pref`] and [`persist_profile_hud`].
pub struct ProfilePrefsPref;
impl Row for ProfilePrefsPref {
    const KEY: &'static str = "profile_prefs";
    const SCOPE: Scope = Scope::Profile;
    const ACCOUNT_BOUND: bool = true;
    const LIVE: bool = false;
    const SETTINGS: Option<SettingsWrite> = None;
    const PARSE: Parse = Parse::SoftVec;
}

/// A [`Setting`] a profile can own: the accessors over [`ProfilePrefs`]. Implemented for exactly the
/// rows whose `FALLBACK` is [`Fallback::LegacyInstallKey`].
pub trait ProfileSetting: Setting {
    /// The profile's own value, if it has picked one.
    fn own(p: &ProfilePrefs) -> Option<Self::Value>;
    fn set_own(p: &mut ProfilePrefs, v: Self::Value);
}

macro_rules! profile_setting {
    ($($marker:ty = $field:ident),* $(,)?) => {$(
        const _: () = assert!(matches!(<$marker as Row>::FALLBACK, Fallback::LegacyInstallKey));
        impl ProfileSetting for $marker {
            fn own(p: &ProfilePrefs) -> Option<<$marker as Setting>::Value> { p.$field }
            fn set_own(p: &mut ProfilePrefs, v: <$marker as Setting>::Value) { p.$field = Some(v) }
        }
    )*};
}
profile_setting!(
    QualityPref = playback_quality,
    DirectPlayModePref = direct_play_mode,
    SubtitleTonePref = subtitle_tone,
    SubtitleSizePref = subtitle_size,
    SubtitlePositionPref = subtitle_position,
    HdrOverlayBrightnessPref = hdr_overlay_brightness,
    NextEpisodeModePref = next_episode_mode,
    SkipIntervalPref = skip_interval,
    InstantDpadSeekingPref = instant_dpad_seeking,
    UriEnginePref = uri_engine,
    DolbyVisionModePref = dolby_vision_mode,
    IntroSkipPref = intro_skip,
    CreditsSkipPref = credits_skip,
    DeckPressPref = deck_press,
    UnderPostersPref = under_posters,
    AudioEnhancementsPref = audio_enhancements,
    TrailerAutoplayPref = trailer_autoplay,
    HudEndTimePref = hud_end_time,
);

const _: () = assert!(matches!(<HudSelectionPref as Row>::FALLBACK, Fallback::LegacyInstallKey));

/// What a profile reads for [`HudSelectionPref`]: its own pick, else the legacy install-wide key; empty is the
/// built-in. [`effective`]'s counterpart for a `String` value, which no [`Setting`] can carry.
pub fn effective_hud(legacy: &Preferences, own: Option<&ProfilePrefs>) -> String {
    own.and_then(|p| p.hud_selection.clone()).unwrap_or_else(|| legacy.hud_selection.clone())
}

/// [`effective_hud`] over a stored record, for the profile `seat` names ([`stored_effective`]'s rule).
pub fn stored_effective_hud(session: &Session, seat: &Seat) -> String {
    effective_hud(&session.prefs, own_for(session, seat))
}

/// [`put_own`] for [`HudSelectionPref`]: the same refusals (an empty stamp, a pick the profile already holds) and
/// the same pruning.
pub fn put_own_hud(records: &mut Vec<ProfilePrefsRecord>, user: &str, stamp: &str, id: &str) -> bool {
    if stamp.is_empty() || own_prefs(records, user, stamp).and_then(|p| p.hud_selection.as_deref()) == Some(id) {
        return false;
    }
    records.prune_foreign(stamp);
    let at = records.position_of(user, stamp).unwrap_or_else(|| {
        records.push(ProfilePrefsRecord { user: user.to_string(), account: stamp.to_string(), ..Default::default() });
        records.len() - 1
    });
    records[at].prefs.hud_selection = Some(id.to_string());
    records.prune_empty();
    true
}

/// The values `user` picked under the stamp `stamp`, if the stamp is one that can be read at all
/// (an empty stamp matches nothing) and a record carries it.
pub fn own_prefs<'a>(records: &'a [ProfilePrefsRecord], user: &str, stamp: &str) -> Option<&'a ProfilePrefs> {
    if stamp.is_empty() {
        return None;
    }
    records.iter().find(|r| r.user == user && r.account == stamp).map(|r| &r.prefs)
}

/// **What a profile reads for `S`**: its own value, else the legacy install-wide key (`S::get`, so
/// quality `None` is Original), which is itself the type's default when the file never had it.
pub fn effective<S: ProfileSetting>(legacy: &Preferences, own: Option<&ProfilePrefs>) -> S::Value {
    own.and_then(S::own).unwrap_or_else(|| S::get(legacy))
}

/// [`effective`] over a stored record, for the one profile `seat` names; [`Seat::Unseated`] has
/// nobody to own a value and reads the legacy one. This is the value a stored session shows (the
/// live cells may differ from it for a moment: a pick is published before it is written).
pub fn stored_effective<S: ProfileSetting>(session: &Session, seat: &Seat) -> S::Value {
    effective::<S>(&session.prefs, own_for(session, seat))
}

/// The profile's own values for `seat`. A seat stamped with this session's owner stamp also
/// reads a record an older build still has under the token fingerprint, until a write adopts it.
fn own_for<'a>(session: &'a Session, seat: &Seat) -> Option<&'a ProfilePrefs> {
    let Seat::Seated { user, stamp } = seat else { return None };
    let records = &session.prefs.profile_prefs;
    if let Some(own) = own_prefs(records, user, stamp) {
        return Some(own);
    }
    let current = super::super::account_stamp(session);
    let legacy = crate::plex::grant::account_key(&session.account_token);
    if stamp == &current && !legacy.is_empty() && legacy != current {
        own_prefs(records, user, &legacy)
    } else {
        None
    }
}

/// Copy each field `dest` has not picked from `src`. Naming every field makes a new setting a
/// compile error here.
pub(crate) fn fill_absent(dest: &mut ProfilePrefs, src: &ProfilePrefs) {
    let ProfilePrefs {
        playback_quality, direct_play_mode, subtitle_tone, subtitle_size, subtitle_position,
        hdr_overlay_brightness, next_episode_mode, skip_interval, instant_dpad_seeking, uri_engine,
        dolby_vision_mode, intro_skip, credits_skip, deck_press, under_posters, audio_enhancements,
        trailer_autoplay, hud_end_time, hud_selection,
    } = src;
    macro_rules! keep {
        ($($field:ident),* $(,)?) => {$(
            if dest.$field.is_none() { dest.$field = $field.clone(); }
        )*};
    }
    keep!(
        playback_quality, direct_play_mode, subtitle_tone, subtitle_size, subtitle_position,
        hdr_overlay_brightness, next_episode_mode, skip_interval, instant_dpad_seeking, uri_engine,
        dolby_vision_mode, intro_skip, credits_skip, deck_press, under_posters, audio_enhancements,
        trailer_autoplay, hud_end_time, hud_selection,
    );
}

/// **Make the record list hold `S = v` for (`user`, `stamp`)**, and say whether it changed. An empty
/// stamp is refused (nothing can be read back under it); a profile whose own value already is `v`
/// is not a change. Otherwise the records of every other stamp are pruned (a previous sign-in's,
/// unreadable now) and the profile's record is created if need be.
pub fn put_own<S: ProfileSetting>(records: &mut Vec<ProfilePrefsRecord>, user: &str, stamp: &str, v: S::Value) -> bool {
    if stamp.is_empty() || own_prefs(records, user, stamp).and_then(S::own) == Some(v) {
        return false;
    }
    records.prune_foreign(stamp);
    let at = records.position_of(user, stamp).unwrap_or_else(|| {
        records.push(ProfilePrefsRecord { user: user.to_string(), account: stamp.to_string(), ..Default::default() });
        records.len() - 1
    });
    S::set_own(&mut records[at].prefs, v);
    records.prune_empty();
    true
}

/// **Who a pick is for, captured on the main thread when the viewer picks**: the seat, the
/// revocation tenure and the credentials the record belonged to. The worker's write is refused when
/// any of them moved (the account-data guard of `queue_update_ticket`), so a pick queued before a
/// sign-out or a different login lands nowhere, and a pick made as profile A lands on A's record
/// whoever is seated when the write runs.
#[derive(Clone, Debug)]
pub struct ProfileWrite {
    user: String,
    stamp: String,
    tenure: u64,
    client_id: String,
}

impl ProfileWrite {
    /// The write for the profile seated NOW; `None` while [`Seat::Unseated`].
    pub fn capture() -> Option<ProfileWrite> {
        let Seat::Seated { user, stamp } = crate::plex::session::current_seat() else { return None };
        Some(ProfileWrite { user, stamp, tenure: crate::plex::session::tenure(), client_id: crate::plex::session::peek().client_id.clone() })
    }

    /// The seat the pick was made under.
    pub fn seat(&self) -> Seat {
        Seat::Seated { user: self.user.clone(), stamp: self.stamp.clone() }
    }

    pub fn user(&self) -> &str {
        &self.user
    }

    pub fn stamp(&self) -> &str {
        &self.stamp
    }

    /// Whether the record `cur` is still the one this pick was made for.
    fn admits(&self, cur: &Session) -> bool {
        let stamp_ok = !self.stamp.is_empty()
            && (super::super::account_stamp(cur) == self.stamp
                || crate::plex::grant::account_key(&cur.account_token) == self.stamp);
        crate::plex::session::tenure() == self.tenure
            && (self.client_id.is_empty() || cur.client_id == self.client_id)
            && stamp_ok
    }
}

/// **The persist half of the one write door for a per-profile setting** (`media::route::prefs::choose`
/// is the live half): make the captured profile's record hold `S = v`, as a read-modify-write under
/// the session lock, and say whether the record holds it afterwards.
///
/// The skip compares the profile's OWN value with `Some(v)`, so a first pick equal to the fallback
/// still writes (own `None` differs), the way an explicit quality pick over an absent key always
/// has: a legacy absent quality must not be reinterpreted by a later readiness change. `true` is a
/// `Durable` write, an `Ephemeral` one (a platform that keeps nothing across launches: the
/// process-local record took the edit), or no write at all because the profile already held the
/// value (`read && unchanged`). `false` is a refused or failed write, an unreadable record, a pick whose tenure or
/// credentials moved, and the revocation guard of
/// [`update_guarded_with_outcome`](super::super::update_guarded_with_outcome).
pub fn persist_profile_pref<S: ProfileSetting>(w: &ProfileWrite, v: S::Value) -> bool {
    let user = w.user.clone();
    persist_profile_with(w, move |records, stamp| put_own::<S>(records, &user, stamp, v))
}

/// [`persist_profile_pref`] for [`HudSelectionPref`]: the one durable write of the chosen HUD, under the same
/// guards (seat, tenure, credentials, an empty stamp).
pub fn persist_profile_hud(w: &ProfileWrite, id: &str) -> bool {
    let user = w.user.clone();
    let id = id.to_string();
    persist_profile_with(w, move |records, stamp| put_own_hud(records, &user, stamp, &id))
}

/// The read-modify-write both doors share: `put` edits the record list and says whether it changed.
/// The stamp it writes is the session's [`super::super::account_stamp`] after any older-build
/// token stamp has been adopted, so the write does not prune the record it just adopted.
fn persist_profile_with(w: &ProfileWrite, put: impl FnOnce(&mut Vec<ProfilePrefsRecord>, &str) -> bool) -> bool {
    let (mut read, mut unchanged) = (false, false);
    let write = crate::plex::session::update_guarded_with_outcome(
        |cur| {
            if !w.admits(cur) {
                return None;
            }
            read = true;
            let mut next = cur.clone();
            let adopted = super::adopt_legacy_token_stamps(&mut next);
            let stamp = super::super::account_stamp(&next);
            let changed = put(&mut next.prefs.profile_prefs, &stamp);
            if changed || adopted {
                Some(next)
            } else {
                unchanged = true;
                None
            }
        },
        || crate::plex::session::tenure() == w.tenure,
    );
    match write {
        // A write held in memory by design (a platform with no credential store) took the edit.
        Some(write) => matches!(write.classify(),
            crate::plex::session::async_persistence::CompletionOutcome::Durable(_)
            | crate::plex::session::async_persistence::CompletionOutcome::Ephemeral),
        None => read && unchanged,
    }
}
