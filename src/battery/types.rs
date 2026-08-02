// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! Our battery types. Nothing above this module reads a sysfs string.

use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Status {
    Charging,
    Discharging,
    Full,
    NotCharging,
    #[default]
    Unknown,
}

impl Status {
    pub fn parse(raw: &str) -> Self {
        match raw.trim() {
            "Charging" => Status::Charging,
            "Discharging" => Status::Discharging,
            "Full" => Status::Full,
            "Not charging" => Status::NotCharging,
            _ => Status::Unknown,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Status::Charging => "Charging",
            Status::Discharging => "Discharging",
            Status::Full => "Fully charged",
            Status::NotCharging => "Not charging",
            Status::Unknown => "Unknown",
        }
    }
}

/// One reading of one battery. Energies are Wh, power W, voltage V — the sysfs
/// micro-units and the charge/energy gauge split are resolved in `sysfs.rs`.
#[derive(Debug, Clone, Default)]
pub struct Battery {
    pub name: String,
    pub status: Status,
    pub percent: f64,
    pub energy_now: Option<f64>,
    pub energy_full: Option<f64>,
    pub energy_design: Option<f64>,
    pub power: Option<f64>,
    pub voltage: Option<f64>,
    pub cycle_count: Option<u32>,
    pub model: Option<String>,
    pub manufacturer: Option<String>,
    pub technology: Option<String>,
    pub on_ac: bool,
}

impl Battery {
    /// The themed icon for this reading. Adwaita names these per 10% step, and
    /// there is **no** `battery-level-100-charging-symbolic` — a full battery on
    /// power is `charged`, not `charging`.
    pub fn icon_name(&self) -> String {
        let level = (self.percent / 10.0).round().clamp(0.0, 10.0) as u32 * 10;
        let state = match self.status {
            Status::Full => return "battery-level-100-charged-symbolic".to_owned(),
            Status::Charging if level == 100 => {
                return "battery-level-100-charged-symbolic".to_owned();
            }
            Status::Charging => "-charging",
            Status::NotCharging | Status::Unknown if self.on_ac => "-plugged-in",
            _ => "",
        };
        format!("battery-level-{level}{state}-symbolic")
    }

    /// Usable capacity against the factory rating, as a percentage.
    pub fn health(&self) -> Option<f64> {
        let (full, design) = (self.energy_full?, self.energy_design?);
        (design > 0.0).then(|| full / design * 100.0)
    }

    /// Time until empty when discharging, until full when charging, at the given
    /// rate in watts. `None` when idle, or while the gauge still reads zero draw
    /// right after a state change.
    ///
    /// The rate is a parameter rather than `self.power` because a single reading
    /// swings with whatever the CPU is doing; callers pass the smoothed value
    /// from `History::recent_power`.
    pub fn time_remaining(&self, power: f64) -> Option<Duration> {
        let power = (power > 0.1).then_some(power)?;
        let now = self.energy_now?;
        let hours = match self.status {
            Status::Discharging => now / power,
            Status::Charging => (self.energy_full? - now).max(0.0) / power,
            _ => return None,
        };
        (hours.is_finite() && hours > 0.0).then(|| Duration::from_secs_f64(hours * 3600.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn battery(status: Status, now: f64, power: f64) -> Battery {
        Battery {
            status,
            energy_now: Some(now),
            energy_full: Some(50.0),
            energy_design: Some(60.0),
            power: Some(power),
            ..Battery::default()
        }
    }

    #[test]
    fn icons_follow_the_level_and_never_ask_for_a_missing_name() {
        let mut battery = battery(Status::Charging, 50.0, 10.0);
        battery.percent = 47.0;
        assert_eq!(battery.icon_name(), "battery-level-50-charging-symbolic");

        // Adwaita has no 100-charging icon; a full battery on power is "charged".
        battery.percent = 100.0;
        assert_eq!(battery.icon_name(), "battery-level-100-charged-symbolic");

        battery.status = Status::Discharging;
        battery.percent = 3.0;
        assert_eq!(battery.icon_name(), "battery-level-0-symbolic");

        battery.status = Status::NotCharging;
        battery.on_ac = true;
        battery.percent = 62.0;
        assert_eq!(battery.icon_name(), "battery-level-60-plugged-in-symbolic");
    }

    #[test]
    fn health_is_full_over_design() {
        let health = battery(Status::Full, 50.0, 0.0).health().unwrap();
        assert!((health - 83.333).abs() < 0.01);
    }

    #[test]
    fn discharging_drains_what_is_left() {
        let left = battery(Status::Discharging, 25.0, 10.0)
            .time_remaining(10.0)
            .unwrap();
        assert_eq!(left.as_secs(), 9000);
    }

    #[test]
    fn charging_fills_the_gap() {
        let left = battery(Status::Charging, 25.0, 10.0)
            .time_remaining(10.0)
            .unwrap();
        assert_eq!(left.as_secs(), 9000);
    }

    #[test]
    fn the_estimate_follows_the_rate_it_is_given() {
        // The smoothed rate, not the instantaneous one, decides the answer.
        let battery = battery(Status::Discharging, 25.0, 50.0);
        assert_eq!(battery.time_remaining(10.0).unwrap().as_secs(), 9000);
        assert_eq!(battery.time_remaining(25.0).unwrap().as_secs(), 3600);
    }

    #[test]
    fn idle_and_zero_draw_have_no_estimate() {
        assert!(
            battery(Status::Full, 50.0, 0.0)
                .time_remaining(10.0)
                .is_none()
        );
        assert!(
            battery(Status::Discharging, 25.0, 10.0)
                .time_remaining(0.0)
                .is_none()
        );
    }
}
