// Live lifecycle regressions use the existing loopback PMS rig and real app event/stop path.
fn live_body(request: &str) -> String {
    if request.contains("/tune?") {
        return r#"{"MediaContainer":{"key":"/livetv/sessions/12345678","ratingKey":"live-airing"}}"#.into();
    }
    if request.contains("/decision?") {
        return r#"{"MediaContainer":{"Metadata":[{"Media":[{"videoCodec":"h264","audioCodec":"aac","width":1280,"height":720,"audioChannels":2,"Part":[{"key":"/livetv/sessions/12345678/stream/index.m3u8"}]}]}]}}"#.into();
    }
    r#"{"MediaContainer":{}}"#.into()
}

fn tuned_live(rig: &mut Rig) -> plx_machine::machine::EntryId {
    let origin = rig.app.pages.nav.top_page().unwrap().id;
    assert!(plx_media::route::request_live(&mut rig.app.player.session,
        rig.app.runtime.bridge(&mut rig.app.bridge).metadata_mut(),
        plx_os::devcaps::caps(), rig.sid, "7", "C101", "Family Channel"));
    rig.entered.recv_timeout(Duration::from_secs(3)).unwrap();
    rig.accept_start();
    frame(&mut rig.app, 16);
    rig.release.send(()).unwrap();
    poll_until("live fixture plan did not land", || {
        plx_media::route::pump_play(&mut rig.app.player.session,
            rig.app.runtime.bridge(&mut rig.app.bridge).metadata_mut()).is_some()
    });
    assert!(plx_media::route::is_live(&rig.app.player.session));
    assert!(plx_media::route::has_url(&rig.app.player.session));
    assert_eq!(super::super::bridge::player(&rig.app.pages).unwrap().origin.unwrap().entry, origin);
    origin
}

fn live_stop_count(rig: &Rig) -> usize {
    rig.requests.lock().unwrap().iter()
        .filter(|request| request.contains("/:/timeline?") && request.contains("state=stopped")).count()
}

#[test]
fn background_while_live_stops_the_tuner_and_returns_to_the_original_entry() {
    let _serial = plx_base::testlock::serial();
    let _session = plx_plex::plex::session::TempSession::new("live-background");
    let Some(mut rig) = Rig::serving_with(live_body) else {
        eprintln!("SKIPPED background_while_live_stops_the_tuner_and_returns_to_the_original_entry: fixture worker refused");
        return;
    };
    let origin = tuned_live(&mut rig);
    let mut fr = Frame::begin(&rig.app.player.session, rig.app.runtime.metadata_view());
    event(&mut rig.app, &mut fr, 0x103);
    event(&mut rig.app, &mut fr, 0x104);
    assert!(!plx_media::route::is_live(&rig.app.player.session));
    assert!(!rig.app.player.lifecycle.awaiting_load());
    assert_eq!(rig.app.player.lifecycle.state(), ForegroundState::Idle);
    plx_media::route::drain_scrobble();
    assert_eq!(live_stop_count(&rig), 1, "both background edges release one tuner exactly once");
    event(&mut rig.app, &mut fr, 0x105);
    event(&mut rig.app, &mut fr, 0x106);
    let mut now = 32;
    settle(&mut rig.app, &mut now);
    assert_eq!(rig.app.pages.nav.top_page().unwrap().id, origin);
    assert!(super::super::bridge::player(&rig.app.pages).is_none());
    assert!(!rig.app.adapters.player.is_live());
}

#[test]
fn background_after_leaving_live_keeps_the_page_the_viewer_chose() {
    let _serial = plx_base::testlock::serial();
    let _session = plx_plex::plex::session::TempSession::new("live-left-background");
    let Some(mut rig) = Rig::serving_with(live_body) else {
        eprintln!("SKIPPED background_after_leaving_live_keeps_the_page_the_viewer_chose: fixture worker refused");
        return;
    };
    tuned_live(&mut rig);
    player_requests(&mut rig.app.player, &mut rig.app.adapters.player, &mut rig.app.repause_at,
        vec![plx_screens::registry::PlayerReq::Exit], 32, &mut rig.app.refresh_hubs,
        &mut rig.app.pages, &mut rig.app.runtime.bridge(&mut rig.app.bridge));
    let mut now = 32;
    settle(&mut rig.app, &mut now);
    assert!(!plx_media::route::is_live(&rig.app.player.session));
    super::super::bridge::show_page(&mut rig.app.pages, AppArg::Library);
    settle(&mut rig.app, &mut now);
    let chosen = rig.app.pages.nav.top_page().unwrap().id;
    assert!(rig.app.route() == AppArg::Library);
    let mut fr = Frame::begin(&rig.app.player.session, rig.app.runtime.metadata_view());
    for edge in [0x103, 0x104, 0x105, 0x106] { event(&mut rig.app, &mut fr, edge); }
    settle(&mut rig.app, &mut now);
    assert_eq!(rig.app.pages.nav.top_page().unwrap().id, chosen, "a retired live request navigated away on background");
    assert!(rig.app.route() == AppArg::Library);
    plx_media::route::drain_scrobble();
    assert_eq!(live_stop_count(&rig), 1);
}

