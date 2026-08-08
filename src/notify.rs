// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! Desktop notifications for the charge crossing a threshold.
//!
//! Two rules shape everything here. Alerts fire on **transitions**, never on
//! state: the poll runs every couple of seconds, so a level check would re-raise
//! the same warning forever while the battery sits at 19%. And the first reading
//! of a session **seeds silently**, so opening the app at 15% does not
//! immediately alarm you about something you already know.
//!
//! `Alerts` is the whole decision and touches nothing outside this crate's own
//! types; `send` is the only part that needs a running application.
//!
//! **Off by default.** GNOME already warns about low battery, and a second
//! banner repeating it is noise. This exists for people who have turned the
//! system warnings off, or who want the charged-to-full alert GNOME does not
//! give.

use relm4::gtk::gio;
use relm4::gtk::prelude::*;

use crate::battery::types::Status;

const CRITICAL_PERCENT: f64 = 10.0;
const LOW_PERCENT: f64 = 20.0;
const FULL_PERCENT: f64 = 100.0;

/// Once an alert stands it only clears well clear of its own threshold. A
/// battery hovering on the boundary would otherwise re-notify every time the
/// reading ticked up and back down.
const HYSTERESIS: f64 = 3.0;

/// One notification id, so a newer alert replaces the older banner instead of
/// stacking another one beside it.
const ID: &str = "battery";

/// Separate id: a low-battery warning must not replace the notice explaining
/// that the app is still running, nor the other way round.
const BACKGROUND_ID: &str = "background";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Alert {
    Low,
    Critical,
    Full,
}

impl Alert {
    fn threshold(self) -> f64 {
        match self {
            Alert::Critical => CRITICAL_PERCENT,
            Alert::Low => LOW_PERCENT,
            Alert::Full => FULL_PERCENT,
        }
    }

    fn title(self) -> &'static str {
        match self {
            Alert::Low => "Battery low",
            Alert::Critical => "Battery critically low",
            Alert::Full => "Battery charged",
        }
    }

    fn body(self, percent: f64) -> String {
        let percent = percent.round() as i64;
        match self {
            Alert::Low => format!("{percent}% remaining. Consider plugging in."),
            Alert::Critical => format!("{percent}% remaining. Plug in now."),
            Alert::Full => "The battery is fully charged.".to_owned(),
        }
    }

    fn icon_name(self) -> &'static str {
        match self {
            Alert::Low => "battery-level-20-symbolic",
            Alert::Critical => "battery-level-10-symbolic",
            Alert::Full => "battery-level-100-charged-symbolic",
        }
    }

    fn priority(self) -> gio::NotificationPriority {
        match self {
            Alert::Critical => gio::NotificationPriority::Urgent,
            _ => gio::NotificationPriority::Normal,
        }
    }
}

/// Remembers what has already been raised, so only crossings notify.
#[derive(Debug, Default)]
pub struct Alerts {
    raised: Option<Alert>,
    seeded: bool,
}

impl Alerts {
    /// Feed every reading through this. Returns the alert worth raising, which
    /// is `None` far more often than not.
    pub fn advance(&mut self, percent: f64, status: Status) -> Option<Alert> {
        let current = self.level(percent, status);
        if !self.seeded {
            self.seeded = true;
            self.raised = current;
            return None;
        }
        if current == self.raised {
            return None;
        }
        self.raised = current;
        current
    }

    fn level(&self, percent: f64, status: Status) -> Option<Alert> {
        match status {
            Status::Discharging => {
                let ceiling = |threshold: f64| match self.raised {
                    Some(raised) if raised.threshold() <= threshold => threshold + HYSTERESIS,
                    _ => threshold,
                };
                if percent <= ceiling(CRITICAL_PERCENT) {
                    Some(Alert::Critical)
                } else if percent <= ceiling(LOW_PERCENT) {
                    Some(Alert::Low)
                } else {
                    None
                }
            }
            Status::Charging | Status::Full => (percent >= FULL_PERCENT).then_some(Alert::Full),
            _ => None,
        }
    }
}

