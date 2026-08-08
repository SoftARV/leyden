// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! The charge plot: percentage against wall-clock time, coloured by what the
//! battery was doing. Colours come from the Adwaita accent, never hard-coded.
//!
//! `draw` takes a resolved `Palette` and touches no widget, so the drawing can
//! be rendered to an image surface and asserted on without GTK running — see the
//! tests. Everything that needs the live style manager happens in `draw_func`.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use relm4::adw;
use relm4::adw::prelude::*;
use relm4::gtk;
use relm4::gtk::cairo;
use relm4::gtk::gdk;
use relm4::gtk::glib;

use crate::battery::history::{Follows, History, elapsed_secs};
use crate::battery::types::Status;
use crate::format;

const PAD: f64 = 14.0;
const LINE_WIDTH: f64 = 2.0;
const FILL_ALPHA: f64 = 0.14;
const GRID_ALPHA: f64 = 0.10;

/// Hour marks closer together than this are noise, so the interval steps up
/// until they are at least this far apart. A day across ~490px would otherwise
/// be a rule every 20px.
const MIN_MARK_GAP: f64 = 60.0;
const MARK_HOURS: [i64; 6] = [1, 2, 3, 6, 12, 24];

/// The readout holds a line of height even with nothing to say, so the panel
/// does not jump as the pointer enters and leaves the plot.
pub const IDLE_READOUT: &str = " ";

/// Dash for a suspend: long enough to read as deliberate rather than as a
/// rendering artefact.
const ASLEEP_DASH: [f64; 2] = [5.0, 5.0];

/// Dot for a stretch nothing observed — finer and fainter, so an explained gap
/// looks more solid than an unexplained one.
const UNKNOWN_DASH: [f64; 2] = [1.5, 3.5];

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

/// One plotted sample: `at` is seconds since the first one. Carries the status
/// and power too, because the hover has to describe the point, not just place
/// it.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Point {
    at: f64,
    percent: f64,
    status: Status,
    power: Option<f64>,
}

/// Why the record stops for a stretch. Each is drawn differently, so the reason
/// is visible without hovering: what is known reads more solidly than what is
/// merely bounded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GapKind {
    /// The machine was suspended — the reading on the far side says so.
    Asleep,
    /// The app was not running. The charge still moved, but nothing here knows
    /// whether the machine was awake, asleep, or both.
    NotRunning,
    /// Neither marker is present: an older file, or a poll that simply stalled.
    Unexplained,
}

impl GapKind {
    fn of(follows: Follows) -> Self {
        match follows {
            Follows::Sleep => GapKind::Asleep,
            Follows::Launch => GapKind::NotRunning,
            Follows::Poll => GapKind::Unexplained,
        }
    }

    fn label(self) -> &'static str {
        match self {
            GapKind::Asleep => "Asleep",
            GapKind::NotRunning => "App closed",
            GapKind::Unexplained => "Not recorded",
        }
    }

    /// A suspend is explained, so it is drawn as a plain dash. The other two are
    /// holes in the record and fade to a finer, fainter dot.
    fn dash(self) -> (&'static [f64], f64) {
        match self {
            GapKind::Asleep => (&ASLEEP_DASH, 0.55),
            _ => (&UNKNOWN_DASH, 0.30),
        }
    }
}

/// A stretch with no samples, bridged on the plot rather than left as a hole.
#[derive(Clone, Copy, Debug)]
struct Gap {
    from: Point,
    to: Point,
    kind: GapKind,
}

impl Gap {
    /// Duration and, when the charge actually moved, what it cost and how fast.
    /// The rate is the interesting part: a stretch at 1.4%/h was a sleeping
    /// laptop, one at 3%/h was a laptop awake and in use.
    fn describe(&self) -> String {
        let elapsed = Duration::from_secs_f64((self.to.at - self.from.at).max(0.0));
        let used = self.from.percent - self.to.percent;
        let mut text = format!("{} · {}", self.kind.label(), format::duration(elapsed));
        if used > 0.5 {
            text.push_str(&format!(" · {}% used", used.round() as i64));
            let hours = elapsed.as_secs_f64() / 3600.0;
            if hours >= 0.5 {
                text.push_str(&format!(" ({:.1}%/h)", used / hours));
            }
        }
        text
    }
}

struct Segment {
    trend: Trend,
    /// Whether a real gap precedes this segment, as opposed to just a change of
    /// trend. Only a gap breaks the filled area.
    broke: bool,
    points: Vec<Point>,
}

