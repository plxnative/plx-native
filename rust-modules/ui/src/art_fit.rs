//! `art_fit` -- how a card's picture meets its tile when the two do not share a shape.
//!
//! A tile's art has always been COVERED: the picture fills the tile and the overflow is cropped
//! ([`crate::widgets::art_uv`]). That is right for a poster in a poster tile and for the art the
//! server made to size, and wrong for art that is not poster-shaped at all: a square programme image
//! in Live TV's On now row lost its sides and its baked-in title ("Space: New Frontiers" was cut off).
//! The owner's decision (2026-11-26, over PR #1557's default Q11): "square should fit with blurred bg".
//!
//! [`ArtFit`] is that choice, declared on the row ([`crate::card_row::RowStyle::fit`]) and read by the
//! shared card draw, never decided by a screen:
//!
//! - [`ArtFit::Cover`]: today's fit, for every row but Live TV's posters.
//! - [`ArtFit::ContainOnBlur`]: a picture within [`POSTER_FIT_TOLERANCE`] of the tile's own aspect is
//!   still covered (a 2:3 poster draws exactly as before). Any other picture is CONTAINED: the whole
//!   image, centred, its aspect kept, over a backdrop that is the same image cover-filled to the tile
//!   and blurred, and dimmed ([`crate::theme::ART_BACKDROP_TINT`]) so the contained image reads. The
//!   tile's rounded corners, shadow, sheen, focus scale and resume bar are the tile's, drawn once
//!   around both layers by the shared card pass, exactly as for a covered picture.
//!
//! [`place`] is that decision, pure over the source size and the tile rect.
//!
//! **The blur is made once per image, never per frame.** [`backdrop_of`] reduces the decoded
//! picture to a tiny blurred copy ([`BACKDROP_SHORT_SIDE`] texels on the short side) when the picture
//! arrives, on the main thread in the poster adapter's `drain_decoded`, from at most
//! [`TAPS_PER_AXIS`]^2 taps per output texel plus one 3 x 3 box pass over the tiny result (tens of
//! microseconds). The adapter asks only for art requested at the poster tile's box (never a logo, a
//! still, a backdrop or a headshot), and a picture within the tolerance of 2:3 gets none. The copy
//! rides in [`crate::tex::Decoded::backdrop`], is uploaded with the image and lives and dies in the
//! same cache entry ([`crate::tex::TexCache`]); drawing a tile is two textured quads and no render
//! pass. The GPU upscales it bilinearly, which is the rest of the blur. It costs
//! [`BACKDROP_SHORT_SIDE`] x up to [`BACKDROP_LONG_SIDE_MAX`] x 4 bytes (1 KiB for a square, 1.75 KiB
//! for 16:9, 4 KiB at the most) beside the picture's own ~375 KB.
use crate::tex::Decoded;
use crate::{Crop, Rect};

/// How far a picture's aspect may sit from its tile's and still be COVERED rather than contained:
/// the picture's aspect over the tile's lies in `1 +/- 0.08`. A 2:3 tile takes 0.61 - 0.72 as its own,
/// which holds every real poster (the server's 2:3 artwork, a 1000x1500 scan, a cover a few pixels
/// off) and nothing a viewer would call square or landscape. The edge is inclusive.
///
/// Cropping within the tolerance loses at most 8% of one axis, split evenly over both sides; beyond
/// it the crop starts to cut into titles and faces, which is the defect this exists to end.
pub const POSTER_FIT_TOLERANCE: f32 = 0.08;

/// The aspect [`backdrop_of`] judges a picture against when it makes the blurred copy: a poster's
/// 2:3 ([`crate::widgets::POSTER_RES`]). The producer does not know which tile will draw the image,
/// so it is conservative: a picture [`place`] could contain in ANY poster tile gets a copy, and a
/// picture within the tolerance of 2:3 (which every poster-tile fit covers) gets none.
const POSTER_ASPECT: f32 = 2.0 / 3.0;

