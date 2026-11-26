//! **The live player's zap, as two small machines** (Live TV PR4; handoff SPEC-4). Pure: no clock read,
//! no session read, no effect performed; the screen feeds events and acts on the one effect that comes back.
//!
//! [`Zap`] is the channel being stepped to. ▲ / CH+ steps to the next higher number and ▼ / CH- to the
//! next lower, from the channel the banner shows: a press moves the target at once and the banner follows
//! it, while the stream starts [`DEBOUNCE_MS`] after the LAST press, so a run of presses opens one Plex
//! session, not one each. The old stream keeps playing (audio) through the window and is stopped by the
//! tune itself, which is the app's: a one-tuner server frees its tuner before the new tune.
//!
//! ```text
//!   Idle --Step--> Pending{target, last} --Step--> Pending{target', now}   (the window restarts)
//!   Pending --Tick, DEBOUNCE_MS after the last press--> Tuning{target}  + Effect::Tune(target)
//!   Tuning --Step--> Pending{next of target, now}     (a press after the tune fired: a new window)
//!   Tuning --Session: the session plays target and its first frame is up--> Idle
//!   Tuning --Session: the session plays something else, a frame later--> Idle   (the tune was refused)
//! ```
//!
//! [`Art`] is the picture drawn over the video plane's hole until a first frame exists: covering from the
//! first press (owner decision Q10) through the debounce and the tune until the first frame, fading out
//! then. It is told only whether the picture is wanted now; the fade is its own.
use plx_data::livetv::Channel;

/// How long after the last press the stream starts (SPEC-4).
pub const DEBOUNCE_MS: u32 = 600;
/// How long the art takes to fade out when the first frame arrives. SPEC-4 says only "fades out when the
/// video arrives"; the duration is this PR's default, close to the banner's own fade.
pub const ART_FADE_MS: u32 = 300;

/// A channel by identity: its server, DVR and key, never its number.
pub fn same(a: &Channel, b: &Channel) -> bool {
    a.sid == b.sid && a.dvr == b.dvr && a.channel == b.channel
}

/// The zap target, as the screen's machine.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum Zap {
    #[default]
    Idle,
    /// presses are being collected; `last` is the time of the latest
    Pending { target: Channel, last: u32 },
    /// the tune was asked for at `asked`; the target stands until the session shows it
    Tuning { target: Channel, asked: u32 },
}

/// What happened to the machine.
#[derive(Clone, Copy, Debug)]
pub enum ZapEvent<'a> {
    /// ▲ / CH+ (`dir` +1) or ▼ / CH- (-1) at `now`, over `lineup` (in number order), the session playing `playing`
    Step { dir: i32, lineup: &'a [Channel], playing: Option<&'a Channel>, now: u32 },
    /// time passing
    Tick { now: u32 },
    /// what the session plays and whether its first frame is up
    Session { playing: Option<&'a Channel>, framed: bool, now: u32 },
}

/// What the screen must do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ZapEffect {
    None,
    /// start the stream of this channel (`PlayerReq::Tune`)
    Tune(Channel),
}

impl Zap {
    /// The channel stepped to, while one is: what the banner and the art follow.
    pub fn target(&self) -> Option<&Channel> {
        match self {
            Zap::Idle => None,
            Zap::Pending { target, .. } | Zap::Tuning { target, .. } => Some(target),
        }
    }

    pub fn is_idle(&self) -> bool {
        matches!(self, Zap::Idle)
    }

    pub fn step(&mut self, ev: ZapEvent<'_>) -> ZapEffect {
        match ev {
            ZapEvent::Step { dir, lineup, playing, now } => {
                let Some(from) = self.target().or(playing) else { return ZapEffect::None };
                let Some(at) = lineup.iter().position(|c| same(c, from)) else { return ZapEffect::None };
                if lineup.len() < 2 || !matches!(dir, -1 | 1) {
                    return ZapEffect::None;
                }
                let next = if dir < 0 { at.checked_sub(1).unwrap_or(lineup.len() - 1) } else { (at + 1) % lineup.len() };
                *self = Zap::Pending { target: lineup[next].clone(), last: now };
                ZapEffect::None
            }
            ZapEvent::Tick { now } => match self {
                Zap::Pending { target, last } if now.wrapping_sub(*last) >= DEBOUNCE_MS => {
                    let target = target.clone();
                    *self = Zap::Tuning { target: target.clone(), asked: now };
                    ZapEffect::Tune(target)
                }
                _ => ZapEffect::None,
            },
            ZapEvent::Session { playing, framed, now } => {
                if let Zap::Tuning { target, asked } = self {
                    let landed = playing.is_some_and(|c| same(c, target));
                    // refused: a frame after the ask the session still plays another channel
                    if (landed && framed) || (!landed && now.wrapping_sub(*asked) > 0) {
                        *self = Zap::Idle;
                    }
                }
                ZapEffect::None
            }
        }
    }
}

