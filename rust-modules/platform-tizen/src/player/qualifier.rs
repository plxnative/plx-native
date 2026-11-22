//! What the Tizen platform certifies for the planner: [`TizenQualifier`], the production twin of the test-only
//! `SelfCertified` of the route's W6c composition tests.
//!
//! The driver is the platform's only path and says what it plays, so it certifies itself: no viewer opt-in is part
//! of the answer (`ResolvePort::qualify` hands the same evidence to everyone, [`super::compose`] builds it from
//! the driver's own capabilities and display). Three rules, each from the contract's own words:
//!
//! - **An original is certified only where Standard would direct-play it** (`OriginalRequest::standard`): the plan's
//!   own verdicts, never re-derived here. That is what keeps DTS and TrueHD out (the planner's audio table has no
//!   row for them on this platform) and every container Standard hands to the server to remux. The codec table
//!   behind those verdicts is `Caps::assumed()` today, not a Tizen measurement (`docs/tizen.md`).
//! - **The native player plays the source's first audio stream.** The host ABI has no track verb, so a play that
//!   selects any other audio stream is declined ([`UriDecline::NotCertified`]) instead of started on the wrong track.
//! - **A converted PMS output is never certified** ([`PmsVerdict::Unknown`]): certifying a remux or encode needs
//!   evidence the player lacks (HLS, a source time mapping), so a source Standard would not direct-play is the
//!   planner's typed `NoQualifiedBackend` refusal with the PMS decline named, not a conversion.
use plx_media::route::uri_preflight::{KnownSource, OriginalQualifier, OriginalRequest, PmsRequest, PmsVerdict, UriDecline,
    Verdict, SOURCE_VERDICT_TTL};
use plx_base::task::OffFrame;

pub struct TizenQualifier;

impl OriginalQualifier for TizenQualifier {
    fn qualify(&self, _: &OffFrame, request: OriginalRequest) -> Verdict {
        if let Some(gap) = request.standard.first_gap() {
            return Verdict::Unknown(gap);
        }
        // `audio_ordinal` is the selected stream's position among the Part's audio streams; the driver settles the
        // request only for position 0 (`player::driver::CAPS`).
        if request.selection.audio_sid > 0 && request.audio_ordinal != Some(0) {
            return Verdict::Unknown(UriDecline::NotCertified);
        }
        let url = request.source_url.clone();
        Verdict::Known(KnownSource::url_carrier(&request, url, SOURCE_VERDICT_TTL))
    }

    fn qualify_pms(&self, _: &OffFrame, _: PmsRequest) -> PmsVerdict {
        PmsVerdict::Unknown
    }
}