/// Texels on the short side of the blurred copy. The GPU's bilinear upscale of this to a 250 x 375
/// tile spreads each texel over ~15 px, which is the blur's radius: 16 reads as a colour field with
/// no recognisable shape, which is what a backdrop behind a contained picture should be.
pub const BACKDROP_SHORT_SIDE: u32 = 16;

/// Texels on the long side, at the most: a panorama is not given a wider copy than it can use.
pub const BACKDROP_LONG_SIDE_MAX: u32 = 64;

/// The most taps [`backdrop_of`] takes per output texel along one axis. A 667 x 375 picture reduced
/// to 28 x 16 spans ~24 source texels per output texel; sampling 4 x 4 of them on an even grid is
/// plenty for a picture that is blurred twice more (the 3 x 3 pass, then the GPU's upscale), and
/// bounds the work to `28 * 16 * 16` taps whatever the picture's size.
const TAPS_PER_AXIS: usize = 4;

/// The least mean opacity a picture may have to be given a backdrop: a cut-out (a transparent PNG
/// logo) has no picture to blur, and a blurred copy of its texels' colour is meaningless.
const OPAQUE_MEAN: f32 = 0.98;

/// How a tile's art meets its box ([`crate::card_row::RowStyle::fit`]).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum ArtFit {
    /// Fill the tile and crop the overflow: every row but Live TV's posters.
    #[default]
    Cover,
    /// Cover a picture of the tile's own shape; contain any other over its own blurred copy.
    ContainOnBlur,
}

/// Where a picture goes in its tile: what [`place`] answers and the card draw paints.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Placement {
    /// One quad filling the tile, sampling the `uv` window ([`crate::widgets::art_uv`]'s answer).
    Cover { uv: [f32; 4] },
    /// Two quads: the blurred copy over the whole tile (its `backdrop_uv` window), then the picture
    /// whole in `inner`, centred in the tile with its aspect kept.
    Contained { inner: Rect, backdrop_uv: [f32; 4] },
}

/// Is a `w` x `h` picture within [`POSTER_FIT_TOLERANCE`] of aspect `aspect` (w / h)? An empty
/// picture is within it (the undecoded answer is cover's whole texture).
fn within_tolerance(aspect: f32, w: f32, h: f32) -> bool {
    if w <= 0.0 || h <= 0.0 || aspect <= 0.0 {
        return true;
    }
    ((w / h) / aspect - 1.0).abs() <= POSTER_FIT_TOLERANCE
}

/// **The placement of a `tw` x `th` picture in the tile `r`** under `fit`. `crop` is the cover's
/// ([`crate::widgets::art_crop`]); `has_backdrop` says whether the picture came with its blurred
/// copy ([`crate::widgets::ArtImage::backdrop`] is not 0): without one a picture is covered, as it
/// was before this existed, and never drawn contained on a bare ground.
pub fn place(fit: ArtFit, crop: Crop, tw: f32, th: f32, r: Rect, has_backdrop: bool) -> Placement {
    let cover = r.cover_uv(tw, th, crop);
    if fit != ArtFit::ContainOnBlur || !has_backdrop || r.w <= 0.0 || r.h <= 0.0 || within_tolerance(r.w / r.h, tw, th) {
        return Placement::Cover { uv: cover };
    }
    let k = (r.w / tw).min(r.h / th);
    let (w, h) = (tw * k, th * k);
    Placement::Contained { inner: Rect::new(r.x + (r.w - w) * 0.5, r.y + (r.h - h) * 0.5, w, h), backdrop_uv: cover }
}

/// The size of the blurred copy of a `w` x `h` picture: [`BACKDROP_SHORT_SIDE`] on the short side,
/// the same aspect on the long one (at most [`BACKDROP_LONG_SIDE_MAX`]).
fn backdrop_size(w: usize, h: usize) -> (usize, usize) {
    let short = BACKDROP_SHORT_SIDE as usize;
    let long = ((short * w.max(h)) as f32 / w.min(h) as f32).round() as usize;
    let long = long.clamp(short, BACKDROP_LONG_SIDE_MAX as usize);
    if w >= h { (long, short) } else { (short, long) }
}

