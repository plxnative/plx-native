//! The webOS port: the two tables that fill the platform, and the C entry that hands them over.
//!
//! `plx_os::tv::Services` is what the lower layers read, and `host::Platform` (the application's
//! `app::platform::Platform`) adds the shell and the frame probe the loop alone reads. Everything outside
//! this crate reaches the television through those; this is the only place that names `webos`,
//! `keymanager`, `system` and the native video sink together. The simulator and the macOS app are not on
//! these tables: they are their own platform (`plx_platform_desktop`), a different crate that never names
//! this one, so a desktop executable does not contain the television's code at all.
use plxnative_modules::host::{
    self, sdl_init, sdl_open, Exit, FrameProbe, GlProfile, InitHooks, InitSteps, MainThread, OpenAsk, Placement, Platform,
    SdlFlavor, Shell, Surface, WindowPlan,
};
use std::os::raw::{c_char, c_int, c_void};

#[cfg(all(not(test), feature = "play-feed"))]
const SINK: &dyn plx_os::tv::sink::VideoSink = &crate::webos::video_sink::StarfishSink;
#[cfg(any(test, not(feature = "play-feed")))]
const SINK: &dyn plx_os::tv::sink::VideoSink = &plx_os::tv::sink::NoSink;

// URI-only native builds retain the display vtable without the packet-feed vtable.
#[cfg(not(test))]
const DISPLAY: &dyn plx_os::tv::sink::DisplaySink = &crate::webos::video_sink::StarfishSink;
#[cfg(test)]
const DISPLAY: &dyn plx_os::tv::sink::DisplaySink = SINK;

/// What the lower layers read: the webOS identity, key manager, Home key and toast, the storage helper
/// and the native playback seam. The window, the bus pump and the frame probe are the loop's own, in [`PLATFORM`].
static SERVICES: plx_os::tv::Services = plx_os::tv::Services {
    identity: plx_os::tv::IdentityHooks {
        probe_device: crate::webos::probe,
        start_capability_probe: crate::webos::caps::start_probe,
        repair_native_video: Some(crate::webos::jail_repair::execute),
        device_files: crate::webos::DEVICE_FILES,
        system_locale: crate::webos::system_locale,
        sound_output: crate::webos::sound_output,
    },
    secure: plx_os::tv::SecureHooks {
        seal: crate::keymanager::seal,
        open: crate::keymanager::open,
        remove: crate::keymanager::remove,
        mode: plx_os::tv::secure::StoreMode::Durable,
    },
    system: plx_os::tv::SystemHooks {
        go_home: crate::webos::go_home,
        poll_home: crate::webos::poll_home,
        deliver_toast: crate::webos::toast::deliver,
    },
    storage: plx_os::tv::StorageHooks { authority: plx_os::tv::authority::Authority::Helper },
    layout: plx_os::tv::Layout::DERIVED,
    legacy_webos_media: plx_os::tv::LegacyWebosMedia {
        sink: SINK,
        display_sink: DISPLAY,
        uri_media: crate::webos::URI_MEDIA,
    },
};

/// What the webOS platform states about its SDL. The television links LG's fork of SDL2, so its events
/// are laid out in the [`SdlFlavor::Lg`] layout; its driver is a real GLES2 one, so it asks for
/// [`GlProfile::Es2`]; and it runs LG's two boot hooks around `SDL_Init` (the cursor hint before, which SDL reads
/// in `SDL_Init`, and the relaunch tap after, once LG's SDL has filled the slot it wraps) and nothing behind the
/// GL context.
const SDL: InitHooks = InitHooks {
    steps: InitSteps {
        before: crate::system::webos3_cursor_sleep_hint,
        after: crate::system::tap_relaunch_payloads,
        after_context: InitSteps::NONE.after_context,
    },
    flavor: SdlFlavor::Lg,
    gl: GlProfile::Es2,
};

/// The webOS window ask: the process-shaped title every log, `pidof` recipe and skill already uses, and a
/// panel at the canvas size unless a webOS 1-3 owner picked a smaller Interface resolution (or the debug
/// drawable trigger asked for one), retried once at the canvas if the set refuses it.
fn window(stored: Option<(c_int, c_int)>) -> WindowPlan {
    let size = requested_drawable(stored).unwrap_or(CANVAS);
    WindowPlan { title: c"plxnative", placement: Placement::Panel { size, fallback: window_fallback(size) } }
}

/// The canvas the interface is authored at, which is the window a webOS 4+ set always gets.
const CANVAS: (c_int, c_int) = (plx_base::surface::LOGICAL_W as c_int, plx_base::surface::LOGICAL_H as c_int);

