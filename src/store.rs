// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! The history file: `~/.local/share/leyden/history.tsv`.
//!
//! One sample per line, tab separated — `unix_secs`, percent, watts (`-` when
//! the gauge gave nothing), the status key, and an optional marker. Plain text
//! because the record is a handful of scalars: JSON would mean a serde
//! dependency for no gain, and the file stays greppable and diffable.
//!
//! The marker column was added after the format was already on disk. Lines
//! written before it simply lack a fifth field, and `parse` reads what it needs
//! and ignores the rest — so old files load unchanged.
//!
//! Appending is the normal path; the file is compacted on load, which is the
//! only time anything old is dropped. Unparseable lines are skipped rather than
//! treated as errors, so a future format change degrades instead of exploding.
//!
//! Lives above `battery/` because it needs `glib` for the XDG data dir, and
//! `battery/` imports nothing from gtk.

use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use relm4::gtk::glib;

use crate::battery::history::{Follows, Sample};
use crate::battery::types::Status;

/// Samples older than this are dropped when the file is read, and the file is
/// rewritten without them.
pub const MAX_AGE_SECS: f64 = 24.0 * 60.0 * 60.0;

/// A day at the recording cadence is roughly 100 KB. Past this the file is
/// carrying more than a day, so it is compacted on the spot rather than waiting
/// for the next load — a session left running for a week never reloads at all.
const MAX_BYTES: u64 = 256 * 1024;

/// Where everything this app persists lives.
pub fn data_dir() -> PathBuf {
    glib::user_data_dir().join("leyden")
}

fn path() -> PathBuf {
    data_dir().join("history.tsv")
}

/// Every sample from the last `MAX_AGE_SECS`, oldest first. A missing or
/// unreadable file is not an error — it is a first run.
pub fn load() -> Vec<Sample> {
    let (samples, total) = within_horizon();
    if samples.len() < total
        && let Err(error) = rewrite(&samples)
    {
        tracing::warn!("could not compact the history file: {error}");
    }
    samples
}

/// Every sample still inside the horizon, plus how many parsed lines the file
/// held — the difference is what compaction would remove.
fn within_horizon() -> (Vec<Sample>, usize) {
    let Ok(file) = File::open(path()) else {
        return (Vec::new(), 0);
    };
    let cutoff = SystemTime::now().checked_sub(Duration::from_secs_f64(MAX_AGE_SECS));
    let mut total = 0usize;
    let mut samples: Vec<Sample> = BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .filter_map(|line| {
            let sample = parse(&line)?;
            total += 1;
            match cutoff {
                Some(cutoff) if sample.at < cutoff => None,
                _ => Some(sample),
            }
        })
        .collect();
    samples.sort_by_key(|sample| sample.at);
    (samples, total)
}

/// Add one sample to the end of the file, creating it if this is a first run.
pub fn append(sample: &Sample) -> std::io::Result<()> {
    let Some(line) = line(sample) else {
        return Ok(());
    };
    let path = path();
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?
        .write_all(line.as_bytes())?;

    // Compaction otherwise only happens on load, so a long-running session would
    // append past the horizon indefinitely.
    if fs::metadata(&path).is_ok_and(|file| file.len() > MAX_BYTES) {
        let (samples, _) = within_horizon();
        rewrite(&samples)?;
    }
    Ok(())
}

/// Replace the file with exactly `samples`. Written to a sibling temporary and
/// renamed, so an interrupted compaction cannot truncate the real history.
fn rewrite(samples: &[Sample]) -> std::io::Result<()> {
    let path = path();
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let temporary = path.with_extension("tsv.tmp");
    let mut file = File::create(&temporary)?;
    for sample in samples {
        if let Some(line) = line(sample) {
            file.write_all(line.as_bytes())?;
        }
    }
    file.sync_all()?;
    fs::rename(&temporary, &path)
}

fn line(sample: &Sample) -> Option<String> {
    let secs = sample.at.duration_since(UNIX_EPOCH).ok()?.as_secs();
    let power = sample
        .power
        .map_or_else(|| "-".to_owned(), |watts| format!("{watts:.2}"));
    let marker = sample.follows.as_key();
    Some(format!(
        "{secs}\t{:.1}\t{power}\t{}\t{marker}\n",
        sample.percent,
        sample.status.as_key()
    ))
}

fn parse(line: &str) -> Option<Sample> {
    let mut fields = line.split('\t');
    let secs: u64 = fields.next()?.trim().parse().ok()?;
    let percent: f64 = fields.next()?.parse().ok()?;
    let power = match fields.next()? {
        "-" => None,
        watts => Some(watts.parse().ok()?),
    };
    let status = Status::from_key(fields.next()?.trim());
    // Absent on every line written before the marker existed.
    let follows = fields
        .next()
        .map_or(Follows::Poll, |marker| Follows::from_key(marker.trim()));
    Some(Sample {
        at: UNIX_EPOCH + Duration::from_secs(secs),
        percent,
        power,
        status,
        follows,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(secs: u64, power: Option<f64>) -> Sample {
        Sample {
            at: UNIX_EPOCH + Duration::from_secs(secs),
            percent: 41.5,
            power,
            status: Status::Charging,
            follows: Follows::Poll,
        }
    }

    #[test]
    fn a_sample_survives_a_round_trip() {
        let written = line(&sample(1_780_000_000, Some(68.031))).unwrap();
        assert_eq!(written, "1780000000\t41.5\t68.03\tcharging\t\n");

        let read = parse(written.trim_end()).unwrap();
        assert_eq!(read.at, sample(1_780_000_000, None).at);
        assert_eq!(read.percent, 41.5);
        assert_eq!(read.power, Some(68.03));
        assert_eq!(read.status, Status::Charging);
    }

    #[test]
    fn a_missing_power_reading_round_trips_as_a_dash() {
        let written = line(&sample(1_780_000_000, None)).unwrap();
        assert!(written.contains("\t-\t"));
        assert_eq!(parse(written.trim_end()).unwrap().power, None);
    }

    #[test]
    fn markers_round_trip_and_old_lines_still_load() {
        for follows in [Follows::Sleep, Follows::Launch, Follows::Poll] {
            let mut marked = sample(1_780_000_000, None);
            marked.follows = follows;
            let written = line(&marked).unwrap();
            assert_eq!(parse(written.trim_end()).unwrap().follows, follows);
        }

        // A line from before the column existed: four fields, no marker.
        let old = parse("1780000000\t41.5\t-\tcharging").unwrap();
        assert_eq!(old.follows, Follows::Poll);
        // An unrecognised marker is not a reason to drop the reading.
        let future = parse("1780000000\t41.5\t-\tcharging\thibernated").unwrap();
        assert_eq!(future.follows, Follows::Poll);
    }

    #[test]
    fn junk_lines_are_skipped_not_fatal() {
        assert!(parse("").is_none());
        assert!(parse("# leyden history").is_none());
        assert!(parse("1780000000\t41.5").is_none());
        assert!(parse("not-a-number\t41.5\t-\tcharging").is_none());
        // An unknown status is readable — only the parse rules are strict.
        assert_eq!(
            parse("1780000000\t41.5\t-\tsomething-new").unwrap().status,
            Status::Unknown
        );
    }
}