/// **The blurred copy of a decoded picture**, or `None` when it needs none: the picture is within the
/// tolerance of a poster's 2:3 (it is always covered), it is not a whole opaque picture (a cut-out,
/// a short or empty buffer), or it has no size. Pure over the pixels; no GL.
///
/// Each output texel is the mean of an even grid of at most [`TAPS_PER_AXIS`]^2 source texels under
/// it, and one 3 x 3 box pass (edges clamped) smooths the result. The copy is opaque and keeps the
/// picture's aspect, so the tile's cover window ([`Placement::Contained::backdrop_uv`]) addresses it
/// as it addresses the picture.
pub fn backdrop_of(d: &Decoded) -> Option<Decoded> {
    let (w, h) = (d.w as usize, d.h as usize);
    if w == 0 || h == 0 || d.rgba.len() < w * h * 4 || within_tolerance(POSTER_ASPECT, w as f32, h as f32) {
        return None;
    }
    // A copy is smaller than its picture or it is not a blur of it: a thumbnail this small is its own.
    if w.min(h) <= BACKDROP_SHORT_SIDE as usize {
        return None;
    }
    let (bw, bh) = backdrop_size(w, h);
    let (span_x, span_y) = (w as f32 / bw as f32, h as f32 / bh as f32);
    let (nx, ny) = ((span_x.ceil() as usize).clamp(1, TAPS_PER_AXIS), (span_y.ceil() as usize).clamp(1, TAPS_PER_AXIS));
    let mut sum = vec![[0.0f32; 3]; bw * bh];
    let mut opacity = 0.0f32;
    for j in 0..bh {
        for i in 0..bw {
            let acc = &mut sum[j * bw + i];
            for ty in 0..ny {
                let y = (((j as f32 + (ty as f32 + 0.5) / ny as f32) * span_y) as usize).min(h - 1);
                for tx in 0..nx {
                    let x = (((i as f32 + (tx as f32 + 0.5) / nx as f32) * span_x) as usize).min(w - 1);
                    let p = &d.rgba[(y * w + x) * 4..][..4];
                    for k in 0..3 {
                        acc[k] += p[k] as f32;
                    }
                    opacity += p[3] as f32 / 255.0;
                }
            }
            let n = (nx * ny) as f32;
            for c in acc.iter_mut() {
                *c /= n;
            }
        }
    }
    if opacity / ((bw * bh * nx * ny) as f32) < OPAQUE_MEAN {
        return None;
    }
    let mut rgba = Vec::with_capacity(bw * bh * 4);
    for j in 0..bh {
        for i in 0..bw {
            let mut m = [0.0f32; 3];
            for dy in -1i32..=1 {
                for dx in -1i32..=1 {
                    let (x, y) = ((i as i32 + dx).clamp(0, bw as i32 - 1) as usize, (j as i32 + dy).clamp(0, bh as i32 - 1) as usize);
                    for k in 0..3 {
                        m[k] += sum[y * bw + x][k];
                    }
                }
            }
            rgba.extend(m.map(|c| (c / 9.0).round().clamp(0.0, 255.0) as u8));
            rgba.push(255);
        }
    }
    Some(Decoded { w: bw as u16, h: bh as u16, rgba: rgba.into_boxed_slice(), backdrop: None })
}

#[cfg(test)]
mod tests {
    use super::*;
    use plx_gfx::gfx::UV_FULL;

    const TILE: Rect = Rect { x: 100.0, y: 200.0, w: 250.0, h: 375.0 };

    fn near(a: f32, b: f32) -> bool { (a - b).abs() < 1e-3 }
    fn near4(a: [f32; 4], b: [f32; 4]) -> bool { a.iter().zip(b).all(|(x, y)| near(*x, y)) }

