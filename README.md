# chargecap

`chargecap` is an internal, macOS-only, pure-Rust menu-bar app that limits
MacBook battery charging to 80% (or a custom value), to reduce battery wear
on machines that stay plugged in most of the time.

## Architecture

```
 chargecap (menu bar app, user)  <-- Unix socket -->  chargecapd (daemon, root)  <-->  SMC
        crates/app                  /var/run/chargecap.sock      crates/daemon        crates/smc
                          both share the wire contract in crates/proto
```

`chargecapd` runs as a root LaunchDaemon and is the only process that talks to
the SMC. `chargecap` is an unprivileged LaunchAgent that renders the menu-bar
UI and sends commands (set limit, toggle adapter, top-up, ...) to the daemon
over a Unix domain socket, using the JSON protocol defined in `crates/proto`.

## Build

```
cargo build --workspace
```

## Dev run

```
sudo cargo run -p daemon --bin chargecapd -- daemon   # foreground daemon, needs sudo
cargo run -p app --bin chargecap                      # menu-bar app, no sudo
```

See `CLAUDE.md` for the crate layout and verify commands.
