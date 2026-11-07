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

/// Appends the line in one write. Makes the folder of the log if it is missing.
pub fn append(log: &Path, line: &str) -> io::Result<()> {
    if let Some(parent) = log.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new().create(true).append(true).open(log)?;
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

    #[test]
    fn empty_fields_become_a_dash() {
        assert_eq!(line(&["a", "", "b"]), "a\t-\tb");
    }
}
