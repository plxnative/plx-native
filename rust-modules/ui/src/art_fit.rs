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
//!   tile's rounded corners, shadow, focus scale and resume bar are the tile's, and a FOCUSED tile
//!   is lit (rim glow, sheen crown, gloss) as one card pass around both layers, exactly as a covered
//!   picture is. At rest the picture is a second quad over the copy's card pass, so its square edge
//!   covers the constant 1px sheen along the sides where it touches the tile.
//!
//! [`place`] is that decision, pure over the source size and the tile rect.
//!
//! **The blur is made once per image, never per frame.** [`backdrop_of`] reduces the decoded
//! picture to a tiny blurred copy (a few texels on the short side) when the picture arrives, on the
//! main thread in the poster adapter's `drain_decoded`, from at most [`TAPS_PER_AXIS`]^2 taps per
//! output texel plus one 3 x 3 box pass over the tiny result. That is a few thousand taps over a
//! ~375 KB buffer: **unmeasured**, estimated at a few hundred microseconds on the television's
//! ARMv7 (the strided reads miss the cache more than the arithmetic costs), once per image.
//!
//! **A request asks for the copy** ([`BackdropSpec`], carried by the request that names the art:
//! [`crate::tex::Source::probe`]), so only art a row declares it will contain pays for one: never a
//! logo, a still, a backdrop or a headshot, and not a Cover row's poster. The copy rides in
//! [`crate::tex::Decoded::backdrop`], is uploaded with the image and lives and dies in the same
//! cache entry ([`crate::tex::TexCache`]); drawing a tile is two textured quads (one pass when
//! focused: [`crate::Painter::tex_carded_inset`]) and no render pass. The GPU upscales the copy
//! bilinearly, which is the rest of the blur. A poster's copy is [`POSTER_BACKDROP_SIDE`] x up to
//! four times that x 4 bytes (1 KiB for a square, 1.75 KiB for 16:9, 4 KiB at the most) beside the
//! picture's own ~375 KB. The size is the request's, so another surface (a full-screen blurred
//! ground) asks for the copy size that suits it.
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

/// Texels on the short side of a POSTER's blurred copy. The GPU's bilinear upscale of this to a
/// 250 x 375 tile spreads each texel over ~15 px, which is the blur's radius: 16 reads as a colour
/// field with no recognisable shape, which is what a backdrop behind a contained picture should be.
pub const POSTER_BACKDROP_SIDE: u8 = 16;

/// How many times the short side the long side of a copy may be: a panorama is not given a wider
/// copy than it can use.
const LONG_SIDE_RATIO: usize = 4;

/// The most taps [`backdrop_of`] takes per output texel along one axis. A 667 x 375 picture reduced
/// to 28 x 16 spans ~24 source texels per output texel; sampling 4 x 4 of them on an even grid is
/// plenty for a picture that is blurred twice more (the 3 x 3 pass, then the GPU's upscale), and
/// bounds the work to `28 * 16 * 16` taps whatever the picture's size.
const TAPS_PER_AXIS: usize = 4;

/// How a tile's art meets its box ([`crate::card_row::RowStyle::fit`]).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum ArtFit {
    /// Fill the tile and crop the overflow: every row but Live TV's posters.
    #[default]
    Cover,
    /// Cover a picture of the tile's own shape; contain any other over its own blurred copy.
    ContainOnBlur,
}

impl ArtFit {
    /// The blurred copy a request for art drawn under this fit asks for: none under
    /// [`Cover`](Self::Cover), a poster's under [`ContainOnBlur`](Self::ContainOnBlur) (the only
    /// tile that contains is a poster tile).
    pub const fn copy_request(self) -> Option<BackdropSpec> {
        match self {
            Self::Cover => None,
            Self::ContainOnBlur => Some(BackdropSpec::POSTER),
        }
    }
}

/// The blurred copy a request for a picture asks the source to make of it. The request declares
/// it ([`crate::tex::Source::probe`]), so the producer never guesses from a size which consumers
/// will draw it; it is part of the art's identity in the source (the same art asked for with and
/// without a copy is two entries), and `Copy`/`Eq` so it can be.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct BackdropSpec {
    /// Texels on the copy's short side; the long side keeps the picture's aspect, at most
    /// [`LONG_SIDE_RATIO`] times as long.
    pub short_side: u8,
    /// A picture within [`POSTER_FIT_TOLERANCE`] of this shape (`w`, `h`) is one the tile covers
    /// whatever the copy, so it is given none; `None` makes a copy of every picture.
    pub skip_shape: Option<(u16, u16)>,
}

impl BackdropSpec {
    /// A poster tile's: [`POSTER_BACKDROP_SIDE`] texels, none for a picture within the tolerance of
    /// 2:3 (a poster-tile fit covers it, and so does [`place`] for any 2:3 tile).
    pub const POSTER: Self = Self { short_side: POSTER_BACKDROP_SIDE, skip_shape: Some((2, 3)) };

