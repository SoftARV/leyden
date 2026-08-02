# CLAUDE.md

Project instructions for Claude Code. Read this fully before writing code.

## What this is

**Leyden** — a small, native GNOME desktop app that watches the battery of the
laptop it runs on: state, capacity, live power draw/intake, and a graph of how
long a charge lasts and how long a charge takes. One user, one machine. Not a
product, not multi-user, not cross-platform.

The name is the **Leyden jar** (1745), the first device that could store an
electric charge — the ancestor of the battery.

It is the sibling of **Pitwall** (GitHub Actions monitor) and **Slipmat** (Apple
Music client) and shares their stack and taste, but **shares no code with
them** — every component here is written for this app.

The app should be indistinguishable from a first-party GNOME application. If a
design decision would make it look like an Electron app or a generic Qt tool, it
is the wrong decision.

## Author context — read this, it changes how you should respond

The author is a senior frontend engineer (~10 years: TypeScript, React, React
Native, Node) who is **new to Rust**. Consequences:

- When you introduce ownership, borrowing, lifetimes, `Rc`/`Arc`/`RefCell`, or
  `async` pinning, **briefly explain why in your reply** — not in the code (see
  the comment rule below). Do not silently sprinkle `.clone()` to quiet the
  borrow checker.
- Analogies to React/Redux land well. relm4 *is* the Elm architecture; say so.
- Do not dumb down the Rust. Idiomatic code, explained in chat.
- Prefer clarity over cleverness. No macro tricks, no premature generics.

## Hard rules

### 1. Comments are kept to an absolute minimum

This is the one convention that differs from the sibling apps. A module-level
`//!` doc saying what the file is for, and a short comment **only** where the
code cannot explain itself — a hardware quirk, a unit conversion, a
non-obvious GTK/Wayland constraint. No comment that restates the line below it.
Explanations belong in the chat reply and in this file, not in the source.

### 2. The two sysfs gauge flavours are the whole data layer

A laptop has exactly one of them and the app must handle both:

| Gauge  | Charge                | Full                 | Rate          | Units    |
| ------ | --------------------- | -------------------- | ------------- | -------- |
| Energy | `energy_now`          | `energy_full`        | `power_now`   | µWh / µW |
| Charge | `charge_now`          | `charge_full`        | `current_now` | µAh / µA |

A charge gauge is converted with `voltage_now`: `Wh = µAh / 1e6 × V` and
`W = µA / 1e6 × V`. **The author's own machine (Razer Blade, BAT0) is a charge
gauge with no `power_now`**, so the conversion path is the one that gets
exercised daily — but most Intel laptops are energy gauges, so neither branch
may rot. Everything above `battery/sysfs.rs` sees Wh, W and V only.

Other quirks worth knowing: some drivers sign `current_now` by direction (power
is stored as a magnitude; `status` carries the direction), `technology` is often
the literal string `Unknown`, and a missing file is normal, not an error — it
becomes `None` and renders as `—`.

### 3. Never block the GTK main thread

sysfs reads are local file reads in the microseconds, so — unlike the
network-bound siblings — they run **inline in `update()`**, with no relm4
`Command`. That is a deliberate exception, and it holds only for sysfs. Anything
slower (D-Bus, upower, writing history to disk) goes through a `Command`.

### 4. Poll cheaply and stop when hidden

This app monitors battery drain; it must not be a cause of it. `glib::timeout`
at `POLL_SECS` (2s), and the timer is **removed entirely** while the window is
hidden (`is_suspended`), then re-sampled on return so nothing stale is shown.
Never add a second timer — `start_poll` is idempotent for that reason.

### 5. No `.unwrap()` / `.expect()` outside `main.rs` and tests

Hardware disappears, files vanish between reads, values parse to nothing. Every
missing value is an `Option` that renders as `—`, never a panic.

### 6. Use libadwaita widgets, not raw GTK

`adw::ActionRow`, `adw::PreferencesGroup`, `adw::StatusPage`, `adw::AboutDialog`.
That is where the native feel comes from. No custom CSS unless there is no
libadwaita widget for the job — say why before adding any. The graph will be the
first real exception (a `gtk::DrawingArea`), and it must follow the Adwaita
accent/foreground colours rather than hard-coded ones.

## Stack (pinned — do not swap these out)

| Layer        | Crate                | Version                            |
| ------------ | -------------------- | ---------------------------------- |
| UI framework | `relm4`              | 0.11 (features: `libadwaita`, `gnome_49`) |
| Widgets      | `gtk4`, `libadwaita` | via relm4 (do **not** add directly) |
| Logging      | `tracing`            | 0.1                                |

