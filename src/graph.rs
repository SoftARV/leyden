// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! The charge plot: percentage against wall-clock time, coloured by what the
//! battery was doing. Colours come from the Adwaita accent, never hard-coded.

use relm4::adw;
use relm4::adw::prelude::*;
use relm4::gtk;
use relm4::gtk::cairo;
use relm4::gtk::gdk;

use crate::battery::history::{GAP_SECS, History, elapsed_secs};
use crate::battery::types::Status;

const PAD: f64 = 14.0;
const LINE_WIDTH: f64 = 2.0;
const FILL_ALPHA: f64 = 0.14;
const GRID_ALPHA: f64 = 0.10;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Trend {
    Charging,
    Discharging,
    Idle,
}

impl Trend {
    fn of(status: Status) -> Self {
        match status {
            Status::Charging => Trend::Charging,
            Status::Discharging => Trend::Discharging,
            _ => Trend::Idle,
        }
    }

    fn rgba(self, dark: bool, foreground: gdk::RGBA) -> gdk::RGBA {
        match self {
            Trend::Charging => adw::AccentColor::Green.to_standalone_rgba(dark),
            Trend::Discharging => adw::StyleManager::default().accent_color_rgba(),
            Trend::Idle => {
                let mut dim = foreground;
                dim.set_alpha(0.45);
                dim
            }
        }
    }
}

struct Segment {
    trend: Trend,
    /// Whether a real gap precedes this segment, as opposed to just a change of
    /// trend. Only a gap breaks the filled area.
    broke: bool,
    /// (seconds since the first sample, percent)
    points: Vec<(f64, f64)>,
}

/// A drawable snapshot of the history, owned so the draw closure needs no
/// shared mutable state.
pub struct Series {
    segments: Vec<Segment>,
    span: f64,
}

impl Series {
    pub fn from_history(history: &History) -> Self {
        let mut series = Series {
            segments: Vec::new(),
            span: history.span_secs(),
        };
        if history.is_empty() {
            return series;
        }

        let mut previous: Option<(f64, f64, Trend)> = None;
        let start = history.iter().next().map(|sample| sample.at);
        for sample in history.iter() {
            let Some(start) = start else { continue };
            let at = elapsed_secs(start, sample.at);
            let trend = Trend::of(sample.status);
            let gap = previous.is_some_and(|(last, ..)| at - last > GAP_SECS);
            let changed = previous.is_some_and(|(.., last)| last != trend);

            if previous.is_none() || gap || changed {
                let mut points = Vec::new();
                // A colour change with no gap carries the previous point over, so
                // the line stays unbroken where only the trend changed.
                if changed
                    && !gap
                    && let Some((last_at, last_percent, _)) = previous
                {
                    points.push((last_at, last_percent));
                }
                series.segments.push(Segment {
                    trend,
                    broke: gap || previous.is_none(),
                    points,
                });
            }

            if let Some(segment) = series.segments.last_mut() {
                segment.points.push((at, sample.percent));
            }
            previous = Some((at, sample.percent, trend));
        }
        series
    }

    fn points(&self) -> usize {
        self.segments.iter().map(|s| s.points.len()).sum()
    }

    /// Segments merged back together across trend changes, so the filled area
    /// is continuous and only a real gap splits it. Without this a two-sample
    /// discharge blip while charging fills as a solid vertical bar.
    fn runs(&self) -> Vec<Vec<(f64, f64)>> {
        let mut runs: Vec<Vec<(f64, f64)>> = Vec::new();
        for segment in &self.segments {
            match runs.last_mut() {
                Some(run) if !segment.broke => {
                    // A trend change repeats the boundary point; drop the copy.
                    run.extend(segment.points.iter().skip(1).copied());
                }
                _ => runs.push(segment.points.clone()),
            }
        }
        runs
    }