    fn px(w: u32, h: u32, f: impl Fn(u32, u32) -> [u8; 4]) -> Decoded {
        let mut v = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h { for x in 0..w { v.extend_from_slice(&f(x, y)); } }
        Decoded { w: w as u16, h: h as u16, rgba: v.into_boxed_slice(), backdrop: None }
    }

    #[test]
    fn a_square_is_contained_centred_over_a_full_tile_backdrop() {
        let Placement::Contained { inner, backdrop_uv } = place(ArtFit::ContainOnBlur, Crop::Centre, 375.0, 375.0, TILE, true)
        else { panic!("a square is not poster-shaped: it must be contained") };
        assert!(near(inner.w, 250.0) && near(inner.h, 250.0), "whole image, aspect kept: {inner:?}");
        assert!(near(inner.cx(), TILE.cx()) && near(inner.cy(), TILE.cy()), "centred in the tile: {inner:?}");
        assert!(near4(backdrop_uv, TILE.cover_uv(375.0, 375.0, Crop::Centre)), "the backdrop covers the whole tile: {backdrop_uv:?}");
        assert!(backdrop_uv[2] < 1.0 && near(backdrop_uv[3], 1.0), "cover of a square in a poster crops the sides: {backdrop_uv:?}");
    }

    #[test]
    fn a_16_by_9_is_contained_centred_over_a_full_tile_backdrop() {
        let Placement::Contained { inner, backdrop_uv } = place(ArtFit::ContainOnBlur, Crop::Centre, 667.0, 375.0, TILE, true)
        else { panic!("16:9 is not poster-shaped: it must be contained") };
        assert!(near(inner.w, 250.0), "the width is the limit: {inner:?}");
        assert!(near(inner.h, 250.0 * 375.0 / 667.0), "aspect kept: {inner:?}");
        assert!(near(inner.cx(), TILE.cx()) && near(inner.cy(), TILE.cy()), "centred in the tile: {inner:?}");
        assert!(near4(backdrop_uv, TILE.cover_uv(667.0, 375.0, Crop::Centre)));
    }

    #[test]
    fn a_poster_shaped_source_keeps_the_cover_fit_and_no_backdrop() {
        for (tw, th) in [(250.0, 375.0), (500.0, 750.0), (400.0, 600.0)] {
            assert_eq!(place(ArtFit::ContainOnBlur, Crop::Centre, tw, th, TILE, true), Placement::Cover { uv: UV_FULL }, "{tw}x{th}");
        }
        let slightly = place(ArtFit::ContainOnBlur, Crop::Centre, 262.0, 375.0, TILE, true);
        assert_eq!(slightly, Placement::Cover { uv: TILE.cover_uv(262.0, 375.0, Crop::Centre) }, "today's cover crop, unchanged");
    }

    #[test]
    fn the_tolerance_edge_is_inclusive() {
        let h = 1000.0;
        let w_at = |k: f32| h * (2.0 / 3.0) * k;
        let covered = |k: f32| matches!(place(ArtFit::ContainOnBlur, Crop::Centre, w_at(k), h, TILE, true), Placement::Cover { .. });
        assert!(covered(1.0 + POSTER_FIT_TOLERANCE - 0.001), "just inside, wide side");
        assert!(covered(1.0 - POSTER_FIT_TOLERANCE + 0.001), "just inside, narrow side");
        assert!(!covered(1.0 + POSTER_FIT_TOLERANCE + 0.001), "just outside, wide side");
        assert!(!covered(1.0 - POSTER_FIT_TOLERANCE - 0.001), "just outside, narrow side");
        assert_eq!(POSTER_FIT_TOLERANCE, 0.08);
    }

    #[test]
    fn cover_fit_never_contains_and_a_missing_backdrop_falls_back_to_cover() {
        assert!(matches!(place(ArtFit::Cover, Crop::Centre, 375.0, 375.0, TILE, true), Placement::Cover { .. }));
        assert!(matches!(place(ArtFit::ContainOnBlur, Crop::Centre, 375.0, 375.0, TILE, false), Placement::Cover { .. }));
        assert_eq!(place(ArtFit::ContainOnBlur, Crop::Centre, 0.0, 0.0, TILE, true), Placement::Cover { uv: UV_FULL }, "undecoded");
    }

