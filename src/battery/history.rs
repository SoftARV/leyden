// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! In-memory sample ring for the charge graph. Session-scoped: nothing is
//! persisted yet, so the window starts empty on every launch.
//!
//! Samples are stamped with `SystemTime`, not `Instant`: `Instant` does not
//! advance while the machine is suspended, which would draw an overnight sleep
//! as an instant vertical drop.

use std::collections::VecDeque;
use std::time::SystemTime;

use super::types::Status;

/// Samples further apart than this did not just miss a poll — the machine slept
/// or the window was hidden and the timer came off. Nothing may be interpolated
/// or averaged across such a gap.
pub const GAP_SECS: f64 = 15.0;

/// Seconds from `from` to `to`, clamped at zero so a clock stepping backwards
/// cannot produce a negative x coordinate.
pub fn elapsed_secs(from: SystemTime, to: SystemTime) -> f64 {
    to.duration_since(from)
        .map_or(0.0, |elapsed| elapsed.as_secs_f64())
}

#[derive(Debug, Clone, Copy)]
pub struct Sample {
    pub at: SystemTime,
    pub percent: f64,
    pub power: Option<f64>,
    pub status: Status,
}

#[derive(Debug)]
pub struct History {
    samples: VecDeque<Sample>,
    capacity: usize,
}

impl History {
    pub fn new(capacity: usize) -> Self {
        Self {
            samples: VecDeque::with_capacity(capacity),
            capacity,
        }
    }

    pub fn push(&mut self, sample: Sample) {
        if self.samples.len() == self.capacity {
            self.samples.pop_front();
        }
        self.samples.push_back(sample);
    }

    pub fn iter(&self) -> impl Iterator<Item = &Sample> {
        self.samples.iter()
    }

    pub fn len(&self) -> usize {
        self.samples.len()
    }

    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    /// Mean power over the trailing `window_secs`, walking back from the newest
    /// sample and stopping at the first thing that would poison the average: a
    /// different `Status` (a charge rate says nothing about a drain rate) or a
    /// gap (the samples either side of a suspend are not neighbours in time).
    pub fn recent_power(&self, window_secs: f64) -> Option<f64> {
        let newest = self.samples.back()?;
        let mut total = 0.0;
        let mut count = 0usize;
        let mut later = newest.at;

        for sample in self.samples.iter().rev() {
            if sample.status != newest.status
                || elapsed_secs(sample.at, later) > GAP_SECS
                || elapsed_secs(sample.at, newest.at) > window_secs
            {
                break;
            }
            if let Some(power) = sample.power {
                total += power;
                count += 1;
            }
            later = sample.at;
        }

        (count > 0).then(|| total / count as f64)
    }

    /// Seconds between the oldest and newest sample — the graph's x extent.
    pub fn span_secs(&self) -> f64 {
        match (self.samples.front(), self.samples.back()) {
            (Some(first), Some(last)) => elapsed_secs(first.at, last.at),
            _ => 0.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(percent: f64) -> Sample {
        Sample {
            at: SystemTime::now(),
            percent,
            power: None,
            status: Status::Discharging,
        }
    }

    fn history(entries: &[(u64, f64, Status)]) -> History {
        let mut history = History::new(100);
        for (secs, power, status) in entries {
            history.push(Sample {
                at: SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(*secs),
                percent: 50.0,
                power: Some(*power),
                status: *status,
            });
        }
        history
    }

    #[test]
    fn drops_the_oldest_past_capacity() {
        let mut history = History::new(2);
        history.push(sample(1.0));
        history.push(sample(2.0));
        history.push(sample(3.0));
        let percents: Vec<f64> = history.iter().map(|s| s.percent).collect();
        assert_eq!(percents, vec![2.0, 3.0]);
    }

    #[test]
    fn span_covers_oldest_to_newest() {
        let mut history = History::new(4);
        let start = SystemTime::UNIX_EPOCH;
        for offset in [0, 30, 90] {
            history.push(Sample {
                at: start + std::time::Duration::from_secs(offset),
                percent: 50.0,
                power: None,
                status: Status::Discharging,
            });
        }
        assert_eq!(history.span_secs(), 90.0);
    }

    #[test]
    fn power_averages_over_the_window() {
        let history = history(&[
            (0, 10.0, Status::Discharging),
            (2, 20.0, Status::Discharging),
            (4, 30.0, Status::Discharging),
        ]);
        assert_eq!(history.recent_power(60.0), Some(20.0));
    }

    #[test]
    fn power_ignores_samples_older_than_the_window() {
        let history = history(&[
            (0, 100.0, Status::Discharging),
            (58, 10.0, Status::Discharging),
            (60, 20.0, Status::Discharging),
        ]);
        // The 100 W sample is 60s back — outside a 30s window, so it is dropped.
        assert_eq!(history.recent_power(30.0), Some(15.0));
    }

    #[test]
    fn power_never_averages_across_a_state_change() {
        let history = history(&[
            (0, 60.0, Status::Charging),
            (2, 60.0, Status::Charging),
            (4, 12.0, Status::Discharging),
        ]);
        // Unplugging must not blend 60 W of charging into the drain estimate.
        assert_eq!(history.recent_power(600.0), Some(12.0));
    }

    #[test]
    fn power_never_averages_across_a_gap() {
        let history = history(&[
            (0, 40.0, Status::Discharging),
            (2, 40.0, Status::Discharging),
            (7200, 10.0, Status::Discharging),
        ]);
        assert_eq!(history.recent_power(f64::MAX), Some(10.0));
    }

    #[test]
    fn power_is_none_without_readings() {
        let mut history = History::new(4);
        history.push(sample(50.0));
        assert_eq!(history.recent_power(60.0), None);
        assert_eq!(History::new(4).recent_power(60.0), None);
    }
}