/// A drawable snapshot of the history.
#[derive(Default)]
pub struct Series {
    segments: Vec<Segment>,
    gaps: Vec<Gap>,
    /// Wall clock of the first sample, so hour marks can land on real hours.
    start: Option<SystemTime>,
    span: f64,
    twelve_hour: bool,
}

impl Series {
    /// `gap_secs` is how far apart two samples must be before the line breaks
    /// rather than inventing a slope across them. It belongs to the caller
    /// because it is only meaningful against the ring's own recording cadence:
    /// a threshold below that cadence makes every pair look like a gap.
    pub fn from_history(history: &History, gap_secs: f64, twelve_hour: bool) -> Self {
        if history.is_empty() {
            return Series {
                twelve_hour,
                ..Series::default()
            };
        }
        let start = history.iter().next().map(|sample| sample.at);
        let mut series = Series {
            span: history.span_secs(),
            start,
            twelve_hour,
            ..Series::default()
        };
        let Some(start) = start else {
            return series;
        };

        let mut previous: Option<(Point, Trend)> = None;
        for sample in history.iter() {
            let point = Point {
                at: elapsed_secs(start, sample.at),
                percent: sample.percent,
                status: sample.status,
                power: sample.power,
            };
            let trend = Trend::of(sample.status);
            let gap = previous.is_some_and(|(last, _)| point.at - last.at > gap_secs);
            let changed = previous.is_some_and(|(_, last)| last != trend);

            if let Some((last, _)) = previous
                && gap
            {
                series.gaps.push(Gap {
                    from: last,
                    to: point,
                    kind: GapKind::of(sample.follows),
                });
            }

            if previous.is_none() || gap || changed {
                let mut points = Vec::new();
                // A colour change with no gap carries the previous point over, so
                // the line stays unbroken where only the trend changed.
                if changed
                    && !gap
                    && let Some((last, _)) = previous
                {
                    points.push(last);
                }
                series.segments.push(Segment {
                    trend,
                    broke: gap || previous.is_none(),
                    points,
                });
            }

            if let Some(segment) = series.segments.last_mut() {
                segment.points.push(point);
            }
            previous = Some((point, trend));
        }
        series
    }

    /// Offsets, in seconds from the first sample, of the local hour boundaries
    /// worth marking. The interval steps up until the marks are far enough apart
    /// to read; a short history gets none at all.
    fn hour_marks(&self, plot_w: f64) -> Vec<f64> {
        let (Some(start), true) = (self.start, self.span > 0.0 && plot_w > 0.0) else {
            return Vec::new();
        };
        let Some(step) = MARK_HOURS
            .iter()
            .map(|hours| *hours as f64 * 3600.0)
            .find(|step| step / self.span * plot_w >= MIN_MARK_GAP)
        else {
            return Vec::new();
        };

        let Ok(unix) = start.duration_since(UNIX_EPOCH) else {
            return Vec::new();
        };
        let Ok(local) = glib::DateTime::from_unix_local(unix.as_secs() as i64) else {
            return Vec::new();
        };
        // Align to the local clock rather than to the first sample, so marks land
        // on 14:00 rather than 14:37.
        let since_midnight =
            f64::from(local.hour()) * 3600.0 + f64::from(local.minute()) * 60.0 + local.seconds();
        let first = (step - since_midnight % step) % step;

        // A rule flush against either edge of the plot reads as a border, not
        // as a time mark, so both ends are skipped.
        let mut marks = Vec::new();
        let mut at = if first <= 0.0 { step } else { first };
        while at < self.span {
            marks.push(at);
            at += step;
        }
        marks
    }

    /// What the pointer is over, as tooltip text. `None` when there is nothing
    /// to say.
    fn describe(&self, x: f64, width: f64) -> Option<String> {
        let plot_w = width - PAD * 2.0;
        if self.span <= 0.0 || plot_w <= 0.0 {
            return None;
        }
        let at = ((x - PAD) / plot_w * self.span).clamp(0.0, self.span);

        if let Some(gap) = self
            .gaps
            .iter()
            .find(|gap| at > gap.from.at && at < gap.to.at)
        {
            return Some(gap.describe());
        }

        let point = self
            .segments
            .iter()
            .flat_map(|segment| segment.points.iter())
            .min_by(|a, b| (a.at - at).abs().total_cmp(&(b.at - at).abs()))?;
        Some(self.describe_point(point))
    }

