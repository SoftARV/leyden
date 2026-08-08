// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

mod app;
mod battery;
mod format;
mod graph;
mod notify;
mod settings;
mod store;

use relm4::RelmApp;
use relm4::gtk;
use relm4::gtk::gdk;
use tracing_subscriber::EnvFilter;

pub(crate) const APP_ID: &str = "dev.miguelrincon.Leyden";

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("leyden=debug")),
        )
        .init();

    // `RelmApp::new` calls `gtk::init()` and — because relm4's `libadwaita`
    // feature is on — `adw::init()`, so there is deliberately no adw init here.
    let app = RelmApp::new(APP_ID);
    setup_icon();

    // Load and apply before the window is shown, so there is no flash of the
    // wrong colour scheme. The model owns the settings from here.
    let settings = settings::Settings::load();
    settings.apply_theme();
    app.run::<app::AppModel>(settings);
}

/// On Wayland a client cannot set its own toplevel icon — GNOME Shell takes it
/// from the installed `.desktop`, so only an installed build shows one. Kept
/// because it works on X11 and lets a dev build resolve the icon pre-install.
fn setup_icon() {
    if let Some(display) = gdk::Display::default() {
        let theme = gtk::IconTheme::for_display(&display);
        theme.add_search_path(concat!(env!("CARGO_MANIFEST_DIR"), "/data/icons"));
    }
    gtk::Window::set_default_icon_name(APP_ID);
}