/// Raise `alert` on the desktop.
///
/// GNOME drops notifications from an application it cannot resolve to an
/// installed `.desktop` file, so this shows nothing under `cargo run` — testing
/// it needs `make install`.
pub fn send(alert: Alert, percent: f64) {
    let notification = gio::Notification::new(alert.title());
    notification.set_body(Some(&alert.body(percent)));
    notification.set_icon(&gio::ThemedIcon::new(alert.icon_name()));
    notification.set_priority(alert.priority());
    relm4::main_application().send_notification(Some(ID), &notification);
}

/// Tell the user the app is still recording after its window closed, and give
/// them the two things they would otherwise have no way to reach.
///
/// This is the only affordance for a windowless Leyden. GNOME's *Background
/// Apps* menu is driven by `org.freedesktop.background.Monitor`, which this
/// portal does not expose for an unsandboxed app, so without this the app would
/// be running with nothing to show for it and no way to stop it short of
/// relaunching.
pub fn background_running() {
    let notification = gio::Notification::new("Leyden is still recording");
    notification.set_body(Some(
        "The window is closed but the battery is still being measured.",
    ));
    notification.set_icon(&gio::ThemedIcon::new(crate::APP_ID));
    notification.set_priority(gio::NotificationPriority::Low);
    notification.add_button("Show", "app.show");
    notification.add_button("Quit", "app.quit");
    relm4::main_application().send_notification(Some(BACKGROUND_ID), &notification);
}

/// Take the notice down — the window is back, so it no longer applies.
pub fn background_dismissed() {
    relm4::main_application().withdraw_notification(BACKGROUND_ID);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn discharging(alerts: &mut Alerts, percent: f64) -> Option<Alert> {
        alerts.advance(percent, Status::Discharging)
    }

    #[test]
    fn the_first_reading_never_alerts() {
        let mut alerts = Alerts::default();
        // Opening the app at 5% must not alarm about a state already known.
        assert_eq!(discharging(&mut alerts, 5.0), None);
    }

    #[test]
    fn crossing_a_threshold_alerts_once() {
        let mut alerts = Alerts::default();
        assert_eq!(discharging(&mut alerts, 80.0), None);
        assert_eq!(discharging(&mut alerts, 20.0), Some(Alert::Low));
        // Still low, and still the same crossing.
        assert_eq!(discharging(&mut alerts, 19.0), None);
        assert_eq!(discharging(&mut alerts, 15.0), None);
        assert_eq!(discharging(&mut alerts, 10.0), Some(Alert::Critical));
        assert_eq!(discharging(&mut alerts, 4.0), None);
    }

    #[test]
    fn hovering_on_a_boundary_does_not_re_alert() {
        let mut alerts = Alerts::default();
        assert_eq!(discharging(&mut alerts, 80.0), None);
        assert_eq!(discharging(&mut alerts, 20.0), Some(Alert::Low));
        // A gauge wobbling across 20 must stay quiet until clearly recovered.
        for percent in [21.0, 20.0, 22.0, 19.0, 21.0, 23.0] {
            assert_eq!(discharging(&mut alerts, percent), None, "at {percent}%");
        }
    }

    #[test]
    fn recovering_and_falling_again_alerts_again() {
        let mut alerts = Alerts::default();
        assert_eq!(discharging(&mut alerts, 80.0), None);
        assert_eq!(discharging(&mut alerts, 18.0), Some(Alert::Low));
        assert_eq!(discharging(&mut alerts, 60.0), None);
        assert_eq!(discharging(&mut alerts, 18.0), Some(Alert::Low));
    }

    #[test]
    fn charging_clears_the_warning_and_full_alerts() {
        let mut alerts = Alerts::default();
        assert_eq!(discharging(&mut alerts, 80.0), None);
        assert_eq!(discharging(&mut alerts, 12.0), Some(Alert::Low));
        // Plugging in is not itself an alert, but it does clear the state.
        assert_eq!(alerts.advance(12.0, Status::Charging), None);
        assert_eq!(alerts.advance(99.0, Status::Charging), None);
        assert_eq!(alerts.advance(100.0, Status::Charging), Some(Alert::Full));
        assert_eq!(alerts.advance(100.0, Status::Full), None);
    }

    #[test]
    fn a_full_battery_on_power_does_not_alert_on_every_poll() {
        let mut alerts = Alerts::default();
        assert_eq!(alerts.advance(100.0, Status::Full), None);
        for _ in 0..10 {
            assert_eq!(alerts.advance(100.0, Status::Full), None);
        }
    }
}
