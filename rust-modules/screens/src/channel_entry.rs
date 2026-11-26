//! **Number entry, as one small machine** (Live TV PR5; handoff SPEC-5). Pure: no clock read, no focus read, no
//! effect performed; the panel (`channel_entry_panel`) feeds events and acts on the one effect that comes back.
//!
//! The viewer types a channel number anywhere (on the What's on page, or over the live player). The digits narrow
//! the lineup to the channels whose number STARTS with them, and the panel lists those matches under the typed
//! number. Nothing tunes while the digits still fit more than one channel: the picture keeps playing.
//!
//! ```text
//!   Fresh{first} --any event at `now`--> Typing{first, ..}                      (the opening digit rides on the open)
//!   Typing --Digit--> Typing{digits + d}      the highlight restarts on the match the digits name, the timer restarts
//!   Typing --Digit, the digits now fit ONE channel--> Done + Tune(it)           (Leave when that channel is the one on)
//!   Typing --Moved{to}--> Typing{highlight: to}    (the focus engine moved the cursor: up / down)
//!   Typing --Ok--> Done + Tune(highlight)          (Leave when that channel is the one on; nothing when no channel matches)
//!   Typing --Tick, IDLE_MS after the last key--> Done + Tune(highlight)   (Leave when nothing matches, or it is the one on)
//!   Fresh | Typing --Back--> Done + Leave          (no tune at any point)
//! ```
//!
//! The highlight is a channel of the lineup, held by its index, never a row of the shown list: a digit that
//! narrows the list moves no highlight by position. It starts on the exact match when there is one, else on the
//! first match that is not the channel already playing, so OK on the opening highlight is never a silent no-op;
//! the playing channel carries a tag for the same reason.
//!
//! An exact match that is also the prefix of a longer number (`201` with `2010` in the lineup) only seats the
//! highlight and waits for [`IDLE_MS`]: tuning it at once would make `2010` unreachable.

/// How long after the last key the highlighted match tunes (SPEC-5).
pub const IDLE_MS: u32 = 2_500;
/// How many matches the panel's list shows at once; the rest scroll under it (SPEC-5: "a window of five").
pub const LIST_ROWS: usize = 5;

/// One channel of the lineup, as the machine reads it: its number and whether it is the one already on screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cand {
    pub number: String,
    pub playing: bool,
}

/// The number entry's phase.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NumberEntry {
    /// The panel has just opened with the digit that opened it; no event has carried a time yet.
    Fresh { first: u8 },
    /// Digits are being collected. `highlight` is a lineup index and `last` the time of the latest key.
    Typing { digits: String, highlight: usize, last: u32 },
    /// The panel has asked to leave (a tune went out, or a dismissal): nothing more is asked.
    Done,
}

/// What happened to the machine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ev {
    Digit { d: u8, now: u32 },
    /// The focus engine moved the cursor to lineup index `to` (▲ ▼ on the list).
    Moved { to: usize, now: u32 },
    /// Any other key: the idle timer restarts.
    Touch { now: u32 },
    Ok { now: u32 },
    Back,
    Tick { now: u32 },
}

impl Ev {
    /// The time the event carries (none for BACK).
    pub fn now(&self) -> Option<u32> {
        match *self {
            Ev::Digit { now, .. } | Ev::Moved { now, .. } | Ev::Touch { now } | Ev::Ok { now } | Ev::Tick { now } => Some(now),
            Ev::Back => None,
        }
    }
}

/// What the panel must do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fx {
    None,
    /// put the cursor on this lineup index (the digits changed the list)
    Seat(usize),
    /// tune this lineup index, then leave
    Tune(usize),
    /// leave with no tune
    Leave,
}

/// The lineup indices whose number starts with `digits`, in lineup order.
pub fn matches<'a>(lineup: &'a [Cand], digits: &'a str) -> impl Iterator<Item = usize> + 'a {
    lineup.iter().enumerate().filter(move |(_, c)| c.number.starts_with(digits)).map(|(i, _)| i)
}

/// Where the highlight starts for `digits`: the exact match, else the first match that is not playing, else the first.
pub fn start_highlight(lineup: &[Cand], digits: &str) -> Option<usize> {
    let all: Vec<usize> = matches(lineup, digits).collect();
    all.iter().copied().find(|&i| lineup[i].number == digits)
        .or_else(|| all.iter().copied().find(|&i| !lineup[i].playing))
        .or_else(|| all.first().copied())
}