/// The art ground's phase.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Art {
    #[default]
    Clear,
    /// the picture is wanted: drawn whole
    Covering,
    /// the picture is no longer wanted: fading out since `since`
    Fading { since: u32 },
}

impl Art {
    /// One step: `wanted` is whether a picture must stand over the hole now (a zap in flight, or a live
    /// stream with no frame yet).
    pub fn step(&mut self, wanted: bool, now: u32) {
        *self = match (*self, wanted) {
            (_, true) => Art::Covering,
            (Art::Covering, false) => Art::Fading { since: now },
            (Art::Fading { since }, false) if now.wrapping_sub(since) >= ART_FADE_MS => Art::Clear,
            (other, false) => other,
        };
    }

    /// How much of the picture is drawn at `now`: 1 covering, a linear fade, 0 clear.
    pub fn alpha(self, now: u32) -> f32 {
        match self {
            Art::Clear => 0.0,
            Art::Covering => 1.0,
            Art::Fading { since } => (1.0 - now.wrapping_sub(since) as f32 / ART_FADE_MS as f32).clamp(0.0, 1.0),
        }
    }
}

/// The banner's phase: never shown, or shown until a deadline (then it fades, [`live::FADE_MS`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Banner {
    #[default]
    Hidden,
    Shown { until: u32 },
}

/// What the screen reads off the session and the lineup this frame, handed to [`Live::tick`].
#[derive(Clone, Copy, Debug)]
pub struct Observed<'a> {
    pub now: u32,
    /// the channel the session is tuned to (the lineup's entry for it, else the published guide facts)
    pub playing: Option<&'a Channel>,
    pub lineup: &'a [Channel],
    /// the session has no frame on the panel yet (resolving, connecting or buffering, or no engine)
    pub no_frame: bool,
}

/// **The live player's whole decision state**: the zap, the art ground and the banner, as three named
/// phases. Stepped from keys ([`Self::press`], [`Self::reveal`]) and from the frame ([`Self::tick`]); never
/// reads a clock or the session itself.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Live {
    pub zap: Zap,
    pub art: Art,
    pub banner: Banner,
    /// the channel the banner was last raised for, by identity (server, DVR, key): a new one is a tune
    seen: Option<(plx_plex::plex::ServerId, String, String)>,
}

impl Live {
    /// ▲ / CH+ (`dir` +1) or ▼ / CH- (-1): move the target, raise the banner on it and cover with its art.
    pub fn press(&mut self, dir: i32, o: &Observed<'_>) {
        self.zap.step(ZapEvent::Step { dir, lineup: o.lineup, playing: o.playing, now: o.now });
        if !self.zap.is_idle() {
            self.art.step(true, o.now);
            self.reveal(o.now);
        }
    }

    /// Raise the banner for [`live::SHOW_MS`] more (Left, Right, OK, a tune, a press).
    pub fn reveal(&mut self, now: u32) {
        self.banner = Banner::Shown { until: now.wrapping_add(plx_appkit::player_hud::live::SHOW_MS) };
    }

    /// One frame: the session's answer to the zap, the art's phase, the banner's on a tune, then the
    /// debounce. Returns the tune to ask for, if the window has just closed.
    pub fn tick(&mut self, o: &Observed<'_>) -> ZapEffect {
        self.zap.step(ZapEvent::Session { playing: o.playing, framed: !o.no_frame, now: o.now });
        let was_covering = self.art == Art::Covering;
        let wanted = !self.zap.is_idle() || o.no_frame;
        self.art.step(wanted, o.now);
        // compared in place: a frame allocates nothing, only a tune stores the new identity
        let changed = match (o.playing, &self.seen) {
            (Some(c), Some((sid, dvr, key))) => c.sid != *sid || c.dvr != *dvr || c.channel != *key,
            (None, None) => false,
            _ => true,
        };
        if changed {
            self.seen = o.playing.map(|c| (c.sid, c.dvr.clone(), c.channel.clone()));
            if self.seen.is_some() {
                self.reveal(o.now);
            }
        }
        // the first frame of a tune: the banner's four seconds start when the picture does
        if was_covering && !wanted {
            self.reveal(o.now);
        }
        self.zap.step(ZapEvent::Tick { now: o.now })
    }

