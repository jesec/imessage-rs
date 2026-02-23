/// Private API TCP service: communication layer between the imessage-rs server
/// and the DYLD_INSERT_LIBRARIES-injected helper dylib running inside Messages.app.
///
/// Wire protocol: newline-delimited JSON over TCP on localhost.
/// Port: 45670 + (uid - 501), clamped to [45670, 65535].
pub mod actions;
pub mod events;
pub mod injection;
pub mod service;
pub mod transaction;

/// The compiled helper dylib bytes, embedded at build time.
/// Written to disk at runtime for DYLD_INSERT_LIBRARIES injection.
#[cfg(target_os = "macos")]
pub static HELPER_DYLIB: &[u8] = include_bytes!(env!("HELPER_DYLIB_PATH"));

#[cfg(not(target_os = "macos"))]
pub static HELPER_DYLIB: &[u8] = &[];
