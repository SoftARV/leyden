<p align="center">
  <img src="data/icons/hicolor/scalable/apps/dev.miguelrincon.Leyden.svg"
       width="128" height="128" alt="">
</p>

<h1 align="center">Leyden</h1>

<p align="center">
  A small, native GNOME app that watches your laptop battery: state, capacity,
  live power draw and intake, and how long a charge lasts.
</p>

Named after the [Leyden jar](https://en.wikipedia.org/wiki/Leyden_jar) (1745),
the first device that could store an electric charge.

Built with Rust, [relm4](https://relm4.org), GTK 4 and libadwaita — for one
laptop, running GNOME.

<p align="center">
  <img src="docs/screenshots/overview.png" width="440"
       alt="Leyden showing a battery at 70%, discharging with 4 h 31 min left: a
            graph of the last 2 h 56 min with hour marks and a bridged gap, and a
            9.6 W draw with a 10.7 W average">
</p>

## Status

Live battery state, charge, power, voltage, health and cycle count, read from
`/sys/class/power_supply`, plus a graph of the last 24 hours — green while
charging, accent while draining. History is kept in
`~/.local/share/leyden/history.tsv`, so the graph is already populated when you
open the window. Time estimates run on a smoothed power rate rather than a single
noisy reading.

**Recordings** — the button on the left of the header — lists the last five
discharges and the last five charges, each with how long it took, what it cost,
and its own graph. Gaps are bridged rather than left as holes, and say what they
were: a suspend reads *"Asleep · 8 h · 11% used (1.4%/h)"*, a stretch with the app
closed says so instead.

Preferences (refresh interval, theme, time format, alerts, background) live in
`~/.config/leyden/settings.ini`. With **Keep running in the background** on,
closing the window keeps recording, so a long measurement is not interrupted;
relaunching brings the window back. Notifications at 20%, 10% and full charge need
the app installed — GNOME ignores notifications from an app it cannot resolve to
an installed `.desktop`.

## Install

```bash
make install     # builds release, installs to ~/.local — no sudo
```

Then launch **Leyden** from the app grid, or run `leyden`.

Requires GTK ≥ 4.20 and libadwaita ≥ 1.8. On Arch:

```bash
sudo pacman -S --needed base-devel pkgconf rust gtk4 libadwaita
```

## Development

```bash
cargo run
make check       # fmt + clippy -D warnings + test
```

## Licence

GPL-3.0-or-later. See [COPYING](COPYING).
