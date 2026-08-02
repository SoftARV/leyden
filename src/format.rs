// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! Value -> label. Every "—" in the UI comes from a `None` here.

use std::time::Duration;

pub const EMPTY: &str = "—";

pub fn duration(value: Duration) -> String {
    let minutes = value.as_secs() / 60;
    match (minutes / 60, minutes % 60) {
        (0, 0) => "less than a minute".to_owned(),
        (0, m) => format!("{m} min"),
        (h, 0) => format!("{h} h"),
        (h, m) => format!("{h} h {m} min"),
    }
}

pub fn unit(value: Option<f64>, decimals: usize, suffix: &str) -> String {
    value.map_or_else(
        || EMPTY.to_owned(),
        |v| format!("{v:.decimals$} {suffix}", decimals = decimals),
    )
}

pub fn percent(value: f64) -> String {
    format!("{}%", value.round() as i64)
}

pub fn text(value: Option<&String>) -> String {
    value.cloned().unwrap_or_else(|| EMPTY.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_read_naturally() {
        assert_eq!(duration(Duration::from_secs(30)), "less than a minute");
        assert_eq!(duration(Duration::from_secs(48 * 60)), "48 min");
        assert_eq!(duration(Duration::from_secs(2 * 3600)), "2 h");
        assert_eq!(duration(Duration::from_secs(72 * 60)), "1 h 12 min");
    }

    #[test]
    fn missing_values_render_as_a_dash() {
        assert_eq!(unit(None, 1, "W"), EMPTY);
        assert_eq!(unit(Some(12.44), 1, "W"), "12.4 W");
        assert_eq!(text(None), EMPTY);
    }
}