    /// Does a `w` x `h` picture get a copy under this spec?
    pub fn wanted_for(self, w: u16, h: u16) -> bool {
        self.skip_shape.is_none_or(|(sw, sh)| !within_tolerance(sw as f32, sh as f32, w as f32, h as f32))
    }
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

/// Is a `w` x `h` picture within [`POSTER_FIT_TOLERANCE`] of the shape `rw` x `rh`? Cross-multiplied
/// (`|w / h - rw / rh| <= tol * rw / rh`), so the edge is decided on products of the sizes and not
/// on two divisions' rounding. An empty picture is within it (the undecoded answer is cover's whole
/// texture).
fn within_tolerance(rw: f32, rh: f32, w: f32, h: f32) -> bool {
    if w <= 0.0 || h <= 0.0 || rw <= 0.0 || rh <= 0.0 {
        return true;
    }
    (w * rh - h * rw).abs() <= POSTER_FIT_TOLERANCE * h * rw
}

/// **The placement of a `tw` x `th` picture in the tile `r`** under `fit`. `crop` is the cover's
/// ([`crate::widgets::art_crop`]); `has_backdrop` says whether the picture came with its blurred
/// copy ([`crate::widgets::ArtImage::backdrop`] is not 0): without one a picture is covered, as it
/// was before this existed, and never drawn contained on a bare ground.
pub fn place(fit: ArtFit, crop: Crop, tw: f32, th: f32, r: Rect, has_backdrop: bool) -> Placement {
    let cover = r.cover_uv(tw, th, crop);
    if fit != ArtFit::ContainOnBlur || !has_backdrop || r.w <= 0.0 || r.h <= 0.0 || within_tolerance(r.w, r.h, tw, th) {
        return Placement::Cover { uv: cover };
    }
    Placement::Contained { inner: r.contain(tw, th), backdrop_uv: cover }
}

/// The size of the blurred copy of a `w` x `h` picture: `short` texels on the short side, the same
/// aspect on the long one (at most [`LONG_SIDE_RATIO`] times `short`).
fn backdrop_size(w: usize, h: usize, short: usize) -> (usize, usize) {
    let long = ((short * w.max(h)) as f32 / w.min(h) as f32).round() as usize;
    let long = long.clamp(short, short * LONG_SIDE_RATIO);
    if w >= h { (long, short) } else { (short, long) }
}

/// `picture` with the blurred copy `spec` asks for, when the picture gets one
/// ([`BackdropSpec::wanted_for`], [`backdrop_of`]). The producer's one call.
pub fn attach_backdrop(mut picture: Decoded, spec: BackdropSpec) -> Decoded {
    if spec.wanted_for(picture.w, picture.h) {
        picture.backdrop = backdrop_of(&picture, spec.short_side as usize).map(Box::new);
    }
    picture
}

/// **The blurred copy of a decoded picture**, `short` texels on its short side, or `None` when it
/// has none to make: a short or empty buffer, no size, or a picture no larger than the copy. Pure
/// over the pixels; no GL.
///
/// Each output texel is the mean of an even grid of at most [`TAPS_PER_AXIS`]^2 source texels under
/// it (not `card_art::resample`'s full-area box mean, which would read every texel of a ~375 KB
/// picture on the main thread, and would need the buffer copied into its `Rgba`), and one 3 x 3 box
/// pass (edges clamped) smooths the result. **Colour is weighted by coverage** (alpha), so a picture
/// with a transparent ground (a PNG, as some channel art links serve it) blurs to its
/// own ink and not to whatever RGB sits under its transparent texels; a texel with no coverage
/// around it takes the picture's overall colour. An opaque picture is a plain mean. The copy is
/// opaque and keeps the picture's aspect, so the tile's cover window
/// ([`Placement::Contained::backdrop_uv`]) addresses it as it addresses the picture.
pub fn backdrop_of(d: &Decoded, short: usize) -> Option<Decoded> {
    let (w, h) = (d.w as usize, d.h as usize);
    // A copy is smaller than its picture or it is not a blur of it: a thumbnail this small is its own.
    if short == 0 || w <= short || h <= short || d.rgba.len() < w * h * 4 {
        return None;
    }
    let (bw, bh) = backdrop_size(w, h, short);
    let (span_x, span_y) = (w as f32 / bw as f32, h as f32 / bh as f32);
    let (nx, ny) = ((span_x.ceil() as usize).clamp(1, TAPS_PER_AXIS), (span_y.ceil() as usize).clamp(1, TAPS_PER_AXIS));
    // Per output texel: the coverage-weighted colour sum (r, g, b) and the coverage sum.
    let mut sum = vec![[0.0f32; 4]; bw * bh];
    for j in 0..bh {
        for i in 0..bw {
            let acc = &mut sum[j * bw + i];
            for ty in 0..ny {
                let y = (((j as f32 + (ty as f32 + 0.5) / ny as f32) * span_y) as usize).min(h - 1);
                for tx in 0..nx {
                    let x = (((i as f32 + (tx as f32 + 0.5) / nx as f32) * span_x) as usize).min(w - 1);
                    let p = &d.rgba[(y * w + x) * 4..][..4];
                    let a = p[3] as f32 / 255.0;
                    for k in 0..3 {
                        acc[k] += p[k] as f32 * a;
                    }
                    acc[3] += a;
                }
            }
        }
    }
    let all = sum.iter().fold([0.0f32; 4], |t, s| [t[0] + s[0], t[1] + s[1], t[2] + s[2], t[3] + s[3]]);
    let overall = if all[3] > 0.0 { [all[0] / all[3], all[1] / all[3], all[2] / all[3]] } else { [0.0; 3] };
    let mut rgba = Vec::with_capacity(bw * bh * 4);
    for j in 0..bh {
        for i in 0..bw {
            let mut m = [0.0f32; 4];
            for dy in -1i32..=1 {
                for dx in -1i32..=1 {
                    let (x, y) = ((i as i32 + dx).clamp(0, bw as i32 - 1) as usize, (j as i32 + dy).clamp(0, bh as i32 - 1) as usize);
                    for k in 0..4 {
                        m[k] += sum[y * bw + x][k];
                    }
                }
            }
            let c = if m[3] > 1e-3 { [m[0] / m[3], m[1] / m[3], m[2] / m[3]] } else { overall };
            rgba.extend(c.map(|c| c.round().clamp(0.0, 255.0) as u8));
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

    /// The tolerance edge ITSELF, on exact sizes: against the 250 x 375 tile 72 x 100 is 1.08 of the
    /// tile's aspect and 920 x 1500 is 0.92 of it, the two edges, and both are covered (inclusive);
    /// one texel past either is contained.
    #[test]
    fn the_tolerance_edge_is_inclusive() {
        let covered = |w: f32, h: f32| matches!(place(ArtFit::ContainOnBlur, Crop::Centre, w, h, TILE, true), Placement::Cover { .. });
        assert!(covered(72.0, 100.0), "exactly 1.08, wide edge");
        assert!(!covered(73.0, 100.0), "past the wide edge");
        assert!(covered(920.0, 1500.0), "exactly 0.92, narrow edge");
        assert!(!covered(919.0, 1500.0), "past the narrow edge");
        assert!(covered(1080.0, 1500.0), "wide edge at another size");
        assert_eq!(POSTER_FIT_TOLERANCE, 0.08);
    }

    /// The producer's own judgement (`BackdropSpec::POSTER`'s `skip_shape`, 2:3) is the same edge.
    #[test]
    fn a_poster_spec_skips_exactly_what_a_poster_tile_covers() {
        let spec = BackdropSpec::POSTER;
        assert!(!spec.wanted_for(72, 100) && spec.wanted_for(73, 100), "wide edge");
        assert!(!spec.wanted_for(920, 1500) && spec.wanted_for(919, 1500), "narrow edge");
        assert!(!spec.wanted_for(250, 375) && spec.wanted_for(375, 375) && spec.wanted_for(667, 375));
        let any = BackdropSpec { short_side: 16, skip_shape: None };
        assert!(any.wanted_for(250, 375), "a spec with no shape to skip wants every picture");
        assert_eq!(ArtFit::Cover.copy_request(), None);
        assert_eq!(ArtFit::ContainOnBlur.copy_request(), Some(BackdropSpec::POSTER));
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
        let sq = backdrop_of(&px(375, 375, |_, _| [200, 100, 50, 255]), 16).expect("a square needs one");
        assert_eq!((sq.w, sq.h), (16, 16));
        let wide = backdrop_of(&px(667, 375, |_, _| [200, 100, 50, 255]), 16).expect("16:9 needs one");
        assert_eq!((wide.w, wide.h), (28, 16));
        assert_eq!(wide.rgba.len(), 28 * 16 * 4);
        assert!(wide.backdrop.is_none());
        let tall = backdrop_of(&px(200, 600, |_, _| [9, 9, 9, 255]), 16).expect("1:3 needs one");
        assert_eq!((tall.w, tall.h), (16, 48));
        let pano = backdrop_of(&px(2000, 100, |_, _| [9, 9, 9, 255]), 16).expect("a panorama needs one");
        assert_eq!((pano.w, pano.h), (64, 16), "the long side is capped");
    }

    #[test]
    fn a_malformed_or_tiny_picture_has_no_backdrop() {
        let mut short = px(300, 300, |_, _| [1, 2, 3, 255]);
        short.rgba = vec![0; 100].into_boxed_slice();
        assert!(backdrop_of(&short, 16).is_none(), "a buffer shorter than w*h*4");
        assert!(backdrop_of(&px(0, 0, |_, _| [0; 4]), 16).is_none(), "no size");
        assert!(backdrop_of(&px(2, 2, |_, _| [1, 2, 3, 255]), 16).is_none(), "a copy no smaller than the picture is no blur of it");
        assert!(backdrop_of(&px(16, 30, |_, _| [1, 2, 3, 255]), 16).is_none(), "the short side is already the copy's");
        assert!(backdrop_of(&px(17, 30, |_, _| [1, 2, 3, 255]), 16).is_some(), "one texel more is a picture to reduce");
        assert!(backdrop_of(&px(300, 300, |_, _| [1, 2, 3, 255]), 0).is_none(), "a copy of no texels");
    }

    /// The size is the request's: another consumer asks for its own, and the long side keeps its ratio.
    #[test]
    fn the_copy_size_is_the_requests() {
        let d = px(667, 375, |_, _| [5, 6, 7, 255]);
        let big = backdrop_of(&d, 32).expect("a 32-texel copy");
        assert_eq!((big.w, big.h), (57, 32));
        let small = backdrop_of(&d, 8).expect("an 8-texel copy");
        assert_eq!((small.w, small.h), (14, 8));
        let pano = backdrop_of(&px(3000, 100, |_, _| [5, 6, 7, 255]), 8).unwrap();
        assert_eq!((pano.w, pano.h), (32, 8), "the long side is at most 4x the short");
    }

    /// `attach_backdrop` applies the spec's own skip: a 2:3 poster is handed back untouched, a square
    /// gets its copy, and a spec without a shape to skip copies the poster shape too.
    #[test]
    fn attach_backdrop_makes_the_copy_only_for_a_picture_the_spec_wants_one_for() {
        let poster = attach_backdrop(px(250, 375, |_, _| [1, 2, 3, 255]), BackdropSpec::POSTER);
        assert!(poster.backdrop.is_none(), "a poster-shaped picture is covered, never contained");
        let square = attach_backdrop(px(375, 375, |_, _| [1, 2, 3, 255]), BackdropSpec::POSTER);
        let copy = square.backdrop.as_deref().expect("a square gets a copy");
        assert_eq!((copy.w, copy.h), (16, 16));
        let any = attach_backdrop(px(250, 375, |_, _| [1, 2, 3, 255]), BackdropSpec { short_side: 16, skip_shape: None });
        assert!(any.backdrop.is_some());
    }

    /// A picture with a transparent ground (a PNG, as some channel art links serve it) blurs to
    /// its own ink: the RGB under the transparent texels is not the logo's colour and must not tint
    /// the copy, and a picture with nothing opaque still gets a (black) copy rather than none.
    #[test]
    fn a_transparent_ground_blurs_to_the_ink_not_the_rgb_under_it() {
        let logo = px(300, 300, |x, y| if (100..200).contains(&x) && (100..200).contains(&y) { [250, 200, 0, 255] } else { [0, 0, 0, 0] });
        let copy = backdrop_of(&logo, 16).expect("a logo gets a copy");
        assert!(copy.rgba.chunks(4).all(|p| p == [250, 200, 0, 255]), "every texel is the logo's yellow: {:?}", &copy.rgba[..8]);
        let ghost = backdrop_of(&px(300, 300, |_, _| [250, 200, 0, 0]), 16).expect("an empty picture still has a size");
        assert!(ghost.rgba.chunks(4).all(|p| p == [0, 0, 0, 255]), "no coverage anywhere: black, not the invisible RGB");
    }

    #[test]
    fn the_backdrop_is_the_blurred_mean_not_a_crop() {
        let flat = backdrop_of(&px(300, 300, |_, _| [200, 100, 50, 255]), 16).unwrap();
        assert!(flat.rgba.chunks(4).all(|p| p == [200, 100, 50, 255]), "a flat image blurs to itself, opaque");
        let split = backdrop_of(&px(320, 320, |x, _| if x < 160 { [255, 0, 0, 255] } else { [0, 0, 255, 255] }), 16).unwrap();
        let row = |x: usize| split.rgba[(8 * 16 + x) * 4..][..3].to_vec();
        assert_eq!(row(0)[0], 255, "far left stays red");
        assert_eq!(row(15)[2], 255, "far right stays blue");
        let (a, b) = (row(7), row(8));
        assert!(a[0] > 0 && a[2] > 0 && b[0] > 0 && b[2] > 0, "the edge is smeared across texels: {a:?} {b:?}");
    }
}
