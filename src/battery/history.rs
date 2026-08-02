// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! In-memory sample ring for the charge/drain graph. Session-scoped: nothing is
//! persisted yet, so the window starts empty on every launch.
//!
//! The ring already fills on every poll; the graph that reads it is the next
//! milestone, hence the allow.
#![allow(dead_code)]

use std::collections::VecDeque;
use std::time::Instant;

use super::types::Status;

/// One poll's worth of the values the graph plots.
#[derive(Debug, Clone, Copy)]
pub struct Sample {
    pub at: Instant,
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

    /// Seconds between the oldest and newest sample — the graph's x extent.
    pub fn span_secs(&self) -> f64 {
        match (self.samples.front(), self.samples.back()) {
            (Some(first), Some(last)) => last.at.duration_since(first.at).as_secs_f64(),
            _ => 0.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(percent: f64) -> Sample {
        Sample {
            at: Instant::now(),
            percent,
            power: None,
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
}
