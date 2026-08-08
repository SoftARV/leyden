// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! Completed runs: one discharge or one charge, start to finish.
//!
//! The sample ring cannot answer "the last five discharges" — it holds a day,
//! and a day is not five cycles. So runs are summarised into their own small
//! file, which is tiny and long-lived where samples are bulky and short-lived.
//!
//! Each run carries **its own series** — at most `SERIES_POINTS` pairs of
//! `offset:percent` — so it stays drawable long after its samples have aged out
//! of the ring.
//!
//! The offsets are kept rather than resampled onto an even grid. An even grid
//! has to invent a value for every slot, including the slots inside a gap, so a
//! discharge containing an eight-hour sleep drew exactly like one that did not.
//! Downsampling by stride keeps real observations and lets a gap stay a gap.
//!
//! `runs_in` is a pure function over a `History`, which is what makes the rules
//! below testable — every one of them came from replaying real data rather than
//! from guesswork.

use std::fs::{self, File};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::battery::history::{Follows, History, Sample, elapsed_secs};
use crate::battery::types::Status;
use crate::store;

/// Shorter than this and it is not a run, it is the gauge twitching. Half the
/// runs in the first real history were blips: a 30-second `full`, and two
/// sub-two-minute discharge fragments.
pub const MIN_RUN_SECS: f64 = 5.0 * 60.0;

/// Enough to shape a small graph, few enough to keep a line short.
pub const SERIES_POINTS: usize = 64;

/// How many of each kind are kept on disk. Five are shown; the rest are slack so
/// the file does not have to be rewritten on every completed run.
const KEEP_PER_KIND: usize = 20;

/// How many of each kind the dialog lists.
pub const SHOWN_PER_KIND: usize = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Discharge,
    Charge,
}

impl Kind {
    /// The trend a reading belongs to. `Full` and `Not charging` end a run
    /// rather than starting one — reaching full *is* the end of a charge.
    fn of(status: Status) -> Option<Self> {
        match status {
            Status::Discharging => Some(Kind::Discharge),
            Status::Charging => Some(Kind::Charge),
            _ => None,
        }
    }