/// A rig channel on the rig's server, in the Live TV store's lineup (the tune below names it). The
/// store's real pump replaces a seeded lineup with what its sources hold, so a case that reads the
/// Recent row seeds the lineup again after the frames it ran.
fn lineup_the_rig_channel(rig: &mut Rig) {
    let channel = plx_data::livetv::Channel { sid: rig.sid, dvr: "7".into(), channel: "C101".into(),
        vcn: "101".into(), name: "Family Channel".into(), thumb: String::new(), now: String::new(),
        now_thumb: String::new(), now_summary: String::new(), begins_at: 0, ends_at: 0 };
    rig.app.runtime.bridge(&mut rig.app.bridge).stores.livetv.install_for_test(vec![channel]);
}

fn recent_names(rig: &mut Rig) -> Vec<String> {
    rig.app.runtime.bridge(&mut rig.app.bridge).livetv_view().recent.iter().map(|c| c.name.clone()).collect()
}

/// Ask for the rig channel's tune and run the loop's own frames (`playback_tick`, then `update`, whose
/// `pump_play` arm lands the plan) until the resolve has landed, whatever it landed as.
fn tune_through_the_loop(rig: &mut Rig) {
    assert!(plx_media::route::request_live(&mut rig.app.player.session,
        rig.app.runtime.bridge(&mut rig.app.bridge).metadata_mut(),
        plx_os::devcaps::caps(), rig.sid, "7", "C101", "Family Channel"));
    rig.entered.recv_timeout(Duration::from_secs(3)).unwrap();
    rig.accept_start();
    frame(&mut rig.app, 16);
    rig.release.send(()).unwrap();
    poll_until("the live resolve did not land through the loop", || {
        let mut fr = Frame::begin(&rig.app.player.session, rig.app.runtime.metadata_view());
        fr.now = 32;
        unsafe {
            playback_tick(&mut rig.app, &mut fr);
            update(&mut rig.app, &mut fr);
        }
        !plx_media::route::play_pending()
    });
}

/// **The loop's landing notes an admitted tune.** The plan lands through `update`'s own `pump_play`
/// arm (nothing here calls the note), and the channel joins Recent.
#[test]
fn a_live_plan_landing_through_the_loop_notes_the_channel_in_recent() {
    let _serial = plx_base::testlock::serial();
    let _session = plx_plex::plex::session::TempSession::new("live-landing-recent");
    let Some(mut rig) = Rig::serving_with(live_body) else {
        eprintln!("SKIPPED a_live_plan_landing_through_the_loop_notes_the_channel_in_recent: fixture worker refused");
        return;
    };
    lineup_the_rig_channel(&mut rig);
    assert!(recent_names(&mut rig).is_empty(), "nothing tuned yet");
    tune_through_the_loop(&mut rig);
    assert!(plx_media::route::live_tuned(&rig.app.player.session).is_some(), "the server admitted the tune");
    lineup_the_rig_channel(&mut rig);
    assert_eq!(recent_names(&mut rig), ["Family Channel"]);
}

/// **A tune the server did not admit joins nothing.** The same landing, with every tuner busy (the
/// server answers the tune 503): the refusing plan installs (its verdict is on the session) and
/// `pump_play` hands the loop no start for it, so the loop's arm never reaches the note.
#[test]
fn a_live_plan_landing_without_an_admitted_tune_notes_nothing() {
    let _serial = plx_base::testlock::serial();
    let _session = plx_plex::plex::session::TempSession::new("live-landing-refused");
    let Some(mut rig) = Rig::serving_with(|request: &str| if request.contains("/tune?") {
        "!503".into() } else { live_body(request) }) else {
        eprintln!("SKIPPED a_live_plan_landing_without_an_admitted_tune_notes_nothing: fixture worker refused");
        return;
    };
    lineup_the_rig_channel(&mut rig);
    tune_through_the_loop(&mut rig);
    assert_eq!(plx_media::route::play_verdict(&rig.app.player.session).as_deref(),
        Some(plx_os::i18n::msg::widgets_verdict_tuners_busy()), "the refusing plan landed through the loop");
    assert!(plx_media::route::live_tuned(&rig.app.player.session).is_none(), "no live session: the tune was not admitted");
    assert!(plx_media::route::live_request(&rig.app.player.session).is_some(), "the channel was asked for");
    lineup_the_rig_channel(&mut rig);
    assert!(recent_names(&mut rig).is_empty());
}
