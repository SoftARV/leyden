// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! Root component.
//!
//! Elm/Redux shape: `AppMsg` in, one `update` reducer, view derived from
//! `AppModel`. sysfs reads are plain local file reads (microseconds), so unlike
//! the network-bound siblings they happen inline in the reducer rather than in a
//! relm4 command.

use std::time::Duration;

use relm4::actions::{AccelsPlus, RelmAction, RelmActionGroup};
use relm4::adw::prelude::*;
use relm4::gtk::{gio, glib};
use relm4::{Component, ComponentParts, ComponentSender, RelmWidgetExt, adw, gtk};

use crate::battery::history::{self, Follows, History, Sample};
use crate::battery::sysfs;
use crate::battery::types::{Battery, Status};
use crate::format;
use crate::graph::{IDLE_READOUT, Plot, Series};
use crate::notify::{self, Alerts};
use crate::recordings::{self, Kind, Run};
use crate::settings::{Clock, MAX_POLL_SECS, MIN_POLL_SECS, Settings, Theme};
use crate::sleep::{self, Clocks};
use crate::store;

/// While nothing is on screen the app records but does not display, so it polls
/// at exactly the recording cadence: a complete history for a fifteenth of the
/// wakeups. This is the letter of hard rule 4 changing while its spirit — never
/// be a cause of the drain you measure — stays.
const HIDDEN_POLL_SECS: u32 = 30;

/// One graph sample per this many seconds, whatever the poll rate. A day at this
/// cadence is ~2 880 points — more than the plot has pixels, and small enough
/// that the history file stays around 100 KB.
const RECORD_SECS: f64 = HIDDEN_POLL_SECS as f64;

/// Room for a day of `RECORD_SECS` samples plus the extra ones a busy day of
/// state changes adds. Age, not this, is what actually bounds the history.
const HISTORY_CAP: usize = 4096;

/// The live readings kept for smoothing only — `SMOOTH_SECS` at `POLL_SECS`,
/// with room to spare. Never persisted.
const POWER_CAP: usize = 128;

/// Power is averaged over this trailing window before it drives an estimate. A
/// single reading swings with whatever the CPU is doing that instant, which made
/// "time left" jump by tens of minutes between polls.
const SMOOTH_SECS: f64 = 120.0;

/// How far apart two samples must be before they stop counting as neighbours —
/// a suspend, or a window that was hidden with the timer off. There is one per
/// ring because the threshold only means anything against that ring's own
/// cadence: below it, every consecutive pair reads as a gap.
/// The recorded ring's threshold is a constant because its cadence is; the
/// poll ring's follows the configured interval, so it lives in `poll_gap_secs`.
const RECORD_GAP_SECS: f64 = RECORD_SECS * 3.0;

// A second, application-level group. The notification that explains a
// windowless Leyden has to act on something reachable without a window, and
// `win.*` actions are not that.
relm4::new_action_group!(AppLevelGroup, "app");
relm4::new_stateless_action!(ShowAction, AppLevelGroup, "show");
relm4::new_stateless_action!(QuitAppAction, AppLevelGroup, "quit");

relm4::new_action_group!(AppActionGroup, "win");
relm4::new_stateless_action!(RecordingsAction, AppActionGroup, "recordings");
relm4::new_stateless_action!(PreferencesAction, AppActionGroup, "preferences");
relm4::new_stateless_action!(ShortcutsAction, AppActionGroup, "shortcuts");
relm4::new_stateless_action!(AboutAction, AppActionGroup, "about");
relm4::new_stateless_action!(QuitAction, AppActionGroup, "quit");

pub struct AppModel {
    battery: Option<Battery>,
    history: History,
    power_window: History,
    poll: Option<glib::SourceId>,
    /// Nothing of the window is on screen: minimised, occluded, on another
    /// workspace, or closed while running in the background.
    hidden: bool,
    settings: Settings,
    alerts: Alerts,
    /// Completed runs, from disk and from whatever history is loaded.
    recordings: Vec<Run>,
    /// The status at the previous sample; a change is when a run can end.
    last_status: Option<Status>,
    clocks: Clocks,
    /// What the next sample should record as preceding it. Set to `Launch` at
    /// startup so the stretch since the last session is named, and to `Sleep`
    /// when a suspend is noticed; consumed by the next sample.
    next_follows: Follows,
    /// Held for the life of the process; dropping it unsubscribes from logind.
    sleep_watch: Option<sleep::Watcher>,
    /// Shared with the graph's draw and hover callbacks — see `graph::Plot`.
    plot: Plot,
}