    pub fn as_key(self) -> &'static str {
        match self {
            Kind::Discharge => "discharge",
            Kind::Charge => "charge",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "discharge" => Some(Kind::Discharge),
            "charge" => Some(Kind::Charge),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Run {
    pub kind: Kind,
    pub started: SystemTime,
    pub ended: SystemTime,
    pub from_percent: f64,
    pub to_percent: f64,
    /// Mean of the readings taken during the run, in watts.
    pub watts: Option<f64>,
    /// `(seconds from the start, percent)` — real observations, thinned but not
    /// interpolated, so the gaps within a run survive.
    pub series: Vec<(f64, f64)>,
    /// Still running: `ended` is simply the newest reading so far.
    pub in_progress: bool,
}

impl Run {
    pub fn elapsed(&self) -> Duration {
        Duration::from_secs_f64(elapsed_secs(self.started, self.ended))
    }

    /// Charge moved, as a positive magnitude in either direction.
    pub fn moved(&self) -> f64 {
        (self.to_percent - self.from_percent).abs()
    }

    /// Percent per hour, the number that makes two runs comparable.
    pub fn rate(&self) -> Option<f64> {
        let hours = self.elapsed().as_secs_f64() / 3600.0;
        (hours > 0.0).then(|| self.moved() / hours)
    }

    fn long_enough(&self) -> bool {
        elapsed_secs(self.started, self.ended) >= MIN_RUN_SECS
    }
}

/// Every run the history covers, oldest first. The last one may be in progress.
///
/// A gap does **not** end a run when the trend matches on both sides and the
/// charge kept moving the same way; without that, one 73-minute discharge came
/// back as three fragments split by app restarts. If the charge moved the other
/// way across the gap, something was plugged in unobserved and the run is split.
pub fn runs_in(history: &History) -> Vec<Run> {
    let mut runs: Vec<Run> = Vec::new();
    let mut open: Option<Open> = None;

    for sample in history.iter() {
        let kind = Kind::of(sample.status);
        let broken = matches!(sample.follows, Follows::Sleep | Follows::Launch);

        match (&mut open, kind) {
            // The run continues, unless a gap makes it a different one.
            (Some(current), Some(kind))
                if current.kind == kind && current.survives(sample, broken) =>
            {
                current.push(sample);
            }
            // Any other reading closes whatever was open.
            (_, next) => {
                if let Some(current) = open.take() {
                    runs.push(current.finish(Some(sample), false));
                }
                open = next.map(|kind| Open::start(kind, sample));
            }
        }
    }

    if let Some(current) = open {
        runs.push(current.finish(None, true));
    }
    runs.retain(Run::long_enough);
    runs
}

/// A run being accumulated.
struct Open {
    kind: Kind,
    started: SystemTime,
    from_percent: f64,
    last: SystemTime,
    last_percent: f64,
    watts: Vec<f64>,
    points: Vec<(f64, f64)>,
}

impl Open {
    fn start(kind: Kind, sample: &Sample) -> Self {
        let mut open = Open {
            kind,
            started: sample.at,
            from_percent: sample.percent,
            last: sample.at,
            last_percent: sample.percent,
            watts: Vec::new(),
            points: Vec::new(),
        };
        open.push(sample);
        open
    }

    fn push(&mut self, sample: &Sample) {
        self.last = sample.at;
        self.last_percent = sample.percent;
        if let Some(power) = sample.power {
            self.watts.push(power);
        }
        self.points
            .push((elapsed_secs(self.started, sample.at), sample.percent));
    }

    /// Whether `sample` belongs to this run despite a gap before it.
    fn survives(&self, sample: &Sample, broken: bool) -> bool {
        if !broken {
            return true;
        }
        let moved = sample.percent - self.last_percent;
        match self.kind {
            // Charge kept falling across the gap: still the same discharge.
            Kind::Discharge => moved <= 0.0,
            Kind::Charge => moved >= 0.0,
        }
    }

    fn finish(mut self, ending: Option<&Sample>, in_progress: bool) -> Run {
        // The reading that ended the run is its endpoint: a charge that stopped
        // because the battery filled should read as reaching 100% — and its
        // shape has to reach 100% too, or the row and the graph disagree.
        let (ended, to_percent) = match ending {
            Some(sample) => {
                self.points
                    .push((elapsed_secs(self.started, sample.at), sample.percent));
                (sample.at, sample.percent)
            }
            None => (self.last, self.last_percent),
        };
        let watts = (!self.watts.is_empty())
            .then(|| self.watts.iter().sum::<f64>() / self.watts.len() as f64);
        Run {
            kind: self.kind,
            started: self.started,
            ended,
            from_percent: self.from_percent,
            to_percent,
            watts,
            series: downsample(&self.points),
            in_progress,
        }
    }
}

/// Thin the points to at most `SERIES_POINTS`, keeping every one that survives
/// as an actual observation.
///
/// Deliberately not an interpolation onto an even grid: that has to invent a
/// value for every slot, gaps included, which is precisely the information a run
/// graph should not lose.
fn downsample(points: &[(f64, f64)]) -> Vec<(f64, f64)> {
    if points.len() <= SERIES_POINTS {
        return points.to_vec();
    }
    let stride = points.len().div_ceil(SERIES_POINTS);
    let mut thinned: Vec<(f64, f64)> = points.iter().step_by(stride).copied().collect();
    // The end of a run is its most interesting point; never let stride drop it.
    if let (Some(last), Some(kept)) = (points.last(), thinned.last())
        && last != kept
    {
        thinned.push(*last);
    }
    thinned
}

fn path() -> PathBuf {
    store::data_dir().join("recordings.tsv")
}

/// Every recorded run, oldest first. A missing file is a first run.
pub fn load() -> Vec<Run> {
    let Ok(file) = File::open(path()) else {
        return Vec::new();
    };
    let mut runs: Vec<Run> = BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .filter_map(|line| parse(&line))
        .collect();
    runs.sort_by_key(|run| run.started);
    runs
}

/// Replace the file with `runs`, newest `KEEP_PER_KIND` of each kind.
///
/// Rewritten rather than appended: a run completing is rare — a handful a day —
/// and rewriting a file of at most forty lines keeps the trimming in one place.
pub fn save(runs: &[Run]) -> Result<(), String> {
    let mut kept: Vec<&Run> = Vec::new();
    for kind in [Kind::Discharge, Kind::Charge] {
        let mut of_kind: Vec<&Run> = runs
            .iter()
            .filter(|run| run.kind == kind && !run.in_progress)
            .collect();
        of_kind.sort_by_key(|run| run.started);
        kept.extend(of_kind.iter().rev().take(KEEP_PER_KIND).rev());
    }
    kept.sort_by_key(|run| run.started);

    let path = path();
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|error| error.to_string())?;
    }
    let temporary = path.with_extension("tsv.tmp");
    let mut file = File::create(&temporary).map_err(|error| error.to_string())?;
    for run in kept {
        if let Some(line) = line(run) {
            file.write_all(line.as_bytes())
                .map_err(|error| error.to_string())?;
        }
    }
    file.sync_all().map_err(|error| error.to_string())?;
    fs::rename(&temporary, &path).map_err(|error| error.to_string())
}