    pub fn draw_func(self) -> impl Fn(&gtk::DrawingArea, &cairo::Context, i32, i32) + 'static {
        move |area, cr, width, height| self.draw(area, cr, width, height)
    }

    fn draw(&self, area: &gtk::DrawingArea, cr: &cairo::Context, width: i32, height: i32) {
        let (width, height) = (f64::from(width), f64::from(height));
        let (plot_w, plot_h) = (width - PAD * 2.0, height - PAD * 2.0);
        if plot_w <= 0.0 || plot_h <= 0.0 {
            return;
        }

        let foreground = area.color();
        let dark = adw::StyleManager::default().is_dark();
        let y = |percent: f64| PAD + (1.0 - percent / 100.0) * plot_h;

        cr.set_line_width(1.0);
        cr.set_source_rgba(
            f64::from(foreground.red()),
            f64::from(foreground.green()),
            f64::from(foreground.blue()),
            GRID_ALPHA,
        );
        for percent in [0.0, 25.0, 50.0, 75.0, 100.0] {
            cr.move_to(PAD, y(percent));
            cr.line_to(width - PAD, y(percent));
        }
        cr.stroke().ok();

        if self.span <= 0.0 || self.points() < 2 {
            return;
        }

        let x = |at: f64| PAD + at / self.span * plot_w;
        cr.set_line_width(LINE_WIDTH);
        cr.set_line_join(cairo::LineJoin::Round);
        cr.set_line_cap(cairo::LineCap::Round);

        // One fill for the whole run, tinted by what the battery is doing now,
        // then the per-trend strokes on top of it.
        let current = self.segments.last().map_or(Trend::Idle, |s| s.trend);
        let fill = current.rgba(dark, foreground);
        cr.set_source_rgba(
            f64::from(fill.red()),
            f64::from(fill.green()),
            f64::from(fill.blue()),
            FILL_ALPHA,
        );
        for run in self.runs() {
            let Some((first, rest)) = run.split_first() else {
                continue;
            };
            let Some(last) = rest.last() else { continue };
            cr.move_to(x(first.0), y(first.1));
            for point in rest {
                cr.line_to(x(point.0), y(point.1));
            }
            cr.line_to(x(last.0), height - PAD);
            cr.line_to(x(first.0), height - PAD);
            cr.close_path();
            cr.fill().ok();
        }

        for segment in &self.segments {
            let Some((first, rest)) = segment.points.split_first() else {
                continue;
            };
            if rest.is_empty() {
                continue;
            }
            let color = segment.trend.rgba(dark, foreground);
            cr.move_to(x(first.0), y(first.1));
            for point in rest {
                cr.line_to(x(point.0), y(point.1));
            }
            cr.set_source_rgba(
                f64::from(color.red()),
                f64::from(color.green()),
                f64::from(color.blue()),
                f64::from(color.alpha()),
            );
            cr.stroke().ok();
        }

        if let Some(segment) = self.segments.last()
            && let Some(last) = segment.points.last()
        {
            let color = segment.trend.rgba(dark, foreground);
            cr.set_source_rgba(
                f64::from(color.red()),
                f64::from(color.green()),
                f64::from(color.blue()),
                f64::from(color.alpha()),
            );
            cr.arc(x(last.0), y(last.1), 3.5, 0.0, std::f64::consts::TAU);
            cr.fill().ok();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::battery::history::Sample;
    use std::time::{Duration, SystemTime};

    fn history(entries: &[(u64, f64, Status)]) -> History {
        let start = SystemTime::UNIX_EPOCH;
        let mut history = History::new(100);
        for (secs, percent, status) in entries {
            history.push(Sample {
                at: start + Duration::from_secs(*secs),
                percent: *percent,
                power: None,
                status: *status,
            });
        }
        history
    }

    #[test]
    fn a_steady_discharge_is_one_segment() {
        let series = Series::from_history(&history(&[
            (0, 80.0, Status::Discharging),
            (2, 79.0, Status::Discharging),
            (4, 78.0, Status::Discharging),
        ]));
        assert_eq!(series.segments.len(), 1);
        assert_eq!(series.points(), 3);
    }

    #[test]
    fn a_suspend_breaks_the_line() {
        let series = Series::from_history(&history(&[
            (0, 80.0, Status::Discharging),
            (2, 79.0, Status::Discharging),
            (7200, 40.0, Status::Discharging),
        ]));
        assert_eq!(series.segments.len(), 2);
        assert_eq!(series.segments[1].points.len(), 1);
    }

    #[test]
    fn plugging_in_changes_colour_without_breaking_the_line() {
        let series = Series::from_history(&history(&[
            (0, 80.0, Status::Discharging),
            (2, 79.0, Status::Discharging),
            (4, 79.0, Status::Charging),
        ]));
        assert_eq!(series.segments.len(), 2);
        // The charging segment starts at the last discharging point, so the two
        // meet instead of leaving a hole.
        assert_eq!(series.segments[1].points.len(), 2);
        assert_eq!(series.segments[1].points[0], (2.0, 79.0));
    }

    #[test]
    fn only_a_gap_splits_the_filled_area() {
        let plugged_in = Series::from_history(&history(&[
            (0, 80.0, Status::Discharging),
            (2, 79.0, Status::Discharging),
            (4, 79.0, Status::Charging),
        ]));
        assert_eq!(plugged_in.segments.len(), 2);
        assert_eq!(plugged_in.runs().len(), 1);
        assert_eq!(plugged_in.runs()[0].len(), 3);

        let slept = Series::from_history(&history(&[
            (0, 80.0, Status::Discharging),
            (2, 79.0, Status::Discharging),
            (7200, 40.0, Status::Discharging),
        ]));
        assert_eq!(slept.runs().len(), 2);
    }
}