#[derive(Debug)]
pub enum AppMsg {
    Tick,
    /// The window's visibility or occlusion changed; the reducer reads the real
    /// state off the root rather than trusting a single signal.
    PresenceChanged,
    /// The user asked to close the window. Whether that quits depends on the
    /// background preference.
    CloseRequested,
    ShowAbout,
    ShowPreferences,
    ShowShortcuts,
    SetPollSecs(u32),
    SetTheme(Theme),
    SetAlerts(bool),
    SetClock(Clock),
    SetBackground(bool),
    /// logind's `PrepareForSleep`: `true` just before the freeze, `false` on
    /// resume.
    SleepSignal(bool),
    ShowRecordings,
    /// Leave, but write the last reading first.
    Quit,
}

/// Results from off-thread work. Disk I/O never happens in `update` (rule 3);
/// only sysfs earns that exception.
#[derive(Debug)]
pub enum CommandMsg {
    Loaded(Vec<Sample>),
    Written(Result<(), String>),
    Saved(Result<(), String>),
    Recorded(Result<(), String>),
}

impl AppModel {
    fn sample(&mut self, sender: &ComponentSender<Self>) {
        self.battery = sysfs::read();
        let Some(battery) = &self.battery else {
            return;
        };
        let sample = Sample {
            at: history::now(),
            percent: battery.percent,
            power: battery.power,
            status: battery.status,
            follows: std::mem::take(&mut self.next_follows),
        };

        // Always advanced, even with alerts switched off, so turning them on
        // does not immediately fire for a threshold crossed long ago.
        if let Some(alert) = self.alerts.advance(battery.percent, battery.status)
            && self.settings.alerts
        {
            notify::send(alert, battery.percent);
        }

        // A run can only end where the trend changes, so that is the only time
        // the (linear) derivation is worth re-running.
        if self.last_status.replace(battery.status) != Some(battery.status) {
            self.refresh_recordings(sender);
        }

        self.power_window.push(sample);
        if !self.should_record(&sample) {
            return;
        }
        self.history.push(sample);
        self.history.prune_older_than(store::MAX_AGE_SECS);
        sender.oneshot_command(async move {
            CommandMsg::Written(
                relm4::spawn_blocking(move || store::append(&sample))
                    .await
                    .map_err(|error| error.to_string())
                    .and_then(|result| result.map_err(|error| error.to_string())),
            )
        });
    }

    /// Write the newest reading before the process ends.
    ///
    /// Synchronous, unlike every other write: a `Command` dispatched here would
    /// not finish before the main loop stops. Without it the record loses up to
    /// `RECORD_SECS`, and the gap that follows starts in the wrong place.
    fn flush(&mut self) {
        let Some(battery) = &self.battery else {
            return;
        };
        let sample = Sample {
            at: history::now(),
            percent: battery.percent,
            power: battery.power,
            status: battery.status,
            follows: Follows::Poll,
        };
        // The cadence may already have written this second.
        if self
            .history
            .newest()
            .is_some_and(|newest| newest.at >= sample.at)
        {
            return;
        }
        if let Err(error) = store::append(&sample) {
            tracing::warn!("could not flush the last reading: {error}");
        }
    }

    /// Re-derive runs from the history and fold them into what is already
    /// known, persisting only when something actually changed.
    fn refresh_recordings(&mut self, sender: &ComponentSender<Self>) {
        if !recordings::merge(&mut self.recordings, recordings::runs_in(&self.history)) {
            return;
        }
        let runs = self.recordings.clone();
        sender.oneshot_command(async move {
            CommandMsg::Recorded(
                relm4::spawn_blocking(move || recordings::save(&runs))
                    .await
                    .map_err(|error| error.to_string())
                    .and_then(|result| result),
            )
        });
    }

    /// A state change is recorded whenever it happens — it is what colours the
    /// graph, and waiting for the cadence would misplace the transition.
    fn should_record(&self, sample: &Sample) -> bool {
        self.history.newest().is_none_or(|last| {
            last.status != sample.status || history::elapsed_secs(last.at, sample.at) >= RECORD_SECS
        })
    }

    fn start_poll(&mut self, sender: &ComponentSender<Self>) {
        if self.poll.is_some() {
            return;
        }
        let input = sender.input_sender().clone();
        self.poll = Some(glib::timeout_add_seconds_local(
            self.poll_interval(),
            move || {
                input.send(AppMsg::Tick).ok();
                glib::ControlFlow::Continue
            },
        ));
    }

    /// The live rate while the window is on screen, the recording rate when it
    /// is not.
    fn poll_interval(&self) -> u32 {
        if self.hidden {
            HIDDEN_POLL_SECS
        } else {
            self.settings.poll_secs
        }
    }

    /// Two samples further apart than this were not consecutive polls. Derived
    /// from whichever interval is in force, never a bare number — a threshold
    /// below the cadence makes every pair look like a gap.
    fn poll_gap_secs(&self) -> f64 {
        f64::from(self.poll_interval()) * 7.0
    }

