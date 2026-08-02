# Leyden

A small, native GNOME app that watches your laptop battery: state, capacity,
live power draw and intake, and how long a charge lasts.

Named after the [Leyden jar](https://en.wikipedia.org/wiki/Leyden_jar) (1745),
the first device that could store an electric charge.

Built with Rust, [relm4](https://relm4.org), GTK 4 and libadwaita — for one
laptop, running GNOME.

## Status

Early. The current build shows live battery state, charge, power, voltage,
health and cycle count, polled from `/sys/class/power_supply` every two seconds.
The charge/drain graph is next.

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
