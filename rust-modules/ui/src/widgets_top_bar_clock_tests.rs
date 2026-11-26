//! **The bar's clock** (Live TV handoff SPEC-1 item 2: "right edge on --margin-x (96), centred on the
//! top bar. BODY 28 regular, --text-secondary, tabular figures, soft text shadow. Never focusable"):
//! where its box stands, what the draw paints, and when the strip leaves it no room.

use super::*;
use crate::draw_census::capture_runs;

const FACE: &CStr = c"21:38";

fn drawn(clock: ClockRead<'_>) -> Vec<crate::draw_census::Run> {
    let _serial = plx_base::testlock::serial();
    capture_runs(|| draw_top_bar_clock(Painter::recording(), clock))
}

/// The box: its right edge is the safe frame's margin (96 in from 1920), and it is as tall as, and
/// stands on, the bar's pill row (y 62, h 60), so its centre is the bar's: 92.
#[test]
fn the_clock_box_ends_on_the_margin_and_is_centred_on_the_bar() {
    let frame = top_bar_clock_frame(76.0);
    assert_eq!(frame.x + frame.w, 1824.0, "the right edge is SCR_W - MARGIN_X");
    assert_eq!(frame.x + frame.w, crate::consts::SCR_W - crate::consts::MARGIN_X);
    assert_eq!((frame.y, frame.h), (62.0, 60.0), "the bar's pill row: --top-bar-y, --control-h");
    assert_eq!(frame.y + frame.h * 0.5, 92.0, "centred on the bar");
    assert_eq!((frame.y, frame.h), (TOP_BAR_Y, TAB_PILL_H));
}

/// What the draw paints: BODY 28, regular, `TEXT_SECONDARY`, anchored RIGHT on 1824, its cap band
/// centred on 92, with a soft shadow run (the caption's token, one pixel low) under it.
#[test]
fn the_clock_is_body_regular_text_secondary_right_aligned_with_a_soft_shadow() {
    let runs = drawn(ClockRead { text: FACE, w: 76.0 });
    assert_eq!(runs.len(), 2, "the shadow and the ink: {runs:?}");
    let (shadow, ink) = (&runs[0], &runs[1]);
    assert_eq!(ink.text, "21:38");
    assert_eq!(ink.size, 28);
    assert_eq!(ink.size, crate::theme::size::BODY);
    assert!(!ink.bold, "regular");
    assert_eq!(ink.ink, crate::theme::TEXT_SECONDARY);
    assert_eq!(ink.align, 2, "right-aligned");
    assert_eq!(ink.at.x, 1824.0, "its right edge is the margin");
    let (cap_top, baseline) = plx_gfx::text::text_cap_band(28, 0);
    assert_eq!(ink.at.y + (cap_top + baseline) * 0.5, 92.0, "the cap band is centred on the bar");

    assert_eq!(shadow.text, ink.text);
    assert_eq!(shadow.ink, crate::caption_policy::CAPTION_SHADOW, "the caption's soft shadow token");
    assert_eq!((shadow.size, shadow.align, shadow.bold), (ink.size, ink.align, ink.bold));
    assert_eq!(shadow.at.x, ink.at.x);
    assert_eq!(shadow.at.y - ink.at.y, crate::caption_policy::CAPTION_SHADOW_DY);
}

/// The face's digits share one advance (the face's tabular figures), which is what keeps a
/// right-aligned run still when the minute changes: the draw anchors on the RIGHT edge whatever the
/// measured width, so two faces of one width stand on the same left edge, and the box follows the width.
#[test]
fn the_right_edge_does_not_depend_on_the_faces_width() {
    for (text, w) in [(c"9:05", 62.0), (c"21:38", 76.0), (c"12:00\u{202f}AM", 130.0)] {
        let runs = drawn(ClockRead { text, w });
        assert!(runs.iter().all(|run| run.at.x == 1824.0), "{text:?}: {runs:?}");
        assert_eq!(top_bar_clock_frame(w).x + w, 1824.0);
    }
}

/// The room rule, the mirror of `chip_name_budget`: the clock fits when it stands `BAND_AIR` beyond
/// the centred track's right edge. A track grown to `TAB_VIEW_MAX` ends at 1732, 92 px short of the
/// margin: a 24-hour face fits there with air to spare, a 12-hour one does not, and the bar leaves it
/// out rather than eliding it or overlapping the track.
#[test]
fn the_clock_fits_beside_the_track_or_is_left_out() {
    let widest_track = TAB_VIEW_MAX + 2.0 * TAB_TRACK_PAD;
    assert_eq!((crate::consts::SCR_W + widest_track) * 0.5, 1732.0, "the widest track's right edge");
    assert!(top_bar_clock_fits(widest_track, 70.0), "a 24-hour face clears even the widest track");
    assert!(!top_bar_clock_fits(widest_track, 125.0), "a 12-hour face does not");
    assert!(top_bar_clock_fits(700.0, 125.0), "an ordinary strip leaves a 12-hour face room");
    // exactly BAND_AIR of air is enough; a pixel less is not
    let w = TOP_BAR_CLOCK_RIGHT - (crate::consts::SCR_W + 700.0) * 0.5 - BAND_AIR;
    assert!(top_bar_clock_fits(700.0, w));
    assert!(!top_bar_clock_fits(700.0, w + 1.0));
}