    /// The interval is baked into the `glib::timeout` when it is created, so a
    /// change only takes effect on a fresh one.
    fn restart_poll(&mut self, sender: &ComponentSender<Self>) {
        self.stop_poll();
        self.start_poll(sender);
    }

    /// The last few runs of each kind, in their own dialog.
    fn recordings_dialog(&self) -> adw::Dialog {
        // Derived fresh, not read straight off the model: `refresh_recordings`
        // only runs where a trend changes, because that is the only moment a run
        // can *complete*. The run in progress moves continuously, so it is
        // recomputed here — once per opening, and without touching the file,
        // since nothing in progress is ever persisted.
        let mut view = self.recordings.clone();
        recordings::merge(&mut view, recordings::runs_in(&self.history));

        let stack = adw::ViewStack::new();
        for (kind, title, icon) in [
            (Kind::Discharge, "Discharges", "battery-level-30-symbolic"),
            (
                Kind::Charge,
                "Charges",
                "battery-level-50-charging-symbolic",
            ),
        ] {
            let page = self.recordings_page(&view, kind, title);
            stack.add_titled_with_icon(&page, Some(kind.as_key()), title, icon);
        }

        let header = adw::HeaderBar::builder()
            .title_widget(
                &adw::ViewSwitcher::builder()
                    .stack(&stack)
                    .policy(adw::ViewSwitcherPolicy::Wide)
                    .build(),
            )
            .build();

        let toolbar = adw::ToolbarView::builder().content(&stack).build();
        toolbar.add_top_bar(&header);

        adw::Dialog::builder()
            .title("Recordings")
            .child(&toolbar)
            .content_width(460)
            .content_height(580)
            .build()
    }