    fn describe_point(&self, point: &Point) -> String {
        let mut text = format!(
            "{} · {}%",
            self.clock(point.at),
            point.percent.round() as i64
        );
        match point.status {
            Status::Charging => text.push_str(" · charging"),
            Status::Discharging => text.push_str(" · discharging"),
            Status::Full => text.push_str(" · full"),
            Status::NotCharging => text.push_str(" · not charging"),
            Status::Unknown => {}
        }
        if let Some(power) = point.power {
            text.push_str(&format!(" · {power:.1} W"));
        }
        text
    }

    /// Local wall clock for an offset into the series.
    fn clock(&self, at: f64) -> String {
        let Some(start) = self.start else {
            return String::new();
        };
        let Ok(unix) = start.duration_since(UNIX_EPOCH) else {
            return String::new();
        };
        format::time(unix.as_secs() as i64 + at as i64, self.twelve_hour)
    }

    /// Segments merged back together across trend changes, so the filled area
    /// is continuous and only a real gap splits it. Without this a two-sample
    /// discharge blip while charging fills as a solid vertical bar.
    fn runs(&self) -> Vec<Vec<Point>> {
        let mut runs: Vec<Vec<Point>> = Vec::new();
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

    fn points(&self) -> usize {
        self.segments.iter().map(|s| s.points.len()).sum()
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
        for at in self.hour_marks(plot_w) {
            let at_x = PAD + at / self.span * plot_w;
            cr.move_to(at_x, PAD);
            cr.line_to(at_x, height - PAD);
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
            cr.move_to(x(first.at), y(first.percent));
            for point in rest {
                cr.line_to(x(point.at), y(point.percent));
            }
            cr.line_to(x(last.at), height - PAD);
            cr.line_to(x(first.at), height - PAD);
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
            cr.move_to(x(first.at), y(first.percent));
            for point in rest {
                cr.line_to(x(point.at), y(point.percent));
            }
            palette.trend(segment.trend).apply(cr);
            cr.stroke().ok();
        }

        // Bridge each gap with a dashed grey line and no fill: the charge did
        // change across it, but nothing here was measured, and a solid line
        // would claim otherwise.
        for gap in &self.gaps {
            let (dash, alpha) = gap.kind.dash();
            cr.set_dash(dash, 0.0);
            palette.idle.with_alpha(alpha).apply(cr);
            cr.move_to(x(gap.from.at), y(gap.from.percent));
            cr.line_to(x(gap.to.at), y(gap.to.percent));
            cr.stroke().ok();
        }
        cr.set_dash(&[], 0.0);

        if let Some(segment) = self.segments.last()
            && let Some(last) = segment.points.last()
        {
            palette.trend(segment.trend).apply(cr);
            cr.arc(x(last.at), y(last.percent), 3.5, 0.0, std::f64::consts::TAU);
            cr.fill().ok();
        }
    }
}

/// The snapshot the widget callbacks read.
///
/// The draw closure is rebuilt on every `#[watch]` pass and could own its copy,
/// but the tooltip handler is connected once in `init` and must answer
/// synchronously from whatever is current. Sharing one cell between them is the
/// narrow case where interior mutability earns its keep: the state still lives
/// in the model, and nothing mutates it except the view pass.
#[derive(Clone, Default)]
pub struct Plot(Rc<RefCell<Rc<Series>>>);

impl Plot {
    /// Store `series` as the current snapshot and hand back a draw closure that
    /// reads it. One call so the two cannot drift apart.
    pub fn refreshed(
        &self,
        series: Series,
    ) -> impl Fn(&gtk::DrawingArea, &cairo::Context, i32, i32) + 'static {
        *self.0.borrow_mut() = Rc::new(series);
        let shared = Rc::clone(&self.0);
        move |area, cr, width, height| {
            let series = Rc::clone(&shared.borrow());
            series.draw(cr, width, height, Palette::of(area));
        }
    }