/// Merge freshly derived runs into the known set, keyed by when they started.
/// A run already on disk is replaced, so an in-progress run becomes its
/// completed self rather than a duplicate.
pub fn merge(known: &mut Vec<Run>, derived: Vec<Run>) -> bool {
    let mut changed = false;
    for run in derived {
        match known
            .iter_mut()
            .find(|existing| existing.started == run.started)
        {
            Some(existing) if *existing != run => {
                *existing = run;
                changed = true;
            }
            Some(_) => {}
            None => {
                known.push(run);
                changed = true;
            }
        }
    }
    known.sort_by_key(|run| run.started);
    changed
}

/// The newest `SHOWN_PER_KIND` of one kind, newest first, with any in-progress
/// run at the front where it is most useful.
pub fn latest(runs: &[Run], kind: Kind) -> Vec<&Run> {
    let mut of_kind: Vec<&Run> = runs.iter().filter(|run| run.kind == kind).collect();
    of_kind.sort_by_key(|run| run.started);
    of_kind.reverse();
    of_kind.truncate(SHOWN_PER_KIND);
    of_kind
}

fn line(run: &Run) -> Option<String> {
    let started = run.started.duration_since(UNIX_EPOCH).ok()?.as_secs();
    let ended = run.ended.duration_since(UNIX_EPOCH).ok()?.as_secs();
    let watts = run
        .watts
        .map_or_else(|| "-".to_owned(), |watts| format!("{watts:.2}"));
    let series: Vec<String> = run
        .series
        .iter()
        .map(|(at, percent)| format!("{at:.0}:{percent:.1}"))
        .collect();
    Some(format!(
        "{}\t{started}\t{ended}\t{:.1}\t{:.1}\t{watts}\t{}\n",
        run.kind.as_key(),
        run.from_percent,
        run.to_percent,
        series.join(",")
    ))
}

