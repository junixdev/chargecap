# Contributing to chargecap

Thank you for your interest. This page tells you how the code is organized,
how to build it, and how to send a change.

## Scope

chargecap is a macOS-only, pure-Rust menu-bar app for Apple Silicon Macs.
Do not add Windows, Linux, or Intel Mac code paths.

## Crates

The workspace has four crates under `crates/`:

| Crate | Binary | Role |
|---|---|---|
| `proto` | none | The wire contract (`Request` / `Response`) and the shared file paths: socket, config, log, plist. No behavior. Both binaries depend on it and must stay in lockstep with it. |
| `smc` | `smcctl` (dev tool) | Read and write access to the SMC (System Management Controller). |
| `daemon` | `chargecapd` | The root daemon. Talks to the SMC and serves the socket. Subcommands: `daemon`, `install`, `uninstall`, `status`, `limit <n>`. |
| `app` | `chargecap`, `fake-daemon` (dev tool) | The menu-bar app. Talks to `chargecapd` over its Unix socket. Never touches the SMC directly. |

## Architecture

```
 chargecap (menu bar app, user)  <-- Unix socket -->  chargecapd (daemon, root)  <-->  SMC
        crates/app                  /var/run/chargecap.sock      crates/daemon        crates/smc
                          both share the wire contract in crates/proto
```

`chargecapd` runs as a root LaunchDaemon. It is the only process that talks
to the SMC. `chargecap` is an unprivileged LaunchAgent. It renders the
menu-bar UI and sends commands (set limit, toggle adapter, top-up, ...) to
the daemon over a Unix domain socket. The protocol is newline-delimited JSON,
defined in `crates/proto`.

### Charge control modes

chargecap holds the limit with the charge gate the Mac exposes:

| Mode | Keys | Held by |
|---|---|---|
| legacy | `CH0B` + `CH0C`, or `CHTE` | the daemon, which opens and closes the gate |
| firmware | `bfF0`, `bfD0`, `bfE0` | the SMC, which keeps the band while asleep |

Run `cargo run -p smc --bin smcctl -- probe` to print the mode of your Mac.
Sleep and wake hooks apply to legacy mode only.

## Prerequisites

- An Apple Silicon Mac on macOS 14 or later.
- Rust, from [rustup](https://rustup.rs). `rust-toolchain.toml` pins the
  version.
- Xcode Command Line Tools: `xcode-select --install`.

## Build and check

Run these before you open a pull request. CI runs the same commands.

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
scripts/install.sh --dry-run
scripts/test-uninstall-dry-run.sh
```

One test is ignored by default. It needs a real `AppleSMC` service.

## Run from a checkout

`chargecapd` writes to `/var/run/chargecap.sock` and to
`/Library/LaunchDaemons/`. Running it for real (`daemon`, `install`,
`uninstall`) needs `sudo`. `status` and `limit` also talk to the socket under
`/var/run`, so they need `sudo` too. `chargecap` (the menu-bar app) does not
need `sudo`.

```sh
# Foreground daemon, needs sudo.
sudo cargo run -p daemon --bin chargecapd -- daemon

# Menu-bar app, no sudo. Run it in a second terminal.
cargo run -p app --bin chargecap
```

To work on the app without the daemon or the hardware, run the stand-in:

```sh
cargo run -p app --bin fake-daemon
CHARGECAP_SOCKET=/tmp/chargecap.sock cargo run -p app --bin chargecap
```

## Bundle and install

```sh
scripts/bundle.sh      # builds target/bundle/chargecap.app, ad-hoc signed
scripts/install.sh     # bundle + copy to /Applications + install daemon + launch
scripts/uninstall.sh   # quit, re-enable charging, remove daemon and app
```

Or use the `Makefile` targets: `build`, `bundle`, `install`, `uninstall`,
`test`.

## Debug on real hardware

- Daemon log: `/Library/Logs/chargecap/daemon.log`.
- Daemon view of the world: `sudo /usr/local/libexec/chargecapd status --json`.
- SMC keys the daemon uses: `sudo cargo run -p smc --bin smcctl -- probe`.

If a Mac stops charging after a failed uninstall, reset the gate by hand.
Run `probe` first. It names the gate this Mac uses.

| `probe` shows | Reset command |
|---|---|
| `CH0B` and `CH0C` | `sudo cargo run -p smc --bin smcctl -- write CH0B 00 && sudo cargo run -p smc --bin smcctl -- write CH0C 00` |
| `CHTE` | `sudo cargo run -p smc --bin smcctl -- write CHTE 00000000` |
| `bfF0` | `sudo cargo run -p smc --bin smcctl -- write bfF0 00` |

`docs/VERIFICATION.md` records one end-to-end run on a real Mac.

## Send a change

1. Open an issue first for large changes, so we can agree on the approach.
2. Fork the repo and create a branch from `main`.
3. Keep commits small. Use the `type(scope): summary` format, for example
   `fix(daemon): restore the band as soon as a top-up is cancelled`.
4. Add or update tests for behavior changes.
5. If you change `crates/proto`, update both binaries in the same pull
   request.
6. Run the checks listed above.
7. Open a pull request and fill in the template. Say which Mac and macOS
   version you tested on.

## Release

Maintainers cut a release by pushing a tag:

```sh
git tag v0.2.0
git push origin v0.2.0
```

The `Release` workflow builds the bundle, zips it, and attaches it to a
GitHub release. Bump `version` in every `crates/*/Cargo.toml` first.

## Code of conduct

This project follows the [Contributor Covenant](CODE_OF_CONDUCT.md).