    /// Point `readout` at the plot, so moving across it describes the sample
    /// under the pointer straight away.
    ///
    /// Deliberately **not** a tooltip: GTK only raises one once the pointer has
    /// come to rest, so dragging along a chart — the natural way to read one —
    /// never shows anything. A label tracks motion instead.
    ///
    /// Connected once, from `init`. Reconnecting on every view pass would stack
    /// handlers, and the label is left out of `#[watch]` so the two cannot
    /// fight over its text.
    pub fn install_readout(&self, area: &gtk::DrawingArea, readout: &gtk::Label) {
        let motion = gtk::EventControllerMotion::new();

        let shared = Rc::clone(&self.0);
        let label = readout.clone();
        let area_width = area.clone();
        motion.connect_motion(move |_, x, _| {
            let series = Rc::clone(&shared.borrow());
            let width = f64::from(area_width.width());
            label.set_label(
                &series
                    .describe(x, width)
                    .unwrap_or_else(|| IDLE_READOUT.to_owned()),
            );
        });

        let label = readout.clone();
        motion.connect_leave(move |_| label.set_label(IDLE_READOUT));

        area.add_controller(motion);
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
        let empty = Series::from_history(&History::new(4), 90.0, false);
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
        let grid = painted_pixels(&Series::from_history(&History::new(4), 90.0, false));
        let drawn = painted_pixels(&Series::from_history(&steady(30, 20), 90.0, false));
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
        let grid = painted_pixels(&Series::from_history(&History::new(4), 90.0, false));
        let recorded = painted_pixels(&Series::from_history(
            &steady(30, 20),
            RECORD_GAP_TEST,
            false,
        ));
        assert!(
            recorded > grid * 3,
            "a 30s cadence must draw: {recorded} vs {grid} for the bare grid"
        );
    }

    #[test]
    fn the_line_itself_is_stroked_not_just_filled() {
        let pixels = render(&Series::from_history(
            &steady(30, 20),
            RECORD_GAP_TEST,
            false,
        ));
        let stroke = stroked(&pixels);
        assert!(
            stroke > 100,
            "the trend line must reach the canvas, not only its fill: {stroke} opaque pixels"
        );
    }

    /// Same as `history`, but the last sample is marked as the first reading
    /// after a wake.
    fn history_after_sleep(entries: &[(u64, f64, Status)]) -> History {
        let mut built = History::new(100);
        let last = entries.len().saturating_sub(1);
        for (index, (secs, percent, status)) in entries.iter().enumerate() {
            built.push(Sample {
                at: SystemTime::UNIX_EPOCH + Duration::from_secs(*secs),
                percent: *percent,
                power: None,
                status: *status,
                follows: if index == last {
                    Follows::Sleep
                } else {
                    Follows::Poll
                },
            });
        }
        built
    }

    #[test]
    fn a_suspend_is_told_apart_from_a_stretch_nobody_recorded() {
        let entries = [
            (0, 80.0, Status::Discharging),
            (30, 79.0, Status::Discharging),
            (30 + 8 * 3600, 68.0, Status::Discharging),
        ];

        let unrecorded = Series::from_history(&history(&entries), RECORD_GAP_TEST, false);
        assert_eq!(unrecorded.gaps[0].kind, GapKind::Unexplained);
        assert!(
            unrecorded.gaps[0].describe().starts_with("Not recorded"),
            "{}",
            unrecorded.gaps[0].describe()
        );

        let slept = Series::from_history(&history_after_sleep(&entries), RECORD_GAP_TEST, false);
        assert_eq!(slept.gaps[0].kind, GapKind::Asleep);
        let text = slept.gaps[0].describe();
        // The same stretch, now with what it cost and the rate it cost it at.
        assert!(text.starts_with("Asleep"), "{text}");
        assert!(text.contains("8 h"), "{text}");
        assert!(text.contains("11% used"), "{text}");
        assert!(text.contains("%/h"), "{text}");
    }

    #[test]
    fn a_closed_app_is_told_apart_from_a_suspend() {
        // Same stretch, same charge lost — but one was a sleeping laptop and the
        // other a laptop that may well have been awake and in use.
        let mut history = History::new(100);
        for (secs, percent, follows) in [
            (0u64, 80.0, Follows::Poll),
            (30, 79.0, Follows::Poll),
            (30 + 2 * 3600, 73.0, Follows::Launch),
        ] {
            history.push(Sample {
                at: SystemTime::UNIX_EPOCH + Duration::from_secs(secs),
                percent,
                power: None,
                status: Status::Discharging,
                follows,
            });
        }
        let series = Series::from_history(&history, RECORD_GAP_TEST, false);
        assert_eq!(series.gaps[0].kind, GapKind::NotRunning);
        let text = series.gaps[0].describe();
        assert!(text.starts_with("App closed"), "{text}");
        assert!(text.contains("6% used"), "{text}");
        assert!(text.contains("3.0%/h"), "{text}");
        // An explained gap is drawn more solidly than an unexplained one.
        assert!(GapKind::Asleep.dash().1 > GapKind::NotRunning.dash().1);
    }

