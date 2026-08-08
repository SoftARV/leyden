// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! Value -> label. Every "—" in the UI comes from a `None` here.

use std::time::Duration;

use relm4::gtk::glib;

pub const EMPTY: &str = "—";

/// Wall clock for a Unix timestamp, in local time. `%l` is space padded, hence
/// the trim; `%p` is empty in locales without AM/PM, which trims away too.
pub fn time(unix_secs: i64, twelve_hour: bool) -> String {
    let pattern = if twelve_hour { "%l:%M %p" } else { "%H:%M" };
    glib::DateTime::from_unix_local(unix_secs)
        .and_then(|local| local.format(pattern))
        .map(|text| text.trim().to_owned())
        .unwrap_or_default()
}

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
    fn the_clock_follows_the_chosen_notation() {
        // 1970-01-01 00:00 UTC, rendered in whatever the local zone is — the
        // shape is what matters, not the hour.
        let twenty_four = time(0, false);
        let twelve = time(0, true);
        assert_eq!(twenty_four.len(), 5, "{twenty_four}");
        assert!(twenty_four.contains(':'), "{twenty_four}");
        assert!(
            twelve.contains("AM") || twelve.contains("PM") || twelve == twenty_four,
            "{twelve}"
        );
        assert!(
            !twelve.starts_with(' '),
            "padding must be trimmed: {twelve:?}"
        );
    }

    #[test]
    fn missing_values_render_as_a_dash() {
        assert_eq!(unit(None, 1, "W"), EMPTY);
        assert_eq!(unit(Some(12.44), 1, "W"), "12.4 W");
        assert_eq!(text(None), EMPTY);
    }
}