/// The window to ask for when it is not the 1920x1080 canvas, `None` for the canvas (1:1): whatever
/// `/tmp/plxnative-drawable=<W>x<H>` says when that trigger is armed (debug builds only; it
/// outranks the setting and is bounded to 640x360..=1920x1080 so a typo cannot ask for nonsense),
/// else, on webOS 1-3 only, the install-wide Settings > System > Interface resolution choice
/// (`stored`, the size the stored setting asks for, `None` for Automatic). **Automatic is the canvas on
/// every set**, and webOS 4+ is always the canvas; a smaller window is only ever a webOS 1-3 owner's
/// pick (or the debug trigger).
///
/// What was measured, with the heartbeat framedrop A/B (not `profile-tv` counters, so no cause is
/// claimed): on a 2016 55UH650V (webOS 3.4.3, Mali-T760) at 1280x720, average 46.9 -> 54.5 fps and
/// frames over 33 ms from 7.1% to 0.5%; on a 2017 OLED C7V (webOS 3.9.3, Mali-T820) frames over
/// 16.7 ms from 36% at 1080p to 2% at 1280x720 and 16% at 1600x900. The same owner found 720p text
/// "disturbingly soft" on the C7V, which is why this is opt-in rather than a webOS-release rule.
/// `surface::probe` reads back what webOS granted and `scale`/`viewport` fit the canvas into it.
fn requested_drawable(stored: Option<(c_int, c_int)>) -> Option<(c_int, c_int)> {
    if let Some(v) = plx_base::devtrig::read("drawable") {
        let parsed = v.trim().split_once('x').and_then(|(w, h)| {
            Some((w.trim().parse::<c_int>().ok()?, h.trim().parse::<c_int>().ok()?))
        });
        let ok = parsed.filter(|&(w, h)| (640..=CANVAS.0).contains(&w) && (360..=CANVAS.1).contains(&h));
        plx_base::eventlog::log(&format!("surface: drawable trigger {:?} -> {:?}", v.trim(), ok));
        if ok.is_some() {
            return ok;
        }
    }
    let major = plx_os::tv::device::info().major;
    let size = drawable_for_setting(major, stored);
    plx_base::eventlog::log(&format!("surface: webOS major {major}, Interface resolution {stored:?} -> {size:?}"));
    size
}

/// The window an Interface resolution choice asks for on firmware `major`: `stored` where the setting is
/// offered (webOS 1-3), and `None` on every set it is not offered on (webOS 4+ or unknown,
/// [`plx_os::tv::device::offers_ui_resolution`]) whatever is stored, so a record carried over from another
/// set cannot shrink the interface where the owner has no way to change it back.
fn drawable_for_setting(major: u32, stored: Option<(c_int, c_int)>) -> Option<(c_int, c_int)> {
    if plx_os::tv::device::offers_ui_resolution(major) { stored } else { None }
}

/// Where to retry when the set refuses a window of size `asked`: the native canvas, unless that is
/// what was asked, which has nowhere left to fall back to.
fn window_fallback(asked: (c_int, c_int)) -> Option<(c_int, c_int)> {
    (asked != CANVAS).then_some(CANVAS)
}

/// The webOS shell: the SDL window's Wayland surface, the opaque-region hint, the LS2 bus pump and the
/// Magic Remote's pointer. The bodies stay in `crate::system` (and `webos::bind_window`); this is the
/// table over them. SDL itself, and the window it opens, are the shell crate's (`plx_shell_sdl`), which
/// this platform brings up with LG's two boot hooks around `SDL_Init`.
struct WebosShell;

impl Shell for WebosShell {
    fn sdl(&self) -> InitHooks {
        SDL
    }
    fn window(&self, stored: Option<(c_int, c_int)>) -> WindowPlan {
        window(stored)
    }
    fn init(&self, mt: &MainThread) -> Result<(), Exit> {
        // SAFETY: both hooks are LG's SDL boot steps (`crate::system`), run on the SDL main thread `mt`
        // proves, at the two points `plx_shell_sdl::init::init` calls them: the cursor hint before
        // `SDL_Init`, which reads it, and the relaunch tap after, once LG's SDL has filled its slot.
        unsafe { sdl_init(mt, &SDL) }
    }
    fn open(&self, mt: &MainThread, ask: &OpenAsk) -> Result<Box<dyn Surface>, Exit> {
        sdl_open(mt, &SDL, ask)
    }
    fn bind_window(&self, win: *mut c_void) {
        crate::webos::bind_window(win);
    }
    fn grab_surface(&self, win: *mut c_void) {
        crate::system::sys_grab_wayland(win);
    }
    fn release_surface(&self) -> bool {
        crate::system::sys_release_wayland()
    }
    fn arm_opaque_region(&self) {
        crate::system::opaque_region_init();
    }
    fn opaque_route(&self, player: bool) {
        crate::system::opaque_route(player);
    }
    fn clear_opaque_region(&self) {
        crate::system::clear_opaque_region();
    }
    fn pump_bus(&self) {
        crate::system::ls2_pump();
    }
    fn hide_pointer(&self) {
        crate::system::hide_cursor();
    }
    fn show_pointer_after_motion(&self) {
        crate::system::show_cursor_after_motion();
    }
    fn hide_is_cursor_swap(&self) -> bool {
        crate::system::cursor_is_transparent()
    }
}

