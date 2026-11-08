//! One line per attempt, with tabs between the fields, never the text itself.
//!
//! Fields: time (UTC), outcome, path, who, fingerprint before, fingerprint after,
//! size in bytes. A dash stands for a field with no value. On the outcome
//! `changed`, the two fingerprints are the one expected and the one found.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

/// The current time in UTC, for example `2026-11-07 08:10:05Z`.
pub fn utc_now() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    utc_text(seconds)
}

/// Days to a civil date, after Howard Hinnant's `civil_from_days`.
fn utc_text(seconds: u64) -> String {
    let z = (seconds / 86_400) as i64 + 719_468;
    let era = z / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    let rest = seconds % 86_400;
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02}Z",
        rest / 3_600,
        rest % 3_600 / 60,
        rest % 60
    )
}

pub fn line(fields: &[&str]) -> String {
    fields
        .iter()
        .map(|f| if f.is_empty() { "-" } else { f })
        .collect::<Vec<_>>()
        .join("\t")
}

/// Makes a folder and its missing parents that only the owner can open. A folder
/// that is there already keeps its mode.
pub fn private_dir(dir: &Path) -> io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(dir)
}

/// Appends the line in one write. Makes the folder of the log if it is missing.
/// The log names the files and the agents, so only the owner can read it.
pub fn append(log: &Path, line: &str) -> io::Result<()> {
    if let Some(parent) = log.parent() {
        private_dir(parent)?;
    }
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(log)?;
    file.write_all(format!("{line}\n").as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utc_text_matches_known_times() {
        assert_eq!(utc_text(0), "1970-01-01 00:00:00Z");
        assert_eq!(utc_text(951_782_400), "2000-02-29 00:00:00Z");
    }

    #[cfg(unix)]
    #[test]
    fn a_new_log_and_its_folder_are_for_the_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let base = std::env::temp_dir().join(format!("orgthread-log-mode-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let log = base.join("state").join("writes.log");
        append(&log, "a").unwrap();
        append(&log, "b").unwrap();
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&log), 0o600);
        assert_eq!(mode(log.parent().unwrap()), 0o700);
        assert_eq!(mode(&base), 0o700);
        assert_eq!(fs::read_to_string(&log).unwrap(), "a\nb\n");
        fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn empty_fields_become_a_dash() {
        assert_eq!(line(&["a", "", "b"]), "a\t-\tb");
    }
}