    fn recordings_page(&self, view: &[Run], kind: Kind, title: &str) -> gtk::Widget {
        let runs = recordings::latest(view, kind);
        if runs.is_empty() {
            let empty = adw::StatusPage::builder()
                .icon_name("document-open-recent-symbolic")
                .title(format!("No {} Yet", title.to_lowercase()))
                .description(
                    "Runs appear here once the battery has been on one side of the \
                     line for more than a few minutes.",
                )
                .build();
            return empty.upcast();
        }

        let group = adw::PreferencesGroup::new();
        for run in runs {
            group.add(&recording_row(run, self.settings.clock.twelve_hour()));
        }

        let clamp = adw::Clamp::builder()
            .maximum_size(520)
            .child(&group)
            .margin_top(18)
            .margin_bottom(18)
            .margin_start(12)
            .margin_end(12)
            .build();
        gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&clamp)
            .build()
            .upcast()
    }

    fn preferences_dialog(&self, sender: &ComponentSender<Self>) -> adw::PreferencesDialog {
        let group = adw::PreferencesGroup::builder().title("General").build();

        let poll =
            adw::SpinRow::with_range(f64::from(MIN_POLL_SECS), f64::from(MAX_POLL_SECS), 1.0);
        poll.set_title("Refresh interval");
        poll.set_subtitle("Seconds between battery readings");
        poll.set_value(f64::from(self.settings.poll_secs));
        let poll_sender = sender.input_sender().clone();
        poll.connect_value_notify(move |row| {
            poll_sender
                .send(AppMsg::SetPollSecs(row.value().round() as u32))
                .ok();
        });
        group.add(&poll);

        let theme = adw::ComboRow::builder()
            .title("Appearance")
            .model(&gtk::StringList::new(&["Follow system", "Light", "Dark"]))
            .selected(self.settings.theme.index())
            .build();
        let theme_sender = sender.input_sender().clone();
        theme.connect_selected_notify(move |row| {
            theme_sender
                .send(AppMsg::SetTheme(Theme::from_index(row.selected())))
                .ok();
        });
        group.add(&theme);

        let clock = adw::ComboRow::builder()
            .title("Time format")
            .model(&gtk::StringList::new(&[
                "Follow system",
                "24-hour",
                "12-hour",
            ]))
            .selected(self.settings.clock.index())
            .build();
        let clock_sender = sender.input_sender().clone();
        clock.connect_selected_notify(move |row| {
            clock_sender
                .send(AppMsg::SetClock(Clock::from_index(row.selected())))
                .ok();
        });
        group.add(&clock);

        let background = adw::SwitchRow::builder()
            .title("Keep running in the background")
            .subtitle(
                "Closing the window keeps recording, so a long measurement is not interrupted",
            )
            .active(self.settings.background)
            .build();
        let background_sender = sender.input_sender().clone();
        background.connect_active_notify(move |row| {
            background_sender
                .send(AppMsg::SetBackground(row.is_active()))
                .ok();
        });
        group.add(&background);

        let alerts = adw::SwitchRow::builder()
            .title("Battery alerts")
            .subtitle("Notify at 20%, at 10% and when charged. GNOME already warns about low battery, so this is off by default")
            .active(self.settings.alerts)
            .build();
        let alerts_sender = sender.input_sender().clone();
        alerts.connect_active_notify(move |row| {
            alerts_sender.send(AppMsg::SetAlerts(row.is_active())).ok();
        });
        group.add(&alerts);

        let page = adw::PreferencesPage::new();
        page.add(&group);
        let dialog = adw::PreferencesDialog::new();
        dialog.add(&page);
        dialog
    }

    fn save_settings(&self, sender: &ComponentSender<Self>) {
        let settings = self.settings.clone();
        sender.oneshot_command(async move {
            CommandMsg::Saved(
                relm4::spawn_blocking(move || settings.save())
                    .await
                    .map_err(|error| error.to_string())
                    .and_then(|result| result),
            )
        });
    }

    fn stop_poll(&mut self) {
        if let Some(poll) = self.poll.take() {
            poll.remove();
        }
    }

    fn subtitle(&self) -> String {
        self.battery
            .as_ref()
            .map_or_else(String::new, |b| match (&b.manufacturer, &b.model) {
                (Some(make), Some(model)) => format!("{make} {model}"),
                (None, Some(model)) => model.clone(),
                _ => b.name.clone(),
            })
    }

    fn icon_name(&self) -> String {
        self.battery
            .as_ref()
            .map_or_else(|| "battery-missing-symbolic".to_owned(), Battery::icon_name)
    }

    fn percent_label(&self) -> String {
        self.battery
            .as_ref()
            .map_or_else(|| format::EMPTY.to_owned(), |b| format::percent(b.percent))
    }

    /// The rate the estimates run on: the trailing average, falling back to the
    /// live reading until there is enough history to average.
    fn smoothed_power(&self) -> Option<f64> {
        self.power_window
            .recent_power(SMOOTH_SECS, self.poll_gap_secs())
            .or_else(|| self.battery.as_ref().and_then(|b| b.power))
    }

    /// "Charging · 1 h 12 min until full" — the headline the app exists for.
    fn status_label(&self) -> String {
        let Some(battery) = &self.battery else {
            return String::new();
        };
        let remaining = self
            .smoothed_power()
            .and_then(|power| battery.time_remaining(power));
        match (battery.status, remaining) {
            (Status::Discharging, Some(left)) => {
                format!("Discharging · {} left", format::duration(left))
            }
            (Status::Charging, Some(left)) => {
                format!("Charging · {} until full", format::duration(left))
            }
            (status, _) => status.label().to_owned(),
        }
    }

    /// The graph's caption. Below a minute there is nothing to read yet, so it
    /// says so rather than drawing a misleading near-flat line.
    fn history_caption(&self) -> String {
        let span = self.history.span_secs();
        if self.history.len() < 2 || span < 60.0 {
            "Collecting samples…".to_owned()
        } else {
            format!("Last {}", format::duration(Duration::from_secs_f64(span)))
        }
    }

    fn level(&self) -> f64 {
        self.battery.as_ref().map_or(0.0, |b| b.percent / 100.0)
    }

    fn power_title(&self) -> &'static str {
        match self.battery.as_ref().map(|b| b.status) {
            Some(Status::Charging) => "Intake",
            Some(Status::Discharging) => "Draw",
            _ => "Power",
        }
    }

    fn power_label(&self) -> String {
        format::unit(self.battery.as_ref().and_then(|b| b.power), 1, "W")
    }

    /// Shown only once the average has drifted from the live reading — until
    /// then it would just repeat the number on the right of the same row.
    fn power_subtitle(&self) -> String {
        let (Some(average), Some(live)) = (
            self.power_window
                .recent_power(SMOOTH_SECS, self.poll_gap_secs()),
            self.battery.as_ref().and_then(|b| b.power),
        ) else {
            return String::new();
        };
        if (average - live).abs() < 0.1 {
            return String::new();
        }
        format!("{average:.1} W average")
    }

    fn voltage_label(&self) -> String {
        format::unit(self.battery.as_ref().and_then(|b| b.voltage), 2, "V")
    }

    fn charge_label(&self) -> String {
        let Some(battery) = &self.battery else {
            return format::EMPTY.to_owned();
        };
        match (battery.energy_now, battery.energy_full) {
            (Some(now), Some(full)) => format!("{now:.1} of {full:.1} Wh"),
            _ => format::EMPTY.to_owned(),
        }
    }

    fn source_label(&self) -> String {
        match self.battery.as_ref().map(|b| b.on_ac) {
            Some(true) => "AC adapter".to_owned(),
            Some(false) => "Battery".to_owned(),
            None => format::EMPTY.to_owned(),
        }
    }

    fn health_label(&self) -> String {
        self.battery
            .as_ref()
            .and_then(|b| b.health())
            .map_or_else(|| format::EMPTY.to_owned(), format::percent)
    }

    fn health_subtitle(&self) -> String {
        let Some(battery) = &self.battery else {
            return String::new();
        };
        match (battery.energy_full, battery.energy_design) {
            (Some(full), Some(design)) => format!("{full:.1} Wh of {design:.1} Wh when new"),
            _ => String::new(),
        }
    }

    fn cycles_label(&self) -> String {
        self.battery
            .as_ref()
            .and_then(|b| b.cycle_count)
            .map_or_else(|| format::EMPTY.to_owned(), |c| c.to_string())
    }

    fn technology_label(&self) -> String {
        format::text(self.battery.as_ref().and_then(|b| b.technology.as_ref()))
    }
}

