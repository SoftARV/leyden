// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! Noticing that the machine slept, and for how long.
//!
//! **Nothing can sample during a suspend** — userspace is frozen, no timer
//! fires, no file is read. What is measurable is the pair of readings either
//! side of it, and over hours that is a better rate than sampling would give:
//! one clean interval instead of a noisy average.
//!
//! Two mechanisms, in that order of authority:
//!
//! 1. **The clocks disagree.** `SystemTime` advances across a suspend and
//!    `Instant` (`CLOCK_MONOTONIC`) does not, so their divergence *is* the time
//!    spent asleep. This needs nothing but the standard library and is the
//!    mirror of the M2 decision — there, `Instant` stalling was the bug; here it
//!    is the signal.
//! 2. **logind says so.** `PrepareForSleep` fires *before* the freeze and again
//!    on resume, which is the only way to get endpoint readings that are not up
//!    to a poll interval stale. It is a precision aid, not the source of truth:
//!    without it the arithmetic still works, the endpoints are just older.

use std::time::{Duration, Instant, SystemTime};

use relm4::gtk::gio;

use crate::battery::history::elapsed_secs;

/// The clocks always disagree a little — scheduling, a slow poll, an NTP step.
/// Only a divergence past this is a suspend.
const SLEPT_SECS: f64 = 10.0;

/// Watches the two clocks drift apart.
#[derive(Debug, Default)]
pub struct Clocks {
    last: Option<(SystemTime, Instant)>,
}

impl Clocks {
    /// Call once per poll. Returns how long the machine was asleep since the
    /// previous call, when it was.
    pub fn advance(&mut self) -> Option<Duration> {
        let now = (SystemTime::now(), Instant::now());
        let slept = self.last.and_then(|(wall, mono)| {
            let by_wall = elapsed_secs(wall, now.0);
            let by_mono = now.1.duration_since(mono).as_secs_f64();
            let difference = by_wall - by_mono;
            (difference > SLEPT_SECS).then(|| Duration::from_secs_f64(difference))
        });
        self.last = Some(now);
        slept
    }

    /// Forget the previous reading, so the next call cannot report a suspend.
    /// Used when the poll interval itself changed and a long quiet stretch is
    /// expected rather than suspicious.
    pub fn reset(&mut self) {
        self.last = None;
    }
}

/// Holds the bus connection and the subscription. Both unsubscribe when
/// dropped, so this lives as long as the app does.
pub struct Watcher {
    _connection: gio::DBusConnection,
    _subscription: gio::SignalSubscription,
}

impl std::fmt::Debug for Watcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Watcher(logind)")
    }
}

/// Subscribe to logind's `PrepareForSleep`, calling `on_signal(true)` just
/// before the machine freezes and `on_signal(false)` when it wakes.
///
/// Best effort by design. A machine without systemd, or a bus that cannot be
/// reached, leaves the clock arithmetic to do the work on its own — so a failure
/// here is a `debug!`, never an error the user sees.
///
/// The bus connection is a local socket opened once at startup: the same class
/// of cost as the sysfs reads that rule 3 already allows inline.
pub fn watch_logind<F: Fn(bool) + 'static>(on_signal: F) -> Option<Watcher> {
    let connection = gio::bus_get_sync(gio::BusType::System, gio::Cancellable::NONE)
        .inspect_err(|error| {
            tracing::debug!("no system bus; sleep endpoints will come from the clocks: {error}");
        })
        .ok()?;

    let subscription = connection.subscribe_to_signal(
        Some("org.freedesktop.login1"),
        Some("org.freedesktop.login1.Manager"),
        Some("PrepareForSleep"),
        Some("/org/freedesktop/login1"),
        None,
        gio::DBusSignalFlags::NONE,
        move |signal| {
            if let Some(going_to_sleep) = signal.parameters.child_value(0).get::<bool>() {
                on_signal(going_to_sleep);
            }
        },
    );

    tracing::debug!("watching logind for suspend");
    Some(Watcher {
        _connection: connection,
        _subscription: subscription,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_normal_poll_is_not_a_suspend() {
        let mut clocks = Clocks::default();
        assert!(clocks.advance().is_none(), "the first call has no previous");
        // Two calls in quick succession: both clocks moved together.
        assert!(clocks.advance().is_none());
    }

    #[test]
    fn a_reset_swallows_the_next_reading() {
        let mut clocks = Clocks::default();
        clocks.advance();
        clocks.reset();
        assert!(clocks.advance().is_none());
    }

    #[test]
    fn a_sleep_reads_as_its_wall_clock_length() {
        // Wall clock jumped an hour while the monotonic clock barely moved —
        // exactly what a suspend looks like from userspace.
        let mut clocks = Clocks {
            last: Some((
                SystemTime::now() - Duration::from_secs(3600),
                Instant::now(),
            )),
        };
        let slept = clocks
            .advance()
            .expect("an hour of wall clock is a suspend");
        assert!((slept.as_secs_f64() - 3600.0).abs() < 5.0, "got {slept:?}");
    }
}
