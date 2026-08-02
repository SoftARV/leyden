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

    pub fn icon_name(self) -> &'static str {
        match self {
            Status::Charging => "battery-level-100-charging-symbolic",
            Status::Full => "battery-level-100-charged-symbolic",
            _ => "battery-symbolic",
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
    /// Usable capacity against the factory rating, as a percentage.
    pub fn health(&self) -> Option<f64> {
        let (full, design) = (self.energy_full?, self.energy_design?);
        (design > 0.0).then(|| full / design * 100.0)
    }

    /// Time until empty when discharging, until full when charging. `None` when
    /// idle, or while the gauge still reads zero draw right after a state change.
    pub fn time_remaining(&self) -> Option<Duration> {
        let power = self.power.filter(|p| *p > 0.1)?;
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
    fn health_is_full_over_design() {
        let health = battery(Status::Full, 50.0, 0.0).health().unwrap();
        assert!((health - 83.333).abs() < 0.01);
    }

    #[test]
    fn discharging_drains_what_is_left() {
        let left = battery(Status::Discharging, 25.0, 10.0)
            .time_remaining()
            .unwrap();
        assert_eq!(left.as_secs(), 9000);
    }

    #[test]
    fn charging_fills_the_gap() {
        let left = battery(Status::Charging, 25.0, 10.0)
            .time_remaining()
            .unwrap();
        assert_eq!(left.as_secs(), 9000);
    }

    #[test]
    fn idle_and_zero_draw_have_no_estimate() {
        assert!(battery(Status::Full, 50.0, 0.0).time_remaining().is_none());
        assert!(
            battery(Status::Discharging, 25.0, 0.0)
                .time_remaining()
                .is_none()
        );
    }
}
