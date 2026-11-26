//! Number entry's machine, graded through its events (Live TV PR5; handoff SPEC-5).
use super::*;

/// A lineup where `201` is a channel AND the prefix of six more, with 2010 on screen.
fn board() -> Vec<Cand> {
    ["201", "2010", "2011", "2012", "2013", "2014", "2015", "3001", "3002", "44"]
        .map(|n| Cand { number: n.to_string(), playing: n == "2010" })
        .to_vec()
}

/// Type each digit of `s` 100 ms apart from `t0`: the last answer and the time of the last key.
fn type_in(m: &mut NumberEntry, lineup: &[Cand], s: &str, t0: u32) -> (Fx, u32) {
    let mut fx = Fx::None;
    let mut t = t0;
    for (k, c) in s.chars().enumerate() {
        t = t0 + 100 * k as u32;
        fx = m.step(Ev::Digit { d: c as u8 - b'0', now: t }, lineup);
    }
    (fx, t)
}

/// The panel as it opens: `first` is the digit that opened it; its time is the first event's.
fn opened(first: char, lineup: &[Cand]) -> (NumberEntry, Fx) {
    let mut m = NumberEntry::open(first as u8 - b'0');
    let fx = m.step(Ev::Touch { now: 1_000 }, lineup);
    (m, fx)
}

#[test]
fn digits_filter_the_lineup_by_prefix_in_lineup_order() {
    let l = board();
    assert_eq!(matches(&l, "201").collect::<Vec<_>>(), vec![0, 1, 2, 3, 4, 5, 6]);
    assert_eq!(matches(&l, "2010").collect::<Vec<_>>(), vec![1]);
    assert_eq!(matches(&l, "30").collect::<Vec<_>>(), vec![7, 8]);
    assert_eq!(matches(&l, "2099").count(), 0);
    assert_eq!(matches(&l, "1").count(), 0, "a prefix, never a substring");
}

#[test]
fn a_prefix_that_fits_exactly_one_channel_tunes_at_once() {
    let l = board();
    // 3 fits 3001 and 3002: nothing tunes while the digits fit two
    let (mut m, fx) = opened('3', &l);
    assert!(matches!(fx, Fx::Seat(_)), "{fx:?}");
    let (fx, _) = type_in(&mut m, &l, "00", 1_100);
    assert!(matches!(fx, Fx::Seat(_)), "300 still fits two: {fx:?}");
    // 3002 fits one
    assert_eq!(m.step(Ev::Digit { d: 2, now: 1_400 }, &l), Fx::Tune(8));
    assert_eq!(m, NumberEntry::Done);
    // a short prefix of a single channel tunes at the first digit: 4 is only 44
    let (m, fx) = opened('4', &l);
    assert_eq!(fx, Fx::Tune(9));
    assert_eq!(m, NumberEntry::Done);
}

#[test]
fn a_number_that_is_the_channel_on_screen_tunes_nothing() {
    let l = board();
    let (mut m, _) = opened('2', &l);
    let (fx, _) = type_in(&mut m, &l, "010", 1_100);
    assert_eq!(fx, Fx::Leave, "2010 fits only the channel already on: the panel closes, the stream is not restarted");
    assert_eq!(m, NumberEntry::Done);
}

#[test]
fn an_exact_match_that_is_also_a_prefix_waits_for_the_timeout() {
    let l = board();
    let (mut m, _) = opened('2', &l);
    let (fx, t) = type_in(&mut m, &l, "01", 1_100);
    assert_eq!(fx, Fx::Seat(0), "201 is a channel and the prefix of six more: the highlight seats on it, nothing tunes");
    assert_eq!(m.step(Ev::Tick { now: t + IDLE_MS - 1 }, &l), Fx::None, "not before 2.5 s");
    assert_eq!(m.step(Ev::Tick { now: t + IDLE_MS }, &l), Fx::Tune(0), "at 2.5 s the highlighted match tunes");
    assert_eq!(m, NumberEntry::Done);
}

#[test]
fn an_ambiguous_prefix_never_tunes_on_a_digit() {
    let l = board();
    let (mut m, fx) = opened('2', &l);
    assert!(matches!(fx, Fx::Seat(_)));
    let (fx, _) = type_in(&mut m, &l, "0", 1_100);
    assert!(matches!(fx, Fx::Seat(_)), "20 fits seven: {fx:?}");
    assert!(matches!(m, NumberEntry::Typing { .. }));
}

#[test]
fn the_timeout_tunes_the_highlight_and_restarts_on_every_key() {
    let l = board();
    let (mut m, _) = opened('3', &l);
    // 3 fits 3001 / 3002; the highlight is the first (3001, index 7)
    assert_eq!(m.highlight(), Some(7));
    // a digit restarts the clock: 30, then wait
    let (_, t) = type_in(&mut m, &l, "0", 2_000);
    assert_eq!(m.step(Ev::Tick { now: t + IDLE_MS - 1 }, &l), Fx::None);
    // the cursor moving restarts it
    assert_eq!(m.step(Ev::Moved { to: 8, now: t + 2_000 }, &l), Fx::None);
    assert_eq!(m.step(Ev::Tick { now: t + 2_000 + IDLE_MS - 1 }, &l), Fx::None);
    // any other key restarts it too
    assert_eq!(m.step(Ev::Touch { now: t + 4_000 }, &l), Fx::None);
    assert_eq!(m.step(Ev::Tick { now: t + 4_000 + IDLE_MS - 1 }, &l), Fx::None);
    // 2.5 s after the last key the HIGHLIGHT tunes: 3002, where the cursor was moved
    assert_eq!(m.step(Ev::Tick { now: t + 4_000 + IDLE_MS }, &l), Fx::Tune(8));
}

