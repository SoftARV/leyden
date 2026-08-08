// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! Reads `/sys/class/power_supply`.
//!
//! Two gauge flavours exist and a laptop has exactly one of them: *energy*
//! gauges expose `energy_now`/`energy_full`/`power_now` in µWh and µW, *charge*
//! gauges expose `charge_now`/`charge_full`/`current_now` in µAh and µA. The
//! charge kind is converted with `voltage_now`, so everything above this file
//! sees Wh and W.
//!
//! Everything comes from each supply's **`uevent`**, which holds every value in
//! one file. Reading the individual attributes instead meant about twenty opens
//! per sample; at the poll rate that was the app's entire measurable cost. It
//! also makes the parsing testable from a fixture, so neither gauge branch can
//! rot unnoticed.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use super::types::{Battery, Status};

const SUPPLY_DIR: &str = "/sys/class/power_supply";

/// One supply's `uevent`, keyed the way the sysfs attributes are named:
/// `POWER_SUPPLY_ENERGY_NOW` is stored as `energy_now`.
struct Uevent(HashMap<String, String>);

impl Uevent {
    fn read(dir: &Path) -> Option<Self> {
        Some(Self::parse(&fs::read_to_string(dir.join("uevent")).ok()?))
    }

    fn parse(text: &str) -> Self {
        Uevent(
            text.lines()
                .filter_map(|line| line.split_once('='))
                .map(|(key, value)| {
                    let key = key
                        .trim()
                        .trim_start_matches("POWER_SUPPLY_")
                        .to_lowercase();
                    (key, value.trim().to_owned())
                })
                .filter(|(_, value)| !value.is_empty())
                .collect(),
        )
    }

    fn text(&self, key: &str) -> Option<&str> {
        self.0.get(key).map(String::as_str)
    }

    fn number(&self, key: &str) -> Option<f64> {
        self.text(key)?.parse().ok()
    }
}

/// The first battery present, with mains state folded in. `None` on a desktop.
pub fn read() -> Option<Battery> {
    let supplies: Vec<(PathBuf, Uevent)> = supplies()
        .into_iter()
        .filter_map(|dir| Uevent::read(&dir).map(|uevent| (dir, uevent)))
        .collect();

    let on_ac = supplies.iter().any(|(_, uevent)| {
        uevent.text("type") == Some("Mains") && uevent.text("online") == Some("1")
    });

    supplies
        .into_iter()
        .find(|(_, uevent)| uevent.text("type") == Some("Battery"))
        .map(|(dir, uevent)| battery(&dir, &uevent, on_ac))
}

fn supplies() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = fs::read_dir(SUPPLY_DIR)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .collect();
    dirs.sort();
    dirs
}