fn parse(line: &str) -> Option<Run> {
    let mut fields = line.split('\t');
    let kind = Kind::from_key(fields.next()?.trim())?;
    let started = UNIX_EPOCH + Duration::from_secs(fields.next()?.trim().parse().ok()?);
    let ended = UNIX_EPOCH + Duration::from_secs(fields.next()?.trim().parse().ok()?);
    let from_percent: f64 = fields.next()?.parse().ok()?;
    let to_percent: f64 = fields.next()?.parse().ok()?;
    let watts = match fields.next()? {
        "-" => None,
        watts => Some(watts.parse().ok()?),
    };
    // Pairs only. A line from the first format stored bare percentages with no
    // offsets; those cannot say where a gap was, so they load as no series at
    // all and the run simply shows without a plot.
    let series = fields
        .next()
        .map(|series| {
            series
                .trim()
                .split(',')
                .filter_map(|point| {
                    let (at, percent) = point.split_once(':')?;
                    Some((at.parse().ok()?, percent.parse().ok()?))
                })
                .collect()
        })
        .unwrap_or_default();
    Some(Run {
        kind,
        started,
        ended,
        from_percent,
        to_percent,
        watts,
        series,
        in_progress: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::UNIX_EPOCH;

    fn history_of(entries: &[(u64, f64, Status, Follows)]) -> History {
        let mut history = History::new(4096);
        for (secs, percent, status, follows) in entries {
            history.push(Sample {
                at: UNIX_EPOCH + Duration::from_secs(*secs),
                percent: *percent,
                power: Some(10.0),
                status: *status,
                follows: *follows,
            });
        }
        history
    }

    /// A run of `count` readings 30s apart, walking `percent` by `step`.
    fn walk(
        from: u64,
        count: u64,
        percent: f64,
        step: f64,
        status: Status,
    ) -> Vec<(u64, f64, Status, Follows)> {
        (0..count)
            .map(|i| {
                (
                    from + i * 30,
                    percent + step * i as f64,
                    status,
                    Follows::Poll,
                )
            })
            .collect()
    }

    #[test]
    fn a_discharge_then_a_charge_are_two_runs() {
        let mut entries = walk(0, 40, 80.0, -0.5, Status::Discharging);
        entries.extend(walk(1200, 40, 60.0, 0.5, Status::Charging));
        let runs = runs_in(&history_of(&entries));

        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].kind, Kind::Discharge);
        assert_eq!(runs[1].kind, Kind::Charge);
        // The reading that ended the first run is its endpoint.
        assert_eq!(runs[0].to_percent, 60.0);
        assert!(runs[1].in_progress, "the newest run is still going");
        assert!(!runs[0].in_progress);
    }

    #[test]
    fn reaching_full_ends_a_charge() {
        let mut entries = walk(0, 40, 80.0, 0.5, Status::Charging);
        entries.push((1200, 100.0, Status::Full, Follows::Poll));
        let runs = runs_in(&history_of(&entries));

        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].kind, Kind::Charge);
        assert_eq!(runs[0].to_percent, 100.0, "it reached full");
        assert!(!runs[0].in_progress, "full closed it");
        // The graph has to agree with the row it sits under.
        assert_eq!(
            runs[0].series.last().map(|(_, percent)| *percent),
            Some(100.0),
            "the shape reaches full too"
        );
    }

    #[test]
    fn blips_are_not_runs() {
        // Four readings — two minutes — either side of a real discharge.
        let mut entries = walk(0, 4, 100.0, 0.0, Status::Full);
        entries.extend(walk(1000, 40, 90.0, -0.5, Status::Discharging));
        let runs = runs_in(&history_of(&entries));
        assert_eq!(runs.len(), 1, "only the discharge is long enough");
        assert_eq!(runs[0].kind, Kind::Discharge);
    }

    #[test]
    fn a_restart_does_not_split_one_discharge() {
        // The real case: a 73-minute discharge came back as three fragments
        // because each app restart broke it.
        let mut entries = walk(0, 40, 80.0, -0.25, Status::Discharging);
        let mut after = walk(1300, 40, 69.0, -0.25, Status::Discharging);
        after[0].3 = Follows::Launch;
        entries.extend(after);

        let runs = runs_in(&history_of(&entries));
        assert_eq!(runs.len(), 1, "one discharge, not two");
        assert_eq!(runs[0].from_percent, 80.0);
    }

    #[test]
    fn charging_across_a_gap_splits_the_discharge() {
        // Same trend either side, but the charge went *up* while nobody was
        // watching — so it was plugged in and this is a different discharge.
        let mut entries = walk(0, 40, 80.0, -0.25, Status::Discharging);
        let mut after = walk(1300, 40, 95.0, -0.25, Status::Discharging);
        after[0].3 = Follows::Launch;
        entries.extend(after);

        let runs = runs_in(&history_of(&entries));
        assert_eq!(runs.len(), 2, "the charge rose across the gap");
    }

    fn run(kind: Kind, started: u64, in_progress: bool) -> Run {
        Run {
            kind,
            started: UNIX_EPOCH + Duration::from_secs(started),
            ended: UNIX_EPOCH + Duration::from_secs(started + 3600),
            from_percent: 80.0,
            to_percent: 40.0,
            watts: Some(11.25),
            series: vec![(0.0, 80.0), (1800.0, 60.0), (3600.0, 40.0)],
            in_progress,
        }
    }

    #[test]
    fn a_run_survives_a_round_trip_through_the_file_format() {
        let original = run(Kind::Discharge, 1_780_000_000, false);
        let written = line(&original).unwrap();
        assert_eq!(parse(written.trim_end()).unwrap(), original);

        // A line with no series still loads; the graph simply has nothing to draw.
        let bare = parse("charge\t100\t200\t10.0\t20.0\t-").unwrap();
        assert_eq!(bare.kind, Kind::Charge);
        assert!(bare.series.is_empty());
        assert_eq!(bare.watts, None);

        // A line in the first format — bare percentages, no offsets — cannot say
        // where its gaps were, so it loads with no series rather than a lie.
        let old = parse("charge\t100\t200\t10.0\t20.0\t-\t80.0,60.0,40.0").unwrap();
        assert!(old.series.is_empty());

        assert!(parse("nonsense\t1\t2\t3\t4\t5").is_none());
    }

    #[test]
    fn merging_replaces_a_run_rather_than_duplicating_it() {
        let mut known = vec![run(Kind::Discharge, 100, true)];
        // The same run, now finished.
        let finished = run(Kind::Discharge, 100, false);

        assert!(merge(&mut known, vec![finished.clone()]));
        assert_eq!(known.len(), 1, "same start time, same run");
        assert!(!known[0].in_progress);

        // Merging it again changes nothing.
        assert!(!merge(&mut known, vec![finished]));
    }

    #[test]
    fn only_the_newest_few_are_listed_newest_first() {
        let runs: Vec<Run> = (0..8)
            .map(|i| run(Kind::Discharge, 1000 + i * 10_000, i < 7))
            .collect();
        let listed = latest(&runs, Kind::Discharge);

        assert_eq!(listed.len(), SHOWN_PER_KIND);
        assert!(listed[0].started > listed[1].started, "newest first");
        assert!(latest(&runs, Kind::Charge).is_empty());
    }

    #[test]
    fn a_gap_inside_a_run_survives_the_thinning() {
        // A discharge interrupted by a long sleep: the stored shape must still
        // show that nothing was observed in the middle.
        let mut entries = walk(0, 40, 80.0, -0.25, Status::Discharging);
        let mut after = walk(30_000, 40, 60.0, -0.25, Status::Discharging);
        after[0].3 = Follows::Sleep;
        entries.extend(after);

        let runs = runs_in(&history_of(&entries));
        assert_eq!(runs.len(), 1, "one discharge across the sleep");

        let biggest = runs[0]
            .series
            .windows(2)
            .map(|pair| pair[1].0 - pair[0].0)
            .fold(0.0_f64, f64::max);
        assert!(
            biggest > 20_000.0,
            "the unobserved stretch must survive as a real jump, got {biggest}"
        );
    }

    #[test]
    fn a_run_carries_its_own_shape_and_rate() {
        let runs = runs_in(&history_of(&walk(0, 120, 80.0, -0.25, Status::Discharging)));
        let run = &runs[0];

        assert!(run.series.len() <= SERIES_POINTS);
        assert!(run.series.len() > 2, "enough points to draw");
        assert_eq!(run.series.first().map(|(_, percent)| *percent), Some(80.0));
        assert_eq!(run.series.first().map(|(at, _)| *at), Some(0.0));
        assert!((run.watts.unwrap() - 10.0).abs() < 0.001);
        // 29.75% over 59.5 minutes is very close to 30%/h.
        assert!((run.rate().unwrap() - 30.0).abs() < 0.5, "{:?}", run.rate());
    }
}
