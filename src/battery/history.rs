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
            status: Status::Discharging,
        }
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
                status: Status::Discharging,
            });
        }
        assert_eq!(history.span_secs(), 90.0);
    }
}