    /// The decision is about SHAPE, so a focus pop (the same tile, scaled) cannot flip it.
    #[test]
    fn the_pop_scale_does_not_change_the_decision() {
        for s in [1.0, 1.09] {
            let r = TILE.scaled(s);
            assert!(matches!(place(ArtFit::ContainOnBlur, Crop::Centre, 375.0, 375.0, r, true), Placement::Contained { .. }), "{s}");
            assert!(matches!(place(ArtFit::ContainOnBlur, Crop::Centre, 250.0, 375.0, r, true), Placement::Cover { .. }), "{s}");
        }
    }

    #[test]
    fn the_backdrop_of_a_square_and_a_16_by_9_is_tiny_and_keeps_the_aspect() {
        let sq = backdrop_of(&px(375, 375, |_, _| [200, 100, 50, 255])).expect("a square needs one");
        assert_eq!((sq.w, sq.h), (16, 16));
        let wide = backdrop_of(&px(667, 375, |_, _| [200, 100, 50, 255])).expect("16:9 needs one");
        assert_eq!((wide.w, wide.h), (28, 16));
        assert_eq!(wide.rgba.len(), 28 * 16 * 4);
        assert!(wide.backdrop.is_none());
        let tall = backdrop_of(&px(200, 600, |_, _| [9, 9, 9, 255])).expect("1:3 needs one");
        assert_eq!((tall.w, tall.h), (16, 48));
        let pano = backdrop_of(&px(2000, 100, |_, _| [9, 9, 9, 255])).expect("a panorama needs one");
        assert_eq!((pano.w, pano.h), (64, 16), "the long side is capped");
    }

    #[test]
    fn a_poster_shaped_or_cut_out_or_malformed_image_has_no_backdrop() {
        assert!(backdrop_of(&px(250, 375, |_, _| [1, 2, 3, 255])).is_none(), "poster-shaped");
        assert!(backdrop_of(&px(300, 300, |_, _| [1, 2, 3, 0])).is_none(), "a cut-out has no ground to blur");
        let mut short = px(300, 300, |_, _| [1, 2, 3, 255]);
        short.rgba = vec![0; 100].into_boxed_slice();
        assert!(backdrop_of(&short).is_none(), "a buffer shorter than w*h*4");
        assert!(backdrop_of(&px(0, 0, |_, _| [0; 4])).is_none(), "no size");
        assert!(backdrop_of(&px(2, 2, |_, _| [1, 2, 3, 255])).is_none(), "a copy no smaller than the picture is no blur of it");
        assert!(backdrop_of(&px(16, 30, |_, _| [1, 2, 3, 255])).is_none(), "the short side is already the copy's");
        assert!(backdrop_of(&px(17, 30, |_, _| [1, 2, 3, 255])).is_some(), "one texel more is a picture to reduce");
    }

    #[test]
    fn the_backdrop_is_the_blurred_mean_not_a_crop() {
        let flat = backdrop_of(&px(300, 300, |_, _| [200, 100, 50, 255])).unwrap();
        assert!(flat.rgba.chunks(4).all(|p| p == [200, 100, 50, 255]), "a flat image blurs to itself, opaque");
        let split = backdrop_of(&px(320, 320, |x, _| if x < 160 { [255, 0, 0, 255] } else { [0, 0, 255, 255] })).unwrap();
        let row = |x: usize| split.rgba[(8 * 16 + x) * 4..][..3].to_vec();
        assert_eq!(row(0)[0], 255, "far left stays red");
        assert_eq!(row(15)[2], 255, "far right stays blue");
        let (a, b) = (row(7), row(8));
        assert!(a[0] > 0 && a[2] > 0 && b[0] > 0 && b[2] > 0, "the edge is smeared across texels: {a:?} {b:?}");
    }
}