#[relm4::component(pub)]
impl Component for AppModel {
    type Init = Settings;
    type Input = AppMsg;
    type Output = ();
    type CommandOutput = CommandMsg;

    view! {
        adw::ApplicationWindow {
            set_title: Some("Leyden"),
            set_default_size: (440, 700),

            adw::ToolbarView {
                add_top_bar = &adw::HeaderBar {
                    pack_start = &gtk::Button {
                        set_icon_name: "document-open-recent-symbolic",
                        set_tooltip_text: Some("Recordings"),
                        set_action_name: Some("win.recordings"),
                    },

                    #[wrap(Some)]
                    set_title_widget = &adw::WindowTitle {
                        set_title: "Leyden",
                        #[watch]
                        set_subtitle: &model.subtitle(),
                    },

                    #[name = "menu_button"]
                    pack_end = &gtk::MenuButton {
                        set_icon_name: "open-menu-symbolic",
                        set_tooltip_text: Some("Main Menu"),
                    },
                },

                #[wrap(Some)]
                set_content = &gtk::Box {
                    set_orientation: gtk::Orientation::Vertical,

                    adw::StatusPage {
                        #[watch]
                        set_visible: model.battery.is_none(),
                        set_vexpand: true,
                        set_icon_name: Some("battery-missing-symbolic"),
                        set_title: "No Battery Found",
                        set_description: Some(
                            "Nothing in /sys/class/power_supply reports itself as a battery."
                        ),
                    },

                    gtk::ScrolledWindow {
                        #[watch]
                        set_visible: model.battery.is_some(),
                        set_vexpand: true,
                        set_hscrollbar_policy: gtk::PolicyType::Never,

                        adw::Clamp {
                            set_maximum_size: 540,

                            gtk::Box {
                                set_orientation: gtk::Orientation::Vertical,
                                set_spacing: 18,
                                set_margin_all: 18,

                                gtk::Box {
                                    set_orientation: gtk::Orientation::Vertical,
                                    set_spacing: 4,
                                    set_margin_top: 12,
                                    set_margin_bottom: 8,

                                    gtk::Image {
                                        set_pixel_size: 48,
                                        set_margin_bottom: 4,
                                        #[watch]
                                        set_icon_name: Some(&model.icon_name()),
                                    },

                                    gtk::Label {
                                        add_css_class: "title-1",
                                        add_css_class: "numeric",
                                        #[watch]
                                        set_label: &model.percent_label(),
                                    },

                                    gtk::Label {
                                        add_css_class: "dim-label",
                                        set_wrap: true,
                                        set_justify: gtk::Justification::Center,
                                        #[watch]
                                        set_label: &model.status_label(),
                                    },

                                    gtk::LevelBar {
                                        set_margin_top: 10,
                                        #[watch]
                                        set_value: model.level(),
                                    },
                                },

                                adw::PreferencesGroup {
                                    set_title: "History",
                                    #[watch]
                                    set_description: Some(&model.history_caption()),

                                    #[name = "graph"]
                                    gtk::DrawingArea {
                                        set_height_request: 160,
                                        add_css_class: "card",
                                        #[watch]
                                        set_draw_func: model.plot.refreshed(
                                            Series::from_history(
                                                &model.history,
                                                RECORD_GAP_SECS,
                                                model.settings.clock.twelve_hour(),
                                            )
                                        ),
                                    },

                                    #[name = "readout"]
                                    gtk::Label {
                                        set_label: IDLE_READOUT,
                                        set_margin_top: 6,
                                        set_ellipsize: gtk::pango::EllipsizeMode::End,
                                        add_css_class: "dim-label",
                                        add_css_class: "numeric",
                                        add_css_class: "caption",
                                    },
                                },

                                adw::PreferencesGroup {
                                    set_title: "Power",

                                    adw::ActionRow {
                                        #[watch]
                                        set_title: model.power_title(),
                                        #[watch]
                                        set_subtitle: &model.power_subtitle(),
                                        add_suffix = &gtk::Label {
                                            add_css_class: "dim-label",
                                            add_css_class: "numeric",
                                            #[watch]
                                            set_label: &model.power_label(),
                                        },
                                    },

                                    adw::ActionRow {
                                        set_title: "Charge",
                                        add_suffix = &gtk::Label {
                                            add_css_class: "dim-label",
                                            add_css_class: "numeric",
                                            #[watch]
                                            set_label: &model.charge_label(),
                                        },
                                    },

                                    adw::ActionRow {
                                        set_title: "Voltage",
                                        add_suffix = &gtk::Label {
                                            add_css_class: "dim-label",
                                            add_css_class: "numeric",
                                            #[watch]
                                            set_label: &model.voltage_label(),
                                        },
                                    },

                                    adw::ActionRow {
                                        set_title: "Source",
                                        add_suffix = &gtk::Label {
                                            add_css_class: "dim-label",
                                            #[watch]
                                            set_label: &model.source_label(),
                                        },
                                    },
                                },

                                adw::PreferencesGroup {
                                    set_title: "Health",

                                    adw::ActionRow {
                                        set_title: "Capacity",
                                        #[watch]
                                        set_subtitle: &model.health_subtitle(),
                                        add_suffix = &gtk::Label {
                                            add_css_class: "dim-label",
                                            add_css_class: "numeric",
                                            #[watch]
                                            set_label: &model.health_label(),
                                        },
                                    },

                                    adw::ActionRow {
                                        set_title: "Charge cycles",
                                        add_suffix = &gtk::Label {
                                            add_css_class: "dim-label",
                                            add_css_class: "numeric",
                                            #[watch]
                                            set_label: &model.cycles_label(),
                                        },
                                    },

                                    adw::ActionRow {
                                        set_title: "Technology",
                                        add_suffix = &gtk::Label {
                                            add_css_class: "dim-label",
                                            #[watch]
                                            set_label: &model.technology_label(),
                                        },
                                    },
                                },
                            },
                        },
                    },
                },
            },
        }
    }

