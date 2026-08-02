// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! Reads `/sys/class/power_supply`.
//!
//! Two gauge flavours exist and a laptop has exactly one of them: *energy*
//! gauges expose `energy_now`/`energy_full`/`power_now` in µWh and µW, *charge*
//! gauges expose `charge_now`/`charge_full`/`current_now` in µAh and µA. The
//! charge kind is converted with `voltage_now`, so everything above this file
//! sees Wh and W.

use std::fs;
use std::path::{Path, PathBuf};

use super::types::{Battery, Status};

const SUPPLY_DIR: &str = "/sys/class/power_supply";

/// The first battery present, with mains state folded in. `None` on a desktop.
pub fn read() -> Option<Battery> {
    let on_ac = mains_online();
    supplies()
        .into_iter()
        .find(|dir| read_str(dir, "type").as_deref() == Some("Battery"))
        .map(|dir| read_battery(&dir, on_ac))
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

fn mains_online() -> bool {
    supplies().iter().any(|dir| {
        read_str(dir, "type").as_deref() == Some("Mains") && read_f64(dir, "online") == Some(1.0)
    })
}

fn read_battery(dir: &Path, on_ac: bool) -> Battery {
    let voltage = read_f64(dir, "voltage_now").map(|v| v / 1e6);

    // µWh -> Wh directly, or µAh -> Ah -> Wh through the present voltage.
    let energy = |energy_key: &str, charge_key: &str| {
        read_f64(dir, energy_key)
            .map(|e| e / 1e6)
            .or_else(|| Some(read_f64(dir, charge_key)? / 1e6 * voltage?))
    };
    let energy_now = energy("energy_now", "charge_now");
    let energy_full = energy("energy_full", "charge_full");
    let energy_design = energy("energy_full_design", "charge_full_design");

    // Some drivers sign `current_now` by direction; the status carries that, so
    // power stays a magnitude.
    let power = read_f64(dir, "power_now")
        .map(|p| p.abs() / 1e6)
        .or_else(|| Some((read_f64(dir, "current_now")?.abs() / 1e6) * voltage?));

    let percent = read_f64(dir, "capacity")
        .or_else(|| Some(energy_now? / energy_full? * 100.0))
        .unwrap_or(0.0)
        .clamp(0.0, 100.0);

    Battery {
        name: dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        status: read_str(dir, "status").map_or(Status::Unknown, |s| Status::parse(&s)),
        percent,
        energy_now,
        energy_full,
        energy_design,
        power,
        voltage,
        cycle_count: read_f64(dir, "cycle_count")
            .map(|c| c as u32)
            .filter(|c| *c > 0),
        model: read_str(dir, "model_name"),
        manufacturer: read_str(dir, "manufacturer"),
        technology: read_str(dir, "technology").filter(|t| t != "Unknown"),
        on_ac,
    }
}

fn read_str(dir: &Path, key: &str) -> Option<String> {
    let value = fs::read_to_string(dir.join(key)).ok()?;
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

fn read_f64(dir: &Path, key: &str) -> Option<f64> {
    read_str(dir, key)?.parse().ok()
}
