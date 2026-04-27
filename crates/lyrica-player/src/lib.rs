#[cfg(target_os = "linux")]
pub mod mpris;

#[cfg(target_os = "linux")]
pub use mpris::MprisPlayer as DefaultPlayer;

#[cfg(target_os = "macos")]
pub mod macos;

#[cfg(target_os = "macos")]
pub use macos::MacOsPlayer as DefaultPlayer;
