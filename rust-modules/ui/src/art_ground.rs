//! **The art ground: a programme's picture, full screen, standing in for video that has not arrived.**
//!
//! The Live TV player draws it over the video plane's hole from a zap's first press until the new
//! stream's first frame (`docs/hud-plugins.md`, "The live banner"; handoff SPEC-4): the target airing's
//! art, scaled [`ZOOM`], at [`BRIGHTNESS`], softened by the shared glass blur, never black. A channel with
//! no art, or whose art has not decoded yet, stands on the app's own ground ([`theme::SURFACE_APP`]).
//!
//! **The blur is the shared material's** (`widgets::Glass`), asked for once over the whole screen after
//! the sharp art is down, so the frame's blur source is the art itself. Where the frame cannot offer it
//! (`Glass::backdrop` answers `false`: no backdrop plan for the frame, or a driver that latched the blur
//! off) the sharp art stays: the fallback is a picture, never a hole. The blur's strength is the
//! material's, not a per-call radius; the handoff's 28 px is the design intent, and whether the material
//! reads as that on the television is a measurement the PR that landed this leaves open.
use crate::{theme, Crop, Painter, Rect};
use crate::widgets::{resolve_tex_wh_on, Glass};

/// The handoff's scale of the art over the screen (hides the blur's soft edge).
pub const ZOOM: f32 = 1.08;
/// The handoff's brightness of the art, as a tint on its colour.
pub const BRIGHTNESS: f32 = 0.85;

/// The texture window of a `tw` x `th` picture that covers the screen, zoomed [`ZOOM`] about its centre.
pub fn zoomed_uv(tw: f32, th: f32) -> [f32; 4] {
    let [u, v, w, h] = Rect::FULL.cover_uv(tw, th, Crop::Centre);
    let (zw, zh) = (w / ZOOM, h / ZOOM);
    [u + (w - zw) * 0.5, v + (h - zh) * 0.5, zw, zh]
}

/// Draw the art ground at `alpha` (1 covers, 0 shows the video): the art of `key` on server `srv`, else
/// the app's ground. Returns whether the picture was the art (not the stand-in), for a caller's test.
pub fn draw(p: Painter, srv: u16, key: &str, alpha: f32) -> bool {
    if alpha <= 0.0 {
        return false;
    }
    let p = p.alpha(alpha);
    let (tex, tw, th) = if key.is_empty() { (0, 0.0, 0.0) } else { resolve_tex_wh_on(srv, key, 480, 270, 0) };
    if tex == 0 {
        p.rect(Rect::FULL, 0.0, theme::SURFACE_APP, theme::SURFACE_APP, 0.0);
        return false;
    }
    let tint = [BRIGHTNESS, BRIGHTNESS, BRIGHTNESS, 1.0];
    p.tex_uv(tex, zoomed_uv(tw, th), Rect::FULL, 0.0, tint);
    // Soften what was just drawn. `false` is the sharp fallback, not an error.
    let _ = Glass::DYNAMIC_BACKDROP.backdrop(
        p, Rect::FULL, 0.0, 0.0, [1.0; 4], plx_gfx::gfx::GlassRim::Bevelled, plx_gfx::gfx::GlassFace::NONE, theme::PANEL_MATERIAL);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The window is the cover window shrunk by the zoom about its centre, so a 16:9 picture on the 16:9
    /// screen shows its middle 1/1.08 and nothing else.
    #[test]
    fn the_window_is_the_cover_window_zoomed_about_its_centre() {
        let uv = zoomed_uv(480.0, 270.0);
        let want = 1.0 / ZOOM;
        assert!((uv[2] - want).abs() < 1e-5 && (uv[3] - want).abs() < 1e-5, "{uv:?}");
        assert!((uv[0] - (1.0 - want) * 0.5).abs() < 1e-5 && (uv[1] - (1.0 - want) * 0.5).abs() < 1e-5, "{uv:?}");
    }
}