    fn init(
        settings: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let mut model = AppModel {
            battery: None,
            history: History::new(HISTORY_CAP),
            power_window: History::new(POWER_CAP),
            poll: None,
            // Read, not assumed: the window is not on screen yet, and taking
            // this for granted meant a never-shown window polled at the live
            // rate forever.
            hidden: !root.is_visible(),
            settings,
            alerts: Alerts::default(),
            recordings: recordings::load(),
            last_status: None,
            clocks: Clocks::default(),
            // The first reading of a session closes the stretch in which the app
            // was not running.
            next_follows: Follows::Launch,
            sleep_watch: None,
            plot: Plot::default(),
        };
        model.sample(&sender);

        let widgets = view_output!();

        model.plot.install_readout(&widgets.graph, &widgets.readout);

        let sleeping = sender.input_sender().clone();
        model.sleep_watch = sleep::watch_logind(move |going_to_sleep| {
            sleeping.send(AppMsg::SleepSignal(going_to_sleep)).ok();
        });

        let menu = gio::Menu::new();
        menu.append(Some("Preferences"), Some("win.preferences"));
        menu.append(Some("Keyboard Shortcuts"), Some("win.shortcuts"));
        menu.append(Some("About Leyden"), Some("win.about"));
        menu.append(Some("Quit"), Some("win.quit"));
        widgets.menu_button.set_menu_model(Some(&menu));

        let recordings_sender = sender.input_sender().clone();
        let recordings: RelmAction<RecordingsAction> = RelmAction::new_stateless(move |_| {
            recordings_sender.send(AppMsg::ShowRecordings).ok();
        });
        let preferences_sender = sender.input_sender().clone();
        let preferences: RelmAction<PreferencesAction> = RelmAction::new_stateless(move |_| {
            preferences_sender.send(AppMsg::ShowPreferences).ok();
        });
        let shortcuts_sender = sender.input_sender().clone();
        let shortcuts: RelmAction<ShortcutsAction> = RelmAction::new_stateless(move |_| {
            shortcuts_sender.send(AppMsg::ShowShortcuts).ok();
        });
        let about_sender = sender.input_sender().clone();
        let about: RelmAction<AboutAction> = RelmAction::new_stateless(move |_| {
            about_sender.send(AppMsg::ShowAbout).ok();
        });
        let quit_sender = sender.input_sender().clone();
        let quit: RelmAction<QuitAction> = RelmAction::new_stateless(move |_| {
            quit_sender.send(AppMsg::Quit).ok();
        });
        let mut actions = RelmActionGroup::<AppActionGroup>::new();
        actions.add_action(recordings);
        actions.add_action(preferences);
        actions.add_action(shortcuts);
        actions.add_action(about);
        actions.add_action(quit);
        actions.register_for_widget(&root);

        let window = root.clone();
        let show: RelmAction<ShowAction> = RelmAction::new_stateless(move |_| {
            window.present();
        });
        let quit_app_sender = sender.input_sender().clone();
        let quit_app: RelmAction<QuitAppAction> = RelmAction::new_stateless(move |_| {
            quit_app_sender.send(AppMsg::Quit).ok();
        });
        let mut app_actions = RelmActionGroup::<AppLevelGroup>::new();
        app_actions.add_action(show);
        app_actions.add_action(quit_app);
        app_actions.register_for_main_application();

        let application = relm4::main_application();
        application.set_accelerators_for_action::<RecordingsAction>(&["<Control>r"]);
        application.set_accelerators_for_action::<PreferencesAction>(&["<Control>comma"]);
        application.set_accelerators_for_action::<ShortcutsAction>(&["<Control>question"]);
        application.set_accelerators_for_action::<QuitAction>(&["<Control>q"]);

        // Occlusion and visibility are separate signals and either can leave
        // nothing on screen, so both feed one message and the reducer reads the
        // real state back off the window.
        let occluded = sender.input_sender().clone();
        root.connect_suspended_notify(move |_| {
            occluded.send(AppMsg::PresenceChanged).ok();
        });
        let shown = sender.input_sender().clone();
        root.connect_visible_notify(move |_| {
            shown.send(AppMsg::PresenceChanged).ok();
        });

        // Always intercepted: whether closing quits or merely hides is the
        // reducer's decision, and it needs the current preference.
        let closing = sender.input_sender().clone();
        root.connect_close_request(move |_| {
            closing.send(AppMsg::CloseRequested).ok();
            glib::Propagation::Stop
        });

        model.start_poll(&sender);

        // Reading the history file is disk I/O, so it cannot happen inline here.
        // The window paints with whatever this session has sampled and the older
        // samples fold in underneath a moment later.
        sender.oneshot_command(async {
            CommandMsg::Loaded(relm4::spawn_blocking(store::load).await.unwrap_or_default())
        });

        ComponentParts { model, widgets }
    }

