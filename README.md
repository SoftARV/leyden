# Leyden

A small, native GNOME app that watches your laptop battery: state, capacity,
live power draw and intake, and how long a charge lasts.

Named after the [Leyden jar](https://en.wikipedia.org/wiki/Leyden_jar) (1745),
the first device that could store an electric charge.

Built with Rust, [relm4](https://relm4.org), GTK 4 and libadwaita — for one
laptop, running GNOME.

<p align="center">
  <img src="docs/screenshots/overview.png" width="420"
       alt="Leyden showing a battery at 32%, discharging with 1 h 12 min left, drawing 17.1 W">
</p>

## Status

Early. The current build shows live battery state, charge, power, voltage,
health and cycle count, polled from `/sys/class/power_supply` every two seconds,
plus a graph of the charge over the session — green while charging, accent while
draining. History is not persisted yet, so the graph starts empty each launch.

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