Rust edition 2024, toolchain ≥ 1.93 (relm4 0.11's MSRV); libadwaita ≥ 1.8 /
GTK ≥ 4.20 (the `gnome_49` floor). There is deliberately **no async runtime, no
HTTP client and no D-Bus dependency** — the data source is the filesystem. Adding
one is a real decision; ask first.

**relm4 0.11's docs.rs build is broken.** Read the vendored source, which is the
exact version we compile against:

```bash
ls ~/.cargo/registry/src/*/relm4-0.11.0/src/
```

**relm4, not raw gtk4-rs.** Every component is a relm4 `Component` or
`FactoryComponent`. Reaching for `Rc<RefCell<>>` to share widget state is a sign
the state belongs in a model and the change belongs in an `update()`.

## Architecture

```
src/
  main.rs              # RelmApp bootstrap, tracing, icon
  app.rs               # root Component: AppModel, AppMsg, update, view
  format.rs            # value -> label; every "—" in the UI starts here
  graph.rs             # History -> Series -> cairo on a gtk::DrawingArea
  battery/
    mod.rs
    sysfs.rs           # /sys/class/power_supply -> Battery. Units resolved here.
    types.rs           # our Battery / Status (+ health, time_remaining)
    history.rs         # session sample ring: the graph's data, GAP_SECS, and
                       #   recent_power (the smoothed rate the estimates use)
data/
  dev.miguelrincon.Leyden.desktop
  icons/hicolor/scalable/apps/dev.miguelrincon.Leyden.svg
  icons/hicolor/symbolic/apps/dev.miguelrincon.Leyden-symbolic.svg
Makefile               # make install -> ~/.local (no sudo); make check
```

Dependency direction is strictly one-way: `main -> app -> battery/*`, and
`app -> {format, graph} -> battery/*`. `battery/` never imports gtk.

The graph takes an **owned snapshot** (`Series::from_history`) rebuilt on each
`#[watch]` pass, so the cairo draw closure needs no `Rc<RefCell<>>` — copying
~900 points every two seconds is far cheaper than shared mutable state.

```rust
struct AppModel {
    battery: Option<Battery>,     // None on a desktop -> StatusPage
    history: History,             // ring of samples; the graph reads this
    poll: Option<glib::SourceId>, // None while the window is hidden
}

enum AppMsg {
    Tick,                    // poll fired -> re-read sysfs, push a sample
    SuspendedChanged(bool),  // window hidden/shown -> gate the timer
    ShowAbout,
}
```

This is Redux with a compiler: actions in, one reducer, view derived from state.

## UI shape

- `adw::ApplicationWindow` > `adw::ToolbarView` > `adw::HeaderBar`, opening tall
  and narrow (440×700) — it is a glanceable panel, not a workspace.
- Header: `adw::WindowTitle` ("Leyden" + the battery's make/model) and the
  primary menu (About, Quit).
- Content: an `adw::Clamp` (520) in a `gtk::ScrolledWindow` holding a hero block
  — status icon, the percentage in `.title-1 .numeric`, the headline line
  ("Charging · 1 h 12 min until full"), a `gtk::LevelBar` — then
  `adw::PreferencesGroup`s: **History** (the graph, described by the span it
  covers), **Power** (draw/intake with the trailing average as its subtitle,
  charge, voltage, source) and **Health** (capacity vs design, cycles,
  technology).
- No battery: an `adw::StatusPage`. Missing value: `—`, never a blank or a zero.

## Scope

Issue-driven cadence — one small vertical slice, one PR each.

- ✅ **M1** — Scaffold: sysfs read (both gauge flavours), live state / capacity /
  power / voltage / health / cycles, 2s poll with the suspend gate, icon +
  `.desktop` + installer, About.
- ✅ **M2** — The graph: a `gtk::DrawingArea` over `History`, percentage against
  wall-clock time, the line coloured green while charging and accent while
  discharging. Samples are `SystemTime`-stamped (an `Instant` stands still
  across a suspend, which would have drawn an overnight sleep as a vertical
  drop), and a jump over `GAP_SECS` **breaks the line** rather than inventing a
  slope — that gap is real, either a suspend or a hidden window with the timer
  off. The x axis auto-fits the samples held, so the plot fills the width from
  the first minute instead of creeping in from the right.
- ✅ **M3** — Estimates that beat the instantaneous reading. `Battery::
  time_remaining` now **takes the rate in watts** instead of reading
  `self.power`, and the caller passes `History::recent_power(SMOOTH_SECS)` — the
  mean over the trailing 2 minutes. That average walks back from the newest
  sample and stops at anything that would poison it: a different `Status` (a
  charge rate says nothing about a drain rate) or a `GAP_SECS` gap (samples
  either side of a suspend are not neighbours in time). It falls back to the live
  reading until there is history to average. The Power row keeps showing the
  live watts and gains the average as a subtitle, but only once the two differ.
- **M4** — Persistence: keep history across launches (and therefore across
  suspend) so the graph is useful the moment the window opens.
- **M5** — Polish: preferences (poll interval, theme), keyboard shortcuts,
  optional notifications at low charge / full charge.

**Stay lean — flag the drift, don't gatekeep.** Not the default focus: charge
thresholds and other *writes* to the hardware, per-process power attribution
(that is Power Statistics' job), CPU/thermal monitoring, tray icons, other
machines' batteries. The app *watches one battery*. When a change drifts that
way, name the cost and the direction so it is a conscious choice.

## Commands

```bash
cargo run                  # dev
RUST_LOG=leyden=debug cargo run
make check                 # fmt + clippy -D warnings + test — the bar before any commit
make install               # ~/.local, no sudo
```

System deps (CachyOS / Arch):

```bash
sudo pacman -S --needed base-devel pkgconf rust gtk4 libadwaita
```

## Conventions

- `cargo clippy --all-targets -- -D warnings` is the bar, not `cargo build`.
- **The author does every commit.** Never run `git commit`, `git push`, or
  `gh`. When work is ready, give one conventional-commit message (`feat:`,
  `fix:`, `refactor:`, `chore:`) and stop.
- **Licence: GPL-3.0-or-later.** Full text in `COPYING`. Every source file
  carries the two-line SPDX header.
- App ID: `dev.miguelrincon.Leyden` — it must match the `.desktop` file name and
  `RelmApp::new()`. The app is called **Leyden** in the window title and
  `.desktop` `Name=`.
- Versioning: SemVer in `Cargo.toml`; `main` carries a `-dev` pre-release; tags
  are annotated `vX.Y.Z`.

## When you're unsure

Ask before: adding a dependency, introducing a new module, or deviating from the
relm4 component model. Don't ask before: fixing a clippy lint or checking the
vendored relm4 source.