/// The longest number of the lineup, in characters: the most digits worth typing.
pub fn longest(lineup: &[Cand]) -> usize {
    lineup.iter().map(|c| c.number.chars().count()).max().unwrap_or(0)
}

impl NumberEntry {
    pub fn open(first: u8) -> Self {
        NumberEntry::Fresh { first }
    }

    /// The digits typed so far (empty before the first event).
    pub fn digits(&self) -> &str {
        match self {
            NumberEntry::Typing { digits, .. } => digits,
            NumberEntry::Fresh { .. } | NumberEntry::Done => "",
        }
    }

    /// The highlighted lineup index while typing and something matches.
    pub fn highlight(&self) -> Option<usize> {
        match self {
            NumberEntry::Typing { highlight, digits, .. } => Some(*highlight).filter(|_| !digits.is_empty()),
            NumberEntry::Fresh { .. } | NumberEntry::Done => None,
        }
    }

    /// Step the machine on `ev` over the lineup the panel captured when it opened.
    pub fn step(&mut self, ev: Ev, lineup: &[Cand]) -> Fx {
        // The opening digit is read at the time of the first event that carries one, so the machine never
        // reads a clock. BACK before any event is just a cancel.
        if let (NumberEntry::Fresh { first }, Some(now)) = (&*self, ev.now()) {
            let first = *first;
            *self = NumberEntry::Typing { digits: String::new(), highlight: 0, last: now };
            let opened = self.digit(first, now, lineup);
            if *self == NumberEntry::Done {
                return opened;
            }
            // the opening digit answered; the event that woke it is handled as it would have been after it
            let then = self.step_typing(ev, lineup);
            return if then == Fx::None { opened } else { then };
        }
        self.step_typing(ev, lineup)
    }

    fn step_typing(&mut self, ev: Ev, lineup: &[Cand]) -> Fx {
        let NumberEntry::Typing { digits, highlight, last } = self else {
            return match (&*self, ev) {
                (NumberEntry::Fresh { .. }, Ev::Back) => {
                    *self = NumberEntry::Done;
                    Fx::Leave
                }
                _ => Fx::None,
            };
        };
        match ev {
            Ev::Back => {
                *self = NumberEntry::Done;
                Fx::Leave
            }
            Ev::Digit { d, now } => {
                if digits.chars().count() >= longest(lineup) {
                    *last = now; // ignored, but still a key
                    return Fx::None;
                }
                self.digit(d, now, lineup)
            }
            Ev::Moved { to, now } => {
                *last = now;
                if lineup.get(to).is_some_and(|c| c.number.starts_with(digits.as_str())) {
                    *highlight = to;
                }
                Fx::None
            }
            Ev::Touch { now } => {
                *last = now;
                Fx::None
            }
            Ev::Ok { now } => {
                *last = now;
                if matches(lineup, digits).next().is_none() {
                    return Fx::None;
                }
                let h = *highlight;
                self.finish(h, lineup)
            }
            Ev::Tick { now } => {
                if now.wrapping_sub(*last) < IDLE_MS {
                    return Fx::None;
                }
                if matches(lineup, digits).next().is_none() {
                    *self = NumberEntry::Done;
                    return Fx::Leave;
                }
                let h = *highlight;
                self.finish(h, lineup)
            }
        }
    }

    /// Append digit `d` at `now`: one match tunes at once, several seat the highlight, none changes nothing else.
    fn digit(&mut self, d: u8, now: u32, lineup: &[Cand]) -> Fx {
        let NumberEntry::Typing { digits, highlight, last } = self else { return Fx::None };
        digits.push(char::from(b'0' + d.min(9)));
        *last = now;
        let (a, b) = {
            let mut it = matches(lineup, digits);
            (it.next(), it.next())
        };
        match (a, b) {
            (None, _) => Fx::None,
            (Some(only), None) => self.finish(only, lineup),
            (Some(_), Some(_)) => {
                let h = start_highlight(lineup, digits).unwrap_or(0);
                *highlight = h;
                Fx::Seat(h)
            }
        }
    }

    /// Leave having chosen lineup index `i`: a tune, unless that channel is the one already on screen.
    fn finish(&mut self, i: usize, lineup: &[Cand]) -> Fx {
        *self = NumberEntry::Done;
        if lineup.get(i).is_some_and(|c| c.playing) { Fx::Leave } else { Fx::Tune(i) }
    }
}

#[cfg(test)]
#[path = "channel_entry_tests.rs"]
mod tests;