/// The webOS frame probe, over `crate::system`'s compositor-callback clock.
struct WebosProbe;

impl FrameProbe for WebosProbe {
    fn request(&self) {
        crate::system::frame_probe_request();
    }
    fn prepare_begin(&self) {
        crate::system::frame_probe_prepare_begin();
    }
    fn prepare_end(&self) {
        crate::system::frame_probe_prepare_end();
    }
    fn waiting(&self) {
        crate::system::frame_probe_waiting();
    }
    fn acquired(&self) {
        crate::system::frame_probe_acquired();
    }
    fn fields(&self) -> String {
        crate::system::frame_probe_fields()
    }
}

/// The webOS platform: what `plex_main` hands `host::run`, which installs its services for the lower
/// layers and keeps it for the loop. Its media boot is the television's: the plane owner's boot, and the
/// Feed plus the product URI driver. The bodies are the application's `adapters::backends`, handed over as
/// the ready value `host::WEBOS_MEDIA`, until the webOS platform owns that file (W11a): the dev surface and
/// the tests name the URI product there, and nothing outside this crate may name `port`.
static PLATFORM: Platform = Platform::new(
    &SERVICES,
    &WebosShell,
    &WebosProbe,
    host::WEBOS_MEDIA,
);

/// The C shim's entry (`src/main.c`). The `-> c_int` is only the C signature: `plex_main` never
/// returns, so neither does this.
#[no_mangle]
pub extern "C" fn plex_run(pms_host: *const c_char, pms_port: c_int) -> c_int {
    plex_main(pms_host, pms_port)
}

/// The process entry: hand the platform to the application (`host::run`), then end the process with the
/// status it returned. The renderer's clear hook (`gfx::install_clear_probe`) takes plain functions, so
/// it is installed here, with the webOS frame probe's own, on the thread the app boots and draws on.
///
/// The executable owns the process end: `exit_now` (`_exit`, never libc's `exit`, #1115) right after
/// the app returns, in the order the app always ended in. A process that hosts the app never calls it.
///
/// It is typed `-> !` rather than left to diverge through its last expression. `src/main.c` does
/// `return plex_run(..)`, so an entry that returned would fall into `__libc_start_main`, which calls
/// libc `exit` directly (the interposer in `src/exit.c` cannot catch that), and `OPENSSL_cleanup`
/// would then free libcrypto's locks under the live curl workers (#1115). The `const` below is the
/// compile-time proof that replaced the ones `app::boot`'s `seam_order_tests` held for
/// `run_application` and `run_and_shutdown` when those started to return an `Exit`: a later edit that
/// lets this return does not compile.
///
/// The first line installs the storage helper's wake-up (`install_activator`) under a Linux gate.
/// That is the storage client's transport truth (it is a Unix socket with peer
/// credentials, see `plx_os::storage::client`), not a statement that this is a webOS build; the helper's
/// own wire type keeps the wake-up out of `tv::StorageHooks`.
fn plex_main(pms_host: *const c_char, pms_port: c_int) -> ! {
    #[cfg(target_os = "linux")]
    let _ = plx_os::storage::client::install_activator(crate::webos::activate_storage_helper);
    plx_gfx::gfx::install_clear_probe(crate::system::frame_probe_waiting, crate::system::frame_probe_acquired);
    // SAFETY: `src/main.c` hands `PMS_HOST`, a NUL-terminated string literal, so the pointer is never NULL.
    let host = unsafe { std::ffi::CStr::from_ptr(pms_host) };
    let port = u16::try_from(pms_port).expect("PMS_PORT is a TCP port");
    plx_base::process::exit_now(host::run(&PLATFORM, host, port).code)
}

const _: fn(*const c_char, c_int) -> ! = plex_main;

/// `src/main.c` hands over SAM's launch payload (`argv[1]`) before `plex_run`. Kept raw in
/// `tv::launch`; `app::deeplink` reads it once the app has a session to act in.
///
/// # Safety
/// `json` is NULL or a NUL-terminated string that outlives the call.
#[no_mangle]
pub unsafe extern "C" fn plx_launch_args(json: *const c_char) {
    if json.is_null() {
        return;
    }
    let raw = unsafe { std::ffi::CStr::from_ptr(json) }.to_string_lossy();
    plx_os::tv::launch::offer(&raw);
}