    fn update_cmd(
        &mut self,
        msg: Self::CommandOutput,
        sender: ComponentSender<Self>,
        _root: &Self::Root,
    ) {
        match msg {
            CommandMsg::Loaded(samples) => {
                tracing::debug!("loaded {} samples from the history file", samples.len());
                self.history.absorb(samples);
                self.history.prune_older_than(store::MAX_AGE_SECS);
                // Seed from whatever was already on disk, so the list has
                // content before the next cycle completes.
                self.refresh_recordings(&sender);
            }
            CommandMsg::Written(Err(error)) => {
                tracing::warn!("could not write the history file: {error}");
            }
            CommandMsg::Written(Ok(())) => {}
            CommandMsg::Saved(Err(error)) => {
                tracing::warn!("could not write the settings file: {error}");
            }
            CommandMsg::Saved(Ok(())) => {}
            CommandMsg::Recorded(Err(error)) => {
                tracing::warn!("could not write the recordings file: {error}");
            }
            CommandMsg::Recorded(Ok(())) => {}
        }
    }

    fn update(&mut self, msg: Self::Input, sender: ComponentSender<Self>, root: &Self::Root) {
        match msg {
            AppMsg::Tick => {
                if let Some(slept) = self.clocks.advance() {
                    tracing::debug!("machine slept for {slept:?}");
                    self.next_follows = Follows::Sleep;
                }
                self.sample(&sender);
            }

            AppMsg::PresenceChanged => {
                let hidden = !root.is_visible() || root.is_suspended();
                if hidden != self.hidden {
                    self.hidden = hidden;
                    // Coming back, whatever is on screen is as stale as the
                    // absence was long.
                    if !hidden {
                        notify::background_dismissed();
                        self.sample(&sender);
                    }
                    self.restart_poll(&sender);
                }
            }

            AppMsg::SleepSignal(going_to_sleep) => {
                // logind gives a moment either side of the freeze, which is the
                // only way to get endpoints that are not a poll interval stale.
                if going_to_sleep {
                    self.sample(&sender);
                } else {
                    self.next_follows = Follows::Sleep;
                    self.clocks.reset();
                    self.sample(&sender);
                }
            }

            AppMsg::CloseRequested => {
                if self.settings.background {
                    root.set_visible(false);
                    notify::background_running();
                } else {
                    sender.input(AppMsg::Quit);
                }
            }

            AppMsg::Quit => {
                self.flush();
                relm4::main_application().quit();
            }

            AppMsg::SetBackground(enabled) => {
                if enabled != self.settings.background {
                    self.settings.background = enabled;
                    self.save_settings(&sender);
                }
            }

            AppMsg::SetPollSecs(secs) => {
                let secs = secs.clamp(MIN_POLL_SECS, MAX_POLL_SECS);
                if secs != self.settings.poll_secs {
                    self.settings.poll_secs = secs;
                    // A changed interval makes the next quiet stretch expected,
                    // not suspicious.
                    self.clocks.reset();
                    self.restart_poll(&sender);
                    self.save_settings(&sender);
                }
            }

            AppMsg::SetClock(clock) => {
                if clock != self.settings.clock {
                    self.settings.clock = clock;
                    self.save_settings(&sender);
                }
            }

            AppMsg::SetAlerts(enabled) => {
                if enabled != self.settings.alerts {
                    self.settings.alerts = enabled;
                    self.save_settings(&sender);
                }
            }

            AppMsg::SetTheme(theme) => {
                if theme != self.settings.theme {
                    self.settings.theme = theme;
                    self.settings.apply_theme();
                    self.save_settings(&sender);
                }
            }

            AppMsg::ShowRecordings => self.recordings_dialog().present(Some(root)),

            AppMsg::ShowPreferences => self.preferences_dialog(&sender).present(Some(root)),

            AppMsg::ShowShortcuts => shortcuts_dialog().present(Some(root)),

            AppMsg::ShowAbout => {
                let about = adw::AboutDialog::builder()
                    .application_name("Leyden")
                    .application_icon(crate::APP_ID)
                    .developer_name("Miguel Rincon")
                    .version(env!("CARGO_PKG_VERSION"))
                    .license_type(gtk::License::Gpl30)
                    .comments("Watch your laptop battery charge, drain and last.")
                    .build();
                about.present(Some(root));
            }
        }
    }
}

