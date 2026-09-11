# chargecap

`chargecap` is an internal, macOS-only battery charge limiter for MacBooks
that stay plugged in most of the time. It stops charging at 80% (or a custom
percentage) and restarts it a couple of points below that, to reduce battery
wear. A small root daemon, `chargecapd`, does the actual charge-control work
and needs `sudo` once during install, because writing to the SMC (the chip
that controls charging) is a root-only operation on macOS; the menu-bar app
itself never needs `sudo`.

## Install

```
scripts/install.sh
```

This builds the app, ad-hoc signs it, copies it to `/Applications`, installs
`chargecapd` as a LaunchDaemon (asks for your password once), and launches
the menu-bar app. Run it again any time to pick up a new build.

## Uninstall

```
scripts/uninstall.sh
```

Quits the app, re-enables charging, removes the daemon and the app, and
cleans up the launch-at-login agent. Your saved settings are kept:
`/Library/Application Support/chargecap/config.json` holds the daemon's
band, and `~/Library/Application Support/chargecap/app.json` holds the
limit that "Limit enabled" restores. Pass `--purge` to remove both.

## Using the menu

- **Limit presets** — pick 60%, 70%, 80%, 90% or 100% to stop charging at
  that level.
- **Custom...** — type any percentage from 50 to 100.
- **Limit enabled** — checkbox that toggles the limit off (charge to 100%)
  and back to your last chosen percentage.
- **Discharge to limit** — turns the charger off so the battery drains down
  to the limit, then turns it back on.
- **Top up to 100% once** — charges past the limit to 100% one time, then
  goes back to enforcing the limit.
- **MagSafe LED** — follow the system behaviour, force it off, or make it
  reflect the current charging state.
- **Launch at login** — installs or removes a per-user LaunchAgent so
  `chargecap` starts automatically.

The daemon binary is not on your `PATH` — it lives at
`/usr/local/libexec/chargecapd` once installed.

## Troubleshooting

- **Daemon not running** — run `sudo /usr/local/libexec/chargecapd install`
  again (or `scripts/install.sh`); the daemon logs why it stopped.
- **Where the log is** — `/Library/Logs/chargecap/daemon.log`, or use the
  menu's "Open log" item.
- **Check the daemon's own view** — `/usr/local/libexec/chargecapd status --json`.
- **Check the SMC directly** — `sudo cargo run -p smc --bin smcctl -- probe`
  from a checkout shows the charge-control mode and every key `chargecap`
  uses.
- **Mac stopped charging after uninstall** — run `scripts/uninstall.sh` again
  (it resets the SMC charge gate as part of removing the daemon), or reset
  the gate by hand from a checkout. Run `probe` first: it names the gate
  this Mac uses.
  - `CH0B` and `CH0C` present:
    `sudo cargo run -p smc --bin smcctl -- write CH0B 00 && sudo cargo run -p smc --bin smcctl -- write CH0C 00`
  - `CHTE` present instead (newer firmware, for example an M3 Pro on macOS
    15.7): `sudo cargo run -p smc --bin smcctl -- write CHTE 00000000`
  - `bfF0` present (firmware charge control):
    `sudo cargo run -p smc --bin smcctl -- write bfF0 00`

## Supported hardware

Apple Silicon Macs on macOS 14 or later. Intel Macs are not supported.

`chargecap` holds the limit with whichever charge gate the Mac exposes:

| Mode | Keys | Held by |
|---|---|---|
| legacy | `CH0B` + `CH0C`, or `CHTE` | the daemon, which opens and closes the gate |
| firmware | `bfF0`, `bfD0`, `bfE0` | the SMC, which keeps the band while asleep |

`cargo run -p smc --bin smcctl -- probe` prints the mode of the Mac you are
on. Sleep and wake hooks apply to legacy mode only.

`docs/VERIFICATION.md` records one end-to-end run on a real Mac: the test
plan, the result of each step, and the seams the pass found.

## Developer section

### Architecture

```
 chargecap (menu bar app, user)  <-- Unix socket -->  chargecapd (daemon, root)  <-->  SMC
        crates/app                  /var/run/chargecap.sock      crates/daemon        crates/smc
                          both share the wire contract in crates/proto
```

`chargecapd` runs as a root LaunchDaemon and is the only process that talks to
the SMC. `chargecap` is an unprivileged LaunchAgent that renders the menu-bar
UI and sends commands (set limit, toggle adapter, top-up, ...) to the daemon
over a Unix domain socket, using the JSON protocol defined in `crates/proto`.

### Build

```
cargo build --workspace
```

### Bundle

```
scripts/bundle.sh          # target/bundle/chargecap.app
```

### Dev run

```
sudo cargo run -p daemon --bin chargecapd -- daemon   # foreground daemon, needs sudo
cargo run -p app --bin chargecap                      # menu-bar app, no sudo
```

See `CLAUDE.md` for the crate layout and verify commands.