#[test]
fn the_highlight_starts_on_the_exact_match_else_the_first_that_is_not_playing() {
    let l = board();
    // 20: no channel is 20; 2010 is playing, so the first match that is not playing is 201 (index 0)
    let (mut m, _) = opened('2', &l);
    assert_eq!(type_in(&mut m, &l, "0", 1_100).0, Fx::Seat(0));
    assert_eq!(m.highlight(), Some(0));
    // a lineup whose first match IS the playing channel: the highlight skips it
    let l2: Vec<Cand> = ["500", "501", "502"].map(|n| Cand { number: n.to_string(), playing: n == "500" }).to_vec();
    let (m, fx) = opened('5', &l2);
    assert_eq!((fx, m.highlight()), (Fx::Seat(1), Some(1)), "500 is on screen: OK must not be a silent no-op");
    // the exact match wins: 1 is a channel and the prefix of two more
    let l3: Vec<Cand> = ["10", "1", "11"].map(|n| Cand { number: n.to_string(), playing: false }).to_vec();
    let (m, fx) = opened('1', &l3);
    assert_eq!((fx, m.highlight()), (Fx::Seat(1), Some(1)), "the exact match, though 10 comes first");
}

#[test]
fn digits_that_match_nothing_wait_then_close_and_never_tune() {
    let l = board();
    let (mut m, fx) = opened('2', &l);
    assert!(matches!(fx, Fx::Seat(_)));
    let (fx, t) = type_in(&mut m, &l, "099", 1_100);
    assert_eq!((fx, m.digits()), (Fx::None, "2099"));
    assert_eq!(matches(&l, m.digits()).count(), 0);
    assert_eq!(m.step(Ev::Ok { now: t + 10 }, &l), Fx::None, "OK on 'No channel 2099' does nothing");
    assert!(matches!(m, NumberEntry::Typing { .. }));
    assert_eq!(m.step(Ev::Tick { now: t + 10 + IDLE_MS }, &l), Fx::Leave, "the timeout closes a panel that has nothing to tune");
    assert_eq!(m, NumberEntry::Done);
}

#[test]
fn back_cancels_at_every_point() {
    let l = board();
    let mut fresh = NumberEntry::open(2);
    assert_eq!(fresh.step(Ev::Back, &l), Fx::Leave);
    assert_eq!(fresh, NumberEntry::Done);
    let (mut m, _) = opened('3', &l);
    assert_eq!(m.step(Ev::Back, &l), Fx::Leave);
    assert_eq!(m.step(Ev::Tick { now: 99_999 }, &l), Fx::None, "a cancelled entry tunes nothing at its timeout");
    // after an exact-prefix wait too
    let (mut m, _) = opened('2', &l);
    let (_, t) = type_in(&mut m, &l, "01", 1_100);
    assert_eq!(m.step(Ev::Back, &l), Fx::Leave);
    assert_eq!(m.step(Ev::Tick { now: t + IDLE_MS }, &l), Fx::None);
}

#[test]
fn ok_tunes_the_highlight_and_a_playing_highlight_only_closes() {
    let l = board();
    let (mut m, _) = opened('2', &l);
    type_in(&mut m, &l, "0", 1_100);
    // move the cursor to 2012 (index 3), then OK
    assert_eq!(m.step(Ev::Moved { to: 3, now: 1_500 }, &l), Fx::None);
    assert_eq!(m.step(Ev::Ok { now: 1_600 }, &l), Fx::Tune(3));
    assert_eq!(m, NumberEntry::Done);
    // OK on the channel that is on screen closes without a retune
    let (mut m, _) = opened('2', &l);
    type_in(&mut m, &l, "0", 1_100);
    m.step(Ev::Moved { to: 1, now: 1_500 }, &l);
    assert_eq!(m.step(Ev::Ok { now: 1_600 }, &l), Fx::Leave);
}

#[test]
fn digits_past_the_longest_number_are_ignored() {
    let l = board();
    assert_eq!(longest(&l), 4);
    let (mut m, _) = opened('9', &l);
    type_in(&mut m, &l, "99999", 1_100);
    assert_eq!(m.digits(), "9999", "the lineup has no number longer than four digits");
}

#[test]
fn the_opening_digit_takes_the_time_of_the_first_event() {
    let l = board();
    let mut m = NumberEntry::open(3);
    assert_eq!(m.digits(), "", "before any event");
    // a second digit arriving in the same frame as the opening one is not lost: 3 then 0
    assert_eq!(m.step(Ev::Digit { d: 0, now: 5_000 }, &l), Fx::Seat(7));
    assert_eq!(m.digits(), "30");
    // a Tick is the first event: the opening digit is read at its time
    let mut m = NumberEntry::open(3);
    assert_eq!(m.step(Ev::Tick { now: 7_000 }, &l), Fx::Seat(7));
    assert_eq!(m.digits(), "3");
    assert_eq!(m.step(Ev::Tick { now: 7_000 + IDLE_MS }, &l), Fx::Tune(7));
}

#[test]
fn a_finished_entry_asks_for_nothing() {
    let l = board();
    let (mut m, fx) = opened('4', &l);
    assert_eq!(fx, Fx::Tune(9));
    for ev in [Ev::Digit { d: 1, now: 2 }, Ev::Ok { now: 2 }, Ev::Back, Ev::Tick { now: 99_999 }, Ev::Touch { now: 3 }] {
        assert_eq!(m.step(ev, &l), Fx::None, "{ev:?}");
    }
}