/// The accelerators registered in `init`, in the order they are worth learning.
fn shortcuts_dialog() -> adw::ShortcutsDialog {
    let section = adw::ShortcutsSection::new(Some("General"));
    section.add(adw::ShortcutsItem::new("Recordings", "<Control>r"));
    section.add(adw::ShortcutsItem::new("Preferences", "<Control>comma"));
    section.add(adw::ShortcutsItem::new(
        "Keyboard Shortcuts",
        "<Control>question",
    ));
    section.add(adw::ShortcutsItem::new("Quit", "<Control>q"));

    let dialog = adw::ShortcutsDialog::new();
    dialog.add(section);
    dialog
}

/// The plot inside an expanded run. Its shape comes from the run's own stored
/// series, so a run stays drawable long after its samples have aged out.
fn recording_graph(run: &Run, twelve_hour: bool) -> gtk::Widget {
    let series = Series::from_points(
        &run.series,
        run.started,
        run.kind == Kind::Charge,
        twelve_hour,
    );

    let area = gtk::DrawingArea::builder()
        .height_request(120)
        .hexpand(true)
        .css_classes(["card"])
        .build();
    let readout = gtk::Label::builder()
        .label(IDLE_READOUT)
        .margin_top(6)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .css_classes(["dim-label", "numeric", "caption"])
        .build();

    // The same `Plot` the main graph uses, so a run reads back exactly the way
    // the live plot does — including over its gaps.
    let plot = Plot::default();
    area.set_draw_func(plot.refreshed(series));
    plot.install_readout(&area, &readout);

    let wrapper = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .margin_top(6)
        .margin_bottom(12)
        .margin_start(12)
        .margin_end(12)
        .build();
    wrapper.append(&area);
    wrapper.append(&readout);
    wrapper.upcast()
}

/// One run: where the charge went, and how long it took to get there.
fn recording_row(run: &Run, twelve_hour: bool) -> adw::PreferencesRow {
    let title = format!(
        "{}% → {}%",
        run.from_percent.round() as i64,
        run.to_percent.round() as i64
    );

    let started = run
        .started
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs() as i64);
    let mut subtitle = format!(
        "{} · {}",
        format::time(started, twelve_hour),
        format::duration(run.elapsed())
    );
    if let Some(rate) = run.rate().filter(|rate| *rate > 0.05) {
        subtitle.push_str(&format!(" · {rate:.1}%/h"));
    }
    if let Some(watts) = run.watts {
        subtitle.push_str(&format!(" · {watts:.1} W"));
    }

    let badge = || {
        gtk::Label::builder()
            .label("in progress")
            .css_classes(["dim-label", "caption"])
            .build()
    };

    // A run with no stored shape — an older line — has nothing to expand into,
    // so it stays a plain row rather than offering an empty drawer.
    if run.series.len() < 2 {
        let row = adw::ActionRow::builder()
            .title(title)
            .subtitle(subtitle)
            .build();
        if run.in_progress {
            row.add_suffix(&badge());
        }
        return row.upcast();
    }

    let row = adw::ExpanderRow::builder()
        .title(title)
        .subtitle(subtitle)
        .build();
    if run.in_progress {
        row.add_suffix(&badge());
    }
    row.add_row(&recording_graph(run, twelve_hour));
    row.upcast()
}