#[cfg(test)]
mod tests {
    use super::{drawable_for_setting, window_fallback, SDL, SERVICES};
    use plx_os::tv::authority::Authority;
    use plxnative_modules::host::{GlProfile, SdlFlavor};

    /// The webOS platform states LG's SDL layout and an ES 2.0 context, and no step behind the context:
    /// the television's driver has nothing to bind there.
    #[test]
    fn the_webos_platform_states_the_lg_layout_and_an_es2_context() {
        assert_eq!(SDL.flavor, SdlFlavor::Lg);
        assert_eq!(SDL.gl, GlProfile::Es2);
    }

    /// Automatic is the native canvas on every set and asks for no smaller window. The other
    /// options ask for exactly their size.
    #[test]
    fn the_setting_maps_to_the_requested_window() {
        for major in [1, 2, 3] {
            assert_eq!(drawable_for_setting(major, None), None);
            assert_eq!(drawable_for_setting(major, Some((1600, 900))), Some((1600, 900)));
            assert_eq!(drawable_for_setting(major, Some((960, 540))), Some((960, 540)));
        }
        assert_eq!(drawable_for_setting(3, Some((1280, 720))), Some((1280, 720)));
        // webOS 4+, or a set that never said, keeps the canvas whatever a record carried over holds
        for major in [0, 4, 5, 6, 22] {
            for stored in [None, Some((1600, 900)), Some((1280, 720)), Some((960, 540))] {
                assert_eq!(drawable_for_setting(major, stored), None, "major {major} {stored:?}");
            }
        }
    }

    /// A persisted size the set will not give must never brick launch: a refused smaller window is
    /// retried ONCE at the native canvas, and a refused native window has nowhere left to fall.
    #[test]
    fn a_refused_smaller_window_retries_once_at_the_native_canvas() {
        assert_eq!(window_fallback((1600, 900)), Some(super::CANVAS));
        assert_eq!(window_fallback((960, 540)), Some(super::CANVAS));
        assert_eq!(window_fallback(super::CANVAS), None, "the native canvas has no fallback");
    }

    /// The webOS table states the storage helper as the canonical home of the session, the install's
    /// preferences and the consent record: on a television that is DB8, and a table that stated
    /// `Files` would say the app's own files are.
    #[test]
    fn the_webos_table_states_the_storage_helper_as_the_authority() {
        assert_eq!(SERVICES.storage.authority, Authority::Helper);
        assert_eq!(SERVICES.secure.mode, plx_os::tv::secure::StoreMode::Durable);
    }

    /// **The two LG hooks are the webOS platform's `SDL`, handed in the order SDL needs.** The shell crate
    /// pins the order inside `plx_shell_sdl::init::init` (`before`, then `SDL_Init`, then `after`) and
    /// cannot name the hooks; this pins that the webOS platform hands exactly them, the cursor hint as the
    /// step BEFORE (SDL reads it in `SDL_Init`, so a hint set after does nothing) and the relaunch tap as
    /// the step AFTER (LG's SDL fills the slot it wraps inside `SDL_Init`), once each, and that nothing
    /// else in this file names them. Reads the source, like the tests below, because no host test can
    /// run LG's SDL.
    #[test]
    fn the_webos_shell_binds_the_cursor_hint_before_and_the_relaunch_tap_after_sdl_init() {
        // The needles are split with `concat!` so this test's own text, which `include_str!` reads too,
        // can never be what they match.
        let source = include_str!("port.rs");
        let init = source
            .split_once(concat!("const ", "SDL: InitHooks = "))
            .expect("the webOS SDL value")
            .1
            .split_once("\n};\n")
            .expect("the end of the value")
            .0;
        let before = concat!("before: crate::system::", "webos3_cursor_sleep_hint,");
        let after = concat!("after: crate::system::", "tap_relaunch_payloads,");
        assert_eq!(init.matches(before).count(), 1, "the cursor hint is the platform's `before`");
        assert_eq!(init.matches(after).count(), 1, "the relaunch tap is the platform's `after`");
        assert!(init.find(before) < init.find(after), "`before` is stated first, as it runs first");
        for hook in [
            concat!("crate::system::", "webos3_cursor_sleep_hint"),
            concat!("crate::system::", "tap_relaunch_payloads"),
        ] {
            assert_eq!(source.matches(hook).count(), 1, "{hook} is named by the platform's `SDL` and nowhere else");
        }
    }
}
