//! The Tizen URI playback driver and its composition (W10 part B), compiled only with this crate's `uri` feature.
//!
//! - [`port`]: [`port::HostPlayer`], what the driver may ask of the host's media player and read back, and
//!   [`port::HostedPlayer`], its implementation over the hosted contract (`include/plx_host.h`, PLAYER);
//! - [`driver`]: [`driver::TizenUriDriver`], the Sans-IO state machine behind the contract's `UriDriver`;
//! - [`qualifier`]: [`qualifier::TizenQualifier`], what the platform certifies for the planner;
//! - [`compose`]: [`compose::MEDIA`], the `MediaBoot` that installs the driver and returns the resolver.
//!
//! The host side (the C# video thread over `libcapi-media-player.so.0`) is `tizen/host/PlxHost/PlxPlayer.cs`.
pub(crate) mod compose;
pub mod driver;
pub mod port;
pub mod qualifier;

pub(crate) use compose::MEDIA;

#[cfg(test)]
mod tests;
