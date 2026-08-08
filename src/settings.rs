// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! Persistent preferences: `~/.config/leyden/settings.ini`, via `glib::KeyFile`.
//!
//! Deliberately **not** GSettings, which needs a compiled schema installed
//! before the app will start at all — that would break `cargo run`. An INI file
//! costs no dependency and is readable with an editor.
//!
//! A missing or unparseable file is a first run, not an error: every value falls
//! back to its default.

use std::path::PathBuf;

use relm4::adw;
use relm4::gtk::gio;
use relm4::gtk::glib;
use relm4::gtk::prelude::*;

const GROUP: &str = "leyden";

/// Below a second the poll would cost more than it measures; above thirty the
/// live numbers stop feeling live.
pub const MIN_POLL_SECS: u32 = 1;
pub const MAX_POLL_SECS: u32 = 30;
const DEFAULT_POLL_SECS: u32 = 2;

const _: () = assert!(
    DEFAULT_POLL_SECS >= MIN_POLL_SECS && DEFAULT_POLL_SECS <= MAX_POLL_SECS,
    "the default poll interval must sit inside its own bounds"
);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Theme {
    #[default]
    System,
    Light,
    Dark,
}

impl Theme {
    fn color_scheme(self) -> adw::ColorScheme {
        match self {
            Theme::System => adw::ColorScheme::Default,
            Theme::Light => adw::ColorScheme::ForceLight,
            Theme::Dark => adw::ColorScheme::ForceDark,
        }
    }

    fn as_key(self) -> &'static str {
        match self {
            Theme::System => "system",
            Theme::Light => "light",
            Theme::Dark => "dark",
        }
    }

    fn from_key(key: &str) -> Self {
        match key {
            "light" => Theme::Light,
            "dark" => Theme::Dark,
            _ => Theme::System,
        }
    }

    /// Position in the Appearance combo row, and back.
    pub fn index(self) -> u32 {
        match self {
            Theme::System => 0,
            Theme::Light => 1,
            Theme::Dark => 2,
        }
    }

    pub fn from_index(index: u32) -> Self {
        match index {
            1 => Theme::Light,
            2 => Theme::Dark,
            _ => Theme::System,
        }
    }
}

/// How the hover shows a time of day.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Clock {
    /// Whatever GNOME itself is set to.
    #[default]
    System,
    TwentyFour,
    Twelve,
}

impl Clock {
    fn as_key(self) -> &'static str {
        match self {
            Clock::System => "system",
            Clock::TwentyFour => "24h",
            Clock::Twelve => "12h",
        }
    }

    fn from_key(key: &str) -> Self {
        match key {
            "24h" => Clock::TwentyFour,
            "12h" => Clock::Twelve,
            _ => Clock::System,
        }
    }

    pub fn index(self) -> u32 {
        match self {
            Clock::System => 0,
            Clock::TwentyFour => 1,
            Clock::Twelve => 2,
        }
    }

    pub fn from_index(index: u32) -> Self {
        match index {
            1 => Clock::TwentyFour,
            2 => Clock::Twelve,
            _ => Clock::System,
        }
    }

    /// Resolved against the desktop when set to `System`.
    pub fn twelve_hour(self) -> bool {
        match self {
            Clock::TwentyFour => false,
            Clock::Twelve => true,
            Clock::System => system_twelve_hour(),
        }
    }
}

/// GNOME's own `clock-format`, or 24-hour if it cannot be read.
///
/// `gio::Settings::new` **aborts the process** when the schema is not
/// installed, so the schema is looked up first. A non-GNOME desktop must not
/// take the app down over a time format.
fn system_twelve_hour() -> bool {
    const SCHEMA: &str = "org.gnome.desktop.interface";
    let Some(source) = gio::SettingsSchemaSource::default() else {
        return false;
    };
    if source.lookup(SCHEMA, true).is_none() {
        return false;
    }
    gio::Settings::new(SCHEMA).string("clock-format") == "12h"
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    pub poll_secs: u32,
    pub theme: Theme,
    pub alerts: bool,
    pub clock: Clock,
    pub background: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            poll_secs: DEFAULT_POLL_SECS,
            theme: Theme::default(),
            // Off by default: GNOME already warns about low battery, and a
            // second banner saying the same thing is noise, not a feature.
            alerts: false,
            clock: Clock::default(),
            // Off by default: closing a window should close the app unless the
            // user has decided otherwise.
            background: false,
        }
    }
}

impl Settings {
    pub fn load() -> Self {
        let file = glib::KeyFile::new();
        if file
            .load_from_file(path(), glib::KeyFileFlags::NONE)
            .is_err()
        {
            return Settings::default();
        }
        Settings {
            poll_secs: file
                .integer(GROUP, "poll-secs")
                .ok()
                .and_then(|secs| u32::try_from(secs).ok())
                .map_or(DEFAULT_POLL_SECS, |secs| {
                    secs.clamp(MIN_POLL_SECS, MAX_POLL_SECS)
                }),
            theme: file
                .string(GROUP, "theme")
                .map_or_else(|_| Theme::default(), |key| Theme::from_key(&key)),
            alerts: file.boolean(GROUP, "alerts").unwrap_or(false),
            clock: file
                .string(GROUP, "clock")
                .map_or_else(|_| Clock::default(), |key| Clock::from_key(&key)),
            background: file.boolean(GROUP, "background").unwrap_or(false),
        }
    }

    /// Blocking — the caller runs it off the main thread.
    pub fn save(&self) -> Result<(), String> {
        let file = glib::KeyFile::new();
        let Ok(secs) = i32::try_from(self.poll_secs) else {
            return Err("poll interval out of range".to_owned());
        };
        file.set_integer(GROUP, "poll-secs", secs);
        file.set_string(GROUP, "theme", self.theme.as_key());
        file.set_boolean(GROUP, "alerts", self.alerts);
        file.set_string(GROUP, "clock", self.clock.as_key());
        file.set_boolean(GROUP, "background", self.background);

        let path = path();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|error| error.to_string())?;
        }
        file.save_to_file(&path).map_err(|error| error.to_string())
    }

    pub fn apply_theme(&self) {
        adw::StyleManager::default().set_color_scheme(self.theme.color_scheme());
    }
}

fn path() -> PathBuf {
    glib::user_config_dir().join("leyden").join("settings.ini")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theme_keys_round_trip() {
        for theme in [Theme::System, Theme::Light, Theme::Dark] {
            assert_eq!(Theme::from_key(theme.as_key()), theme);
            assert_eq!(Theme::from_index(theme.index()), theme);
        }
    }

    #[test]
    fn clock_keys_round_trip() {
        for clock in [Clock::System, Clock::TwentyFour, Clock::Twelve] {
            assert_eq!(Clock::from_key(clock.as_key()), clock);
            assert_eq!(Clock::from_index(clock.index()), clock);
        }
        assert_eq!(Clock::from_key("half past"), Clock::System);
        assert!(Clock::Twelve.twelve_hour());
        assert!(!Clock::TwentyFour.twelve_hour());
    }

    #[test]
    fn an_unknown_theme_key_falls_back_to_the_system() {
        assert_eq!(Theme::from_key("solarized"), Theme::System);
        assert_eq!(Theme::from_index(99), Theme::System);
    }
}