    #[test]
    fn a_gap_is_recorded_with_both_of_its_ends() {
        let series = Series::from_history(
            &history(&[
                (0, 80.0, Status::Discharging),
                (30, 79.0, Status::Discharging),
                (7200, 40.0, Status::Discharging),
            ]),
            RECORD_GAP_TEST,
            false,
        );
        assert_eq!(series.gaps.len(), 1);
        let gap = series.gaps[0];
        assert_eq!(gap.from.percent, 79.0);
        assert_eq!(gap.to.percent, 40.0);
        assert_eq!(gap.kind, GapKind::Unexplained);
        // 39 points of charge went somewhere unobserved; the hover says so.
        assert!(gap.describe().contains("39% used"), "{}", gap.describe());
    }

    #[test]
    fn hour_marks_thin_out_as_the_span_grows() {
        // An hour across a 200px plot is comfortable; a day is not, so the
        // interval has to step up rather than draw 24 rules.
        let hour = Series::from_history(&steady(30, 120), RECORD_GAP_TEST, false);
        let day = Series::from_history(&steady(30, 2880), RECORD_GAP_TEST, false);
        assert!(
            day.hour_marks(200.0).len() < 12,
            "a day should not draw a rule an hour: {}",
            day.hour_marks(200.0).len()
        );
        for (series, marks) in [
            (&hour, hour.hour_marks(200.0)),
            (&day, day.hour_marks(200.0)),
        ] {
            for at in marks {
                assert!(at > 0.0 && at < series.span, "mark at {at} is on an edge");
            }
        }
    }

    #[test]
    fn a_short_history_gets_no_hour_marks() {
        let minutes = Series::from_history(&steady(30, 8), RECORD_GAP_TEST, false);
        assert!(minutes.hour_marks(200.0).is_empty());
    }

    #[test]
    fn hovering_describes_the_nearest_point() {
        let series = Series::from_history(&steady(30, 20), RECORD_GAP_TEST, false);
        let text = series
            .describe(f64::from(WIDTH) / 2.0, f64::from(WIDTH))
            .unwrap();
        assert!(text.contains('%'), "{text}");
        assert!(text.contains("discharging"), "{text}");
    }

    #[test]
    fn hovering_over_a_gap_says_it_was_not_recorded() {
        let series = Series::from_history(
            &history(&[
                (0, 80.0, Status::Discharging),
                (30, 79.0, Status::Discharging),
                (7200, 40.0, Status::Discharging),
            ]),
            RECORD_GAP_TEST,
            false,
        );
        // Halfway across is deep inside the gap.
        let text = series
            .describe(f64::from(WIDTH) / 2.0, f64::from(WIDTH))
            .unwrap();
        assert!(text.starts_with("Not recorded"), "{text}");
    }

    #[test]
    fn a_bridged_gap_still_leaves_the_line_solid_elsewhere() {
        // #7 showed up as nothing drawn; with bridging it would show up as
        // everything dashed, so assert the solid stroke survives.
        //
        // The recorded stretches have to be a real share of the span for this to
        // mean anything: two samples at the edges of a two-hour plot are a
        // couple of pixels wide, and an earlier version of this test passed only
        // because the gap bridge was itself opaque.
        let mut rows: Vec<(u64, f64, Status)> = (0..40)
            .map(|step| (step * 30, 80.0 - step as f64 * 0.1, Status::Discharging))
            .collect();
        let resume = 40 * 30 + 2 * 3600;
        rows.extend((0..40).map(|step| {
            (
                resume + step * 30,
                68.0 - step as f64 * 0.1,
                Status::Discharging,
            )
        }));

        let gapped = Series::from_history(&history(&rows), RECORD_GAP_TEST, false);
        assert_eq!(gapped.gaps.len(), 1, "one gap, in the middle");
        let stroke = stroked(&render(&gapped));
        assert!(stroke > 100, "solid line pixels expected, got {stroke}");
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
                follows: Follows::Poll,
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
            false,
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
            false,
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
            false,
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
            false,
        );
        assert_eq!(series.segments.len(), 2);
        // The charging segment starts at the last discharging point, so the two
        // meet instead of leaving a hole.
        assert_eq!(series.segments[1].points.len(), 2);
        assert_eq!(series.segments[1].points[0].at, 2.0);
        assert_eq!(series.segments[1].points[0].percent, 79.0);
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
            false,
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
            false,
        );
        assert_eq!(slept.runs().len(), 2);
    }
}
