// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! The charge plot: percentage against wall-clock time, coloured by what the
//! battery was doing. Colours come from the Adwaita accent, never hard-coded.
//!
//! `draw` takes a resolved `Palette` and touches no widget, so the drawing can
//! be rendered to an image surface and asserted on without GTK running — see the
//! tests. Everything that needs the live style manager happens in `draw_func`.

use relm4::adw;
use relm4::adw::prelude::*;
use relm4::gtk;
use relm4::gtk::cairo;
use relm4::gtk::gdk;

use crate::battery::history::{History, elapsed_secs};
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
}

/// A colour in the form cairo wants, so the drawing never handles a gdk type.
#[derive(Clone, Copy)]
pub struct Rgba {
    red: f64,
    green: f64,
    blue: f64,
    alpha: f64,
}

impl Rgba {
    fn of(color: gdk::RGBA) -> Self {
        Self {
            red: f64::from(color.red()),
            green: f64::from(color.green()),
            blue: f64::from(color.blue()),
            alpha: f64::from(color.alpha()),
        }
    }

    fn with_alpha(self, alpha: f64) -> Self {
        Self { alpha, ..self }
    }

    fn apply(self, cr: &cairo::Context) {
        cr.set_source_rgba(self.red, self.green, self.blue, self.alpha);
    }
}

/// Every colour the plot needs, resolved before drawing starts. This is the
/// whole of the plot's dependency on a running GTK, and pulling it out is what
/// makes the drawing testable.
#[derive(Clone, Copy)]
pub struct Palette {
    charging: Rgba,
    discharging: Rgba,
    idle: Rgba,
    grid: Rgba,
}

impl Palette {
    fn of(area: &gtk::DrawingArea) -> Self {
        let manager = adw::StyleManager::default();
        let foreground = Rgba::of(area.color());
        Self {
            charging: Rgba::of(adw::AccentColor::Green.to_standalone_rgba(manager.is_dark())),
            discharging: Rgba::of(manager.accent_color_rgba()),
            idle: foreground.with_alpha(0.45),
            grid: foreground.with_alpha(GRID_ALPHA),
        }
    }