    /// The channel the banner and the art follow: the zap's target while one stands, else the session's.
    pub fn shown<'a>(&'a self, playing: Option<&'a Channel>) -> Option<&'a Channel> {
        self.zap.target().or(playing)
    }

    /// How opaque the banner is at `now`: held while a zap is pending or the picture is not there yet.
    pub fn banner_alpha(&self, now: u32) -> f32 {
        match self.banner {
            Banner::Hidden => 0.0,
            Banner::Shown { until } => {
                plx_appkit::player_hud::live::alpha(now, until, !self.zap.is_idle() || self.art == Art::Covering)
            }
        }
    }

    pub fn art_alpha(&self, now: u32) -> f32 {
        self.art.alpha(now)
    }

    /// A term for the frame gate: changes whenever what is drawn can change with nothing else moving (a
    /// phase edge, the banner's deadline passing, a fade running, the shown channel or its airing).
    pub fn fingerprint(&self, now: u32, shown: Option<&Channel>) -> u64 {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        let mut mix = |v: u64| h = (h ^ v).wrapping_mul(0x0100_0000_01b3);
        mix(match self.zap { Zap::Idle => 0, Zap::Pending { .. } => 1, Zap::Tuning { .. } => 2 });
        mix(match self.art { Art::Clear => 0, Art::Covering => 1, Art::Fading { .. } => 2 });
        let alpha = self.banner_alpha(now);
        mix(match self.banner { Banner::Hidden => 0, Banner::Shown { .. } => 1 + u64::from(alpha > 0.0) });
        // a fade is a frame per tick for as long as it runs; its start is the deadline passing
        let fading = (alpha > 0.0 && alpha < 1.0) || matches!(self.art, Art::Fading { .. });
        if fading {
            mix(u64::from(now));
        }
        if let Banner::Shown { until } = self.banner {
            mix(u64::from(until));
        }
        if let Some(c) = shown {
            mix(c.channel.len() as u64);
            mix(c.begins_at as u64);
            mix(c.ends_at as u64);
        }
        h
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ch(n: &str) -> Channel {
        Channel { sid: plx_plex::plex::ServerId::UNSET, dvr: "7".into(), channel: format!("C{n}"), vcn: n.into(),
            name: format!("Station {n}"), thumb: String::new(), now: format!("Show {n}"), now_thumb: String::new(),
            now_summary: String::new(), begins_at: 0, ends_at: 0 }
    }

    fn lineup() -> Vec<Channel> { ["101", "102", "103", "104"].map(ch).to_vec() }

    #[test]
    fn three_presses_inside_the_window_tune_once_the_third_target() {
        let lineup = lineup();
        let playing = ch("101");
        let mut zap = Zap::default();
        for now in [1_000, 1_200, 1_400] {
            assert_eq!(zap.step(ZapEvent::Step { dir: 1, lineup: &lineup, playing: Some(&playing), now }), ZapEffect::None);
            // the window has not elapsed since the latest press
            assert_eq!(zap.step(ZapEvent::Tick { now: now + 599 }), ZapEffect::None, "at {now}");
        }
        assert_eq!(zap.target().map(|c| c.vcn.as_str()), Some("104"));
        assert_eq!(zap.step(ZapEvent::Tick { now: 1_400 + DEBOUNCE_MS }), ZapEffect::Tune(ch("104")));
        // the machine tunes once: further ticks ask nothing
        assert_eq!(zap.step(ZapEvent::Tick { now: 3_000 }), ZapEffect::None);
    }

    #[test]
    fn a_press_after_the_tune_fired_starts_a_new_window_from_the_tuned_target() {
        let lineup = lineup();
        let playing = ch("101");
        let mut zap = Zap::default();
        zap.step(ZapEvent::Step { dir: 1, lineup: &lineup, playing: Some(&playing), now: 0 });
        assert!(matches!(zap.step(ZapEvent::Tick { now: 600 }), ZapEffect::Tune(_)));
        // the session still plays 101 (the tune has not landed): the next press steps from the TARGET
        zap.step(ZapEvent::Step { dir: 1, lineup: &lineup, playing: Some(&playing), now: 700 });
        assert_eq!(zap.target().map(|c| c.vcn.as_str()), Some("103"));
        assert_eq!(zap.step(ZapEvent::Tick { now: 1_299 }), ZapEffect::None);
        assert_eq!(zap.step(ZapEvent::Tick { now: 1_300 }), ZapEffect::Tune(ch("103")));
    }

    #[test]
    fn up_is_the_next_higher_number_and_both_directions_wrap() {
        let lineup = lineup();
        let mut zap = Zap::default();
        zap.step(ZapEvent::Step { dir: -1, lineup: &lineup, playing: Some(&ch("101")), now: 0 });
        assert_eq!(zap.target().map(|c| c.vcn.as_str()), Some("104"), "below the lowest wraps to the highest");
        zap.step(ZapEvent::Step { dir: 1, lineup: &lineup, playing: Some(&ch("101")), now: 1 });
        assert_eq!(zap.target().map(|c| c.vcn.as_str()), Some("101"), "above the highest wraps to the lowest");
    }

    #[test]
    fn a_channel_the_lineup_does_not_list_or_a_lineup_of_one_steps_nowhere() {
        let mut zap = Zap::default();
        zap.step(ZapEvent::Step { dir: 1, lineup: &lineup(), playing: Some(&ch("999")), now: 0 });
        assert!(zap.is_idle());
        zap.step(ZapEvent::Step { dir: 1, lineup: &[ch("101")], playing: Some(&ch("101")), now: 0 });
        assert!(zap.is_idle());
        zap.step(ZapEvent::Step { dir: 1, lineup: &lineup(), playing: None, now: 0 });
        assert!(zap.is_idle());
    }

    #[test]
    fn the_target_stands_until_the_session_shows_its_first_frame() {
        let lineup = lineup();
        let mut zap = Zap::default();
        zap.step(ZapEvent::Step { dir: 1, lineup: &lineup, playing: Some(&ch("101")), now: 0 });
        zap.step(ZapEvent::Tick { now: 600 });
        let target = ch("102");
        zap.step(ZapEvent::Session { playing: Some(&target), framed: false, now: 700 });
        assert_eq!(zap.target().map(|c| c.vcn.as_str()), Some("102"), "tuned but no frame yet");
        zap.step(ZapEvent::Session { playing: Some(&target), framed: true, now: 900 });
        assert!(zap.is_idle());
    }

    #[test]
    fn a_tune_the_app_did_not_take_ends_the_zap_a_frame_later() {
        let lineup = lineup();
        let old = ch("101");
        let mut zap = Zap::default();
        zap.step(ZapEvent::Step { dir: 1, lineup: &lineup, playing: Some(&old), now: 0 });
        zap.step(ZapEvent::Tick { now: 600 });
        zap.step(ZapEvent::Session { playing: Some(&old), framed: true, now: 600 });
        assert!(!zap.is_idle(), "the ask and the session read share a frame: the answer is not in yet");
        zap.step(ZapEvent::Session { playing: Some(&old), framed: true, now: 617 });
        assert!(zap.is_idle());
    }

    #[test]
    fn the_art_covers_from_the_first_press_and_fades_when_no_longer_wanted() {
        let mut art = Art::default();
        assert_eq!(art.alpha(0), 0.0);
        art.step(true, 10);
        assert_eq!((art, art.alpha(10)), (Art::Covering, 1.0));
        art.step(false, 100);
        assert_eq!(art, Art::Fading { since: 100 });
        assert!(art.alpha(100 + ART_FADE_MS / 2) > 0.4 && art.alpha(100 + ART_FADE_MS / 2) < 0.6);
        art.step(false, 100 + ART_FADE_MS);
        assert_eq!(art, Art::Clear);
        // wanted again mid-fade: back to whole at once
        let mut art = Art::Fading { since: 5 };
        art.step(true, 50);
        assert_eq!(art, Art::Covering);
    }
}
