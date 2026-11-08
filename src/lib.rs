//! orgthread: one command that writes a plain text file the same way every time.

/// The version of this build and the commit it came from, with `+` after the
/// commit if the code had changes not yet committed.
pub const VERSION: &str = concat!(
    env!("CARGO_PKG_VERSION"),
    " (",
    env!("ORGTHREAD_COMMIT"),
    ")"
);

pub mod dropbox;
pub mod fingerprint;
pub mod list;
pub mod log;
pub mod login;
pub mod patch;
pub mod reads;
pub mod screen;
pub mod serve;
pub mod stop;
pub mod storage;
pub mod versions;
pub mod watch;
pub mod write;