fn battery(dir: &Path, uevent: &Uevent, on_ac: bool) -> Battery {
    let voltage = uevent.number("voltage_now").map(|v| v / 1e6);

    // µWh -> Wh directly, or µAh -> Ah -> Wh through the present voltage.
    let energy = |energy_key: &str, charge_key: &str| {
        uevent
            .number(energy_key)
            .map(|e| e / 1e6)
            .or_else(|| Some(uevent.number(charge_key)? / 1e6 * voltage?))
    };
    let energy_now = energy("energy_now", "charge_now");
    let energy_full = energy("energy_full", "charge_full");
    let energy_design = energy("energy_full_design", "charge_full_design");

    // Some drivers sign `current_now` by direction; the status carries that, so
    // power stays a magnitude.
    let power = uevent
        .number("power_now")
        .map(|p| p.abs() / 1e6)
        .or_else(|| Some((uevent.number("current_now")?.abs() / 1e6) * voltage?));

    let percent = uevent
        .number("capacity")
        .or_else(|| Some(energy_now? / energy_full? * 100.0))
        .unwrap_or(0.0)
        .clamp(0.0, 100.0);

    Battery {
        name: uevent.text("name").map_or_else(
            || {
                dir.file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default()
            },
            ToOwned::to_owned,
        ),
        status: uevent.text("status").map_or(Status::Unknown, Status::parse),
        percent,
        energy_now,
        energy_full,
        energy_design,
        power,
        voltage,
        cycle_count: uevent
            .number("cycle_count")
            .map(|count| count as u32)
            .filter(|count| *count > 0),
        model: uevent.text("model_name").map(ToOwned::to_owned),
        manufacturer: uevent.text("manufacturer").map(ToOwned::to_owned),
        technology: uevent
            .text("technology")
            .filter(|technology| *technology != "Unknown")
            .map(ToOwned::to_owned),
        on_ac,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// A charge gauge with no `power_now` — the author's Razer Blade, captured
    /// verbatim. This is the conversion path that runs daily.
    const CHARGE_GAUGE: &str = "\
POWER_SUPPLY_NAME=BAT0
POWER_SUPPLY_TYPE=Battery
POWER_SUPPLY_STATUS=Discharging
POWER_SUPPLY_PRESENT=1
POWER_SUPPLY_TECHNOLOGY=Unknown
POWER_SUPPLY_CYCLE_COUNT=22
POWER_SUPPLY_VOLTAGE_NOW=15450000
POWER_SUPPLY_CURRENT_NOW=806000
POWER_SUPPLY_CHARGE_FULL_DESIGN=4417000
POWER_SUPPLY_CHARGE_FULL=4344000
POWER_SUPPLY_CHARGE_NOW=1096000
POWER_SUPPLY_CAPACITY=25
POWER_SUPPLY_MODEL_NAME=Blade
POWER_SUPPLY_MANUFACTURER=Razer
";

    /// An energy gauge, as most Intel laptops report. Neither branch may rot.
    const ENERGY_GAUGE: &str = "\
POWER_SUPPLY_NAME=BAT0
POWER_SUPPLY_TYPE=Battery
POWER_SUPPLY_STATUS=Charging
POWER_SUPPLY_TECHNOLOGY=Li-ion
POWER_SUPPLY_CYCLE_COUNT=0
POWER_SUPPLY_VOLTAGE_NOW=12000000
POWER_SUPPLY_POWER_NOW=15000000
POWER_SUPPLY_ENERGY_FULL_DESIGN=50000000
POWER_SUPPLY_ENERGY_FULL=48000000
POWER_SUPPLY_ENERGY_NOW=24000000
POWER_SUPPLY_CAPACITY=50
";

    fn parse(text: &str) -> Battery {
        battery(
            Path::new("/sys/class/power_supply/BAT0"),
            &Uevent::parse(text),
            false,
        )
    }

    #[test]
    fn a_charge_gauge_is_converted_through_voltage() {
        let battery = parse(CHARGE_GAUGE);
        assert_eq!(battery.status, Status::Discharging);
        assert_eq!(battery.percent, 25.0);
        // 1.096 Ah × 15.45 V
        assert!((battery.energy_now.unwrap() - 16.93).abs() < 0.01);
        // 0.806 A × 15.45 V — there is no power_now on this machine.
        assert!((battery.power.unwrap() - 12.45).abs() < 0.01);
        assert!((battery.voltage.unwrap() - 15.45).abs() < 0.001);
        assert_eq!(battery.cycle_count, Some(22));
        assert_eq!(battery.model.as_deref(), Some("Blade"));
        // The literal string "Unknown" is not a technology.
        assert_eq!(battery.technology, None);
    }

    #[test]
    fn an_energy_gauge_is_read_directly() {
        let battery = parse(ENERGY_GAUGE);
        assert_eq!(battery.status, Status::Charging);
        assert!((battery.energy_now.unwrap() - 24.0).abs() < 0.001);
        assert!((battery.energy_full.unwrap() - 48.0).abs() < 0.001);
        // power_now wins; it must not be re-derived from current_now.
        assert!((battery.power.unwrap() - 15.0).abs() < 0.001);
        assert_eq!(battery.technology.as_deref(), Some("Li-ion"));
        // A zero cycle count is no reading at all.
        assert_eq!(battery.cycle_count, None);
    }

    #[test]
    fn a_missing_value_is_none_not_a_zero() {
        let battery = parse("POWER_SUPPLY_NAME=BAT0\nPOWER_SUPPLY_TYPE=Battery\n");
        assert_eq!(battery.energy_now, None);
        assert_eq!(battery.power, None);
        assert_eq!(battery.voltage, None);
        assert_eq!(battery.status, Status::Unknown);
        assert_eq!(battery.percent, 0.0);
    }

    #[test]
    fn junk_lines_do_not_derail_the_parse() {
        let uevent = Uevent::parse("garbage\nPOWER_SUPPLY_CAPACITY=42\n\nEMPTY=\n");
        assert_eq!(uevent.number("capacity"), Some(42.0));
        assert_eq!(uevent.text("empty"), None);
    }
}