    fn trend(self, trend: Trend) -> Rgba {
        match trend {
            Trend::Charging => self.charging,
            Trend::Discharging => self.discharging,
            Trend::Idle => self.idle,
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
    /// `gap_secs` is how far apart two samples must be before the line breaks
    /// rather than inventing a slope across them. It belongs to the caller
    /// because it is only meaningful against the ring's own recording cadence:
    /// a threshold below that cadence makes every pair look like a gap.
    pub fn from_history(history: &History, gap_secs: f64) -> Self {
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
            let gap = previous.is_some_and(|(last, ..)| at - last > gap_secs);
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
        move |area, cr, width, height| self.draw(cr, width, height, Palette::of(area))
    }

    fn draw(&self, cr: &cairo::Context, width: i32, height: i32, palette: Palette) {
        let (width, height) = (f64::from(width), f64::from(height));
        let (plot_w, plot_h) = (width - PAD * 2.0, height - PAD * 2.0);
        if plot_w <= 0.0 || plot_h <= 0.0 {
            return;
        }

        let y = |percent: f64| PAD + (1.0 - percent / 100.0) * plot_h;

        cr.set_line_width(1.0);
        palette.grid.apply(cr);
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
        palette.trend(current).with_alpha(FILL_ALPHA).apply(cr);
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
            cr.move_to(x(first.0), y(first.1));
            for point in rest {
                cr.line_to(x(point.0), y(point.1));
            }
            palette.trend(segment.trend).apply(cr);
            cr.stroke().ok();
        }

        if let Some(segment) = self.segments.last()
            && let Some(last) = segment.points.last()
        {
            palette.trend(segment.trend).apply(cr);
            cr.arc(x(last.0), y(last.1), 3.5, 0.0, std::f64::consts::TAU);
            cr.fill().ok();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WIDTH: i32 = 240;
    const HEIGHT: i32 = 120;

    /// Flat, fully opaque colours — the test cares that pixels land, not which
    /// shade the accent happens to be today.
    fn palette() -> Palette {
        let solid = |red, green, blue| Rgba {
            red,
            green,
            blue,
            alpha: 1.0,
        };
        Palette {
            charging: solid(0.0, 1.0, 0.0),
            discharging: solid(0.0, 0.0, 1.0),
            idle: solid(0.5, 0.5, 0.5),
            grid: solid(1.0, 1.0, 1.0).with_alpha(GRID_ALPHA),
        }
    }

    /// Renders to an image surface. Needs no GTK, no window and no main loop —
    /// only cairo, which is why `draw` takes a resolved `Palette` rather than a
    /// widget.
    fn render(series: &Series) -> Vec<u8> {
        let mut surface = cairo::ImageSurface::create(cairo::Format::ARgb32, WIDTH, HEIGHT)
            .expect("image surface");
        {
            let cr = cairo::Context::new(&surface).expect("cairo context");
            series.draw(&cr, WIDTH, HEIGHT, palette());
        }
        surface.data().expect("surface data").to_vec()
    }

    /// Any paint at all: grid, fill and line together.
    fn painted(pixels: &[u8]) -> usize {
        pixels.chunks_exact(4).filter(|pixel| pixel[3] != 0).count()
    }

    /// Nearly opaque paint only — the stroke and the trailing dot. The fill goes
    /// down at `FILL_ALPHA` and the grid at `GRID_ALPHA`, so neither survives
    /// this cut. Without it, deleting every stroke still passes on the strength
    /// of the fill alone.
    fn stroked(pixels: &[u8]) -> usize {
        pixels
            .chunks_exact(4)
            .filter(|pixel| pixel[3] > 200)
            .count()
    }

    fn painted_pixels(series: &Series) -> usize {
        painted(&render(series))
    }

    fn steady(cadence: u64, count: u64) -> History {
        let rows: Vec<(u64, f64, Status)> = (0..count)
            .map(|step| (step * cadence, 80.0 - step as f64, Status::Discharging))
            .collect();
        history(&rows)
    }

    #[test]
    fn an_empty_history_paints_only_the_grid() {
        let empty = Series::from_history(&History::new(4), 90.0);
        let painted = painted_pixels(&empty);
        // Five horizontal rules and nothing else.
        assert!(painted > 0, "the grid should still be drawn");
        assert!(
            painted < 3_000,
            "no data must mean no line and no fill, got {painted} pixels"
        );
    }

    #[test]
    fn a_series_paints_far_more_than_the_grid() {
        let grid = painted_pixels(&Series::from_history(&History::new(4), 90.0));
        let drawn = painted_pixels(&Series::from_history(&steady(30, 20), 90.0));
        assert!(
            drawn > grid * 3,
            "a real series should paint a line and a fill: {drawn} vs {grid} for the bare grid"
        );
    }

    #[test]
    fn the_recorded_cadence_actually_reaches_the_canvas() {
        // The #7 regression, caught at the pixels rather than at the geometry:
        // a history recorded every 30s once produced only single-point segments,
        // so every stroke and every fill was skipped and this painted the grid.
        let grid = painted_pixels(&Series::from_history(&History::new(4), 90.0));
        let recorded = painted_pixels(&Series::from_history(&steady(30, 20), RECORD_GAP_TEST));
        assert!(
            recorded > grid * 3,
            "a 30s cadence must draw: {recorded} vs {grid} for the bare grid"
        );
    }

    #[test]
    fn the_line_itself_is_stroked_not_just_filled() {
        let pixels = render(&Series::from_history(&steady(30, 20), RECORD_GAP_TEST));
        let stroke = stroked(&pixels);
        assert!(
            stroke > 100,
            "the trend line must reach the canvas, not only its fill: {stroke} opaque pixels"
        );
    }

    /// The threshold `app.rs` passes for the recorded ring (`RECORD_SECS * 3`).
    const RECORD_GAP_TEST: f64 = 90.0;

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
    fn the_recorded_cadence_still_draws_one_line() {
        // History is recorded every RECORD_SECS (30s). If the gap threshold is
        // smaller than that, every consecutive pair reads as a gap and the graph
        // degenerates into isolated single points that draw nothing at all.
        let series = Series::from_history(
            &history(&[
                (0, 80.0, Status::Discharging),
                (30, 79.0, Status::Discharging),
                (60, 78.0, Status::Discharging),
                (90, 77.0, Status::Discharging),
            ]),
            90.0,
        );
        assert_eq!(
            series.segments.len(),
            1,
            "a steady 30s cadence must be one segment"
        );
        assert_eq!(series.runs().len(), 1, "and one continuous filled run");
    }

    #[test]
    fn a_steady_discharge_is_one_segment() {
        let series = Series::from_history(
            &history(&[
                (0, 80.0, Status::Discharging),
                (2, 79.0, Status::Discharging),
                (4, 78.0, Status::Discharging),
            ]),
            15.0,
        );
        assert_eq!(series.segments.len(), 1);
        assert_eq!(series.points(), 3);
    }

    #[test]
    fn a_suspend_breaks_the_line() {
        let series = Series::from_history(
            &history(&[
                (0, 80.0, Status::Discharging),
                (2, 79.0, Status::Discharging),
                (7200, 40.0, Status::Discharging),
            ]),
            15.0,
        );
        assert_eq!(series.segments.len(), 2);
        assert_eq!(series.segments[1].points.len(), 1);
    }

    #[test]
    fn plugging_in_changes_colour_without_breaking_the_line() {
        let series = Series::from_history(
            &history(&[
                (0, 80.0, Status::Discharging),
                (2, 79.0, Status::Discharging),
                (4, 79.0, Status::Charging),
            ]),
            15.0,
        );
        assert_eq!(series.segments.len(), 2);
        // The charging segment starts at the last discharging point, so the two
        // meet instead of leaving a hole.
        assert_eq!(series.segments[1].points.len(), 2);
        assert_eq!(series.segments[1].points[0], (2.0, 79.0));
    }

    #[test]
    fn only_a_gap_splits_the_filled_area() {
        let plugged_in = Series::from_history(
            &history(&[
                (0, 80.0, Status::Discharging),
                (2, 79.0, Status::Discharging),
                (4, 79.0, Status::Charging),
            ]),
            15.0,
        );
        assert_eq!(plugged_in.segments.len(), 2);
        assert_eq!(plugged_in.runs().len(), 1);
        assert_eq!(plugged_in.runs()[0].len(), 3);

        let slept = Series::from_history(
            &history(&[
                (0, 80.0, Status::Discharging),
                (2, 79.0, Status::Discharging),
                (7200, 40.0, Status::Discharging),
            ]),
            15.0,
        );
        assert_eq!(slept.runs().len(), 2);
    }
}
