// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! Root component.
//!
//! Elm/Redux shape: `AppMsg` in, one `update` reducer, view derived from
//! `AppModel`. sysfs reads are plain local file reads (microseconds), so unlike
//! the network-bound siblings they happen inline in the reducer rather than in a
//! relm4 command.

use std::time::Instant;

use relm4::actions::{AccelsPlus, RelmAction, RelmActionGroup};
use relm4::adw::prelude::*;
use relm4::gtk::{gio, glib};
use relm4::{Component, ComponentParts, ComponentSender, RelmWidgetExt, adw, gtk};

use crate::battery::history::{History, Sample};
use crate::battery::sysfs;
use crate::battery::types::{Battery, Status};
use crate::format;

const POLL_SECS: u32 = 2;

/// 30 minutes of samples at `POLL_SECS`, the window the graph will draw.
const HISTORY_CAP: usize = 900;

relm4::new_action_group!(AppActionGroup, "win");
relm4::new_stateless_action!(AboutAction, AppActionGroup, "about");
relm4::new_stateless_action!(QuitAction, AppActionGroup, "quit");

pub struct AppModel {
    battery: Option<Battery>,
    history: History,
    poll: Option<glib::SourceId>,
}

#[derive(Debug)]
pub enum AppMsg {
    Tick,
    SuspendedChanged(bool),
    ShowAbout,
}

impl AppModel {
    fn sample(&mut self) {
        self.battery = sysfs::read();
        if let Some(battery) = &self.battery {
            self.history.push(Sample {
                at: Instant::now(),
                percent: battery.percent,
                power: battery.power,
                status: battery.status,
            });
        }
    }

    fn start_poll(&mut self, sender: &ComponentSender<Self>) {
        if self.poll.is_some() {
            return;
        }
        let input = sender.input_sender().clone();
        self.poll = Some(glib::timeout_add_seconds_local(POLL_SECS, move || {
            input.send(AppMsg::Tick).ok();
            glib::ControlFlow::Continue
        }));
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

    fn percent_label(&self) -> String {
        self.battery
            .as_ref()
            .map_or_else(|| format::EMPTY.to_owned(), |b| format::percent(b.percent))
    }

    /// "Charging · 1 h 12 min until full" — the headline the app exists for.
    fn status_label(&self) -> String {
        let Some(battery) = &self.battery else {
            return String::new();
        };
        match (battery.status, battery.time_remaining()) {
            (Status::Discharging, Some(left)) => {
                format!("Discharging · {} left", format::duration(left))
            }
            (Status::Charging, Some(left)) => {
                format!("Charging · {} until full", format::duration(left))
            }
            (status, _) => status.label().to_owned(),
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
    type Init = ();
    type Input = AppMsg;
    type Output = ();
    type CommandOutput = ();

    view! {
        adw::ApplicationWindow {
            set_title: Some("Leyden"),
            set_default_size: (440, 700),

            adw::ToolbarView {
                add_top_bar = &adw::HeaderBar {
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
                            set_maximum_size: 520,

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
                                        set_icon_name: Some(
                                            model.battery.as_ref()
                                                .map_or(Status::Unknown, |b| b.status)
                                                .icon_name()
                                        ),
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
                                    set_title: "Power",

                                    adw::ActionRow {
                                        #[watch]
                                        set_title: model.power_title(),
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
        _init: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let mut model = AppModel {
            battery: None,
            history: History::new(HISTORY_CAP),
            poll: None,
        };
        model.sample();

        let widgets = view_output!();

        let menu = gio::Menu::new();
        menu.append(Some("About Leyden"), Some("win.about"));
        menu.append(Some("Quit"), Some("win.quit"));
        widgets.menu_button.set_menu_model(Some(&menu));

        let about_sender = sender.input_sender().clone();
        let about: RelmAction<AboutAction> = RelmAction::new_stateless(move |_| {
            about_sender.send(AppMsg::ShowAbout).ok();
        });
        let quit: RelmAction<QuitAction> = RelmAction::new_stateless(move |_| {
            relm4::main_application().quit();
        });
        let mut actions = RelmActionGroup::<AppActionGroup>::new();
        actions.add_action(about);
        actions.add_action(quit);
        actions.register_for_widget(&root);

        let application = relm4::main_application();
        application.set_accelerators_for_action::<QuitAction>(&["<Control>q"]);

        // A hidden window has nothing to draw, so the timer comes off entirely
        // rather than waking the CPU on a machine that is running on battery.
        let suspended = sender.input_sender().clone();
        root.connect_suspended_notify(move |window| {
            suspended
                .send(AppMsg::SuspendedChanged(window.is_suspended()))
                .ok();
        });

        model.start_poll(&sender);

        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, sender: ComponentSender<Self>, root: &Self::Root) {
        match msg {
            AppMsg::Tick => self.sample(),

            AppMsg::SuspendedChanged(suspended) => {
                if suspended {
                    self.stop_poll();
                } else {
                    self.sample();
                    self.start_poll(&sender);
                }
            }

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
