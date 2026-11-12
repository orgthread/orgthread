//! orgthread: one command that writes a plain text file the same way every time.

/// The version of this build and the commit it came from, with `+` after the
/// commit if the code had changes not yet committed.
pub const VERSION: &str = concat!(
    env!("CARGO_PKG_VERSION"),
    " (",
    env!("ORGTHREAD_COMMIT"),
    ")"
);

pub mod config;
pub mod copy;
pub mod delete;
pub mod dropbox;
pub mod envfile;
pub mod fingerprint;
pub mod history;
pub mod list;
pub mod log;
pub mod login;
pub mod mv;
pub mod patch;
pub mod reads;
pub mod revert;
pub mod screen;
pub mod search;
pub mod serve;
pub mod settings;
pub mod stop;
pub mod storage;
pub mod transfer;
pub mod versions;
pub mod watch;
pub mod write;
