//! A stop request must not leave a lock behind.
//!
//! By default, Ctrl-C or a closing terminal ends the process on the spot, and
//! the lock stays until it turns stale. Once armed, such a signal only sets a
//! flag. The write then stops at its next safe point and releases the lock.
//! Nothing can catch a kill with signal 9: that lock still turns stale.

use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
use signal_hook::flag;
use std::io;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

pub fn arm() -> io::Result<Arc<AtomicBool>> {
    let stop = Arc::new(AtomicBool::new(false));
    for signal in [SIGINT, SIGTERM, SIGHUP] {
        flag::register(signal, Arc::clone(&stop))?;
    }
    Ok(stop)
}
