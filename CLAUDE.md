# chargecap

macOS-only, pure-Rust menu-bar app that caps MacBook battery charging at 80%
(or a custom value). Apple Silicon only — do not add Windows/Linux code paths.

## Crates

- `crates/proto` — the wire contract (`Request`/`Response`) and shared file
  paths (socket, config, log, plist). No behavior; both binaries depend on it
  and must stay in lockstep with it.
- `crates/smc` — SMC (System Management Controller) read/write access.
  Currently a placeholder; the real implementation lands in a later card.
- `crates/daemon` — bin `chargecapd`, the root daemon. Talks to the SMC and
  serves `proto`'s socket. Subcommands: `daemon`, `install`, `uninstall`,
  `status`, `limit <n>`.
- `crates/app` — bin `chargecap`, the menu-bar app. Talks to `chargecapd` over
  its Unix socket; never touches the SMC directly.

## Build / test

```
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

## Running

`chargecapd` writes to `/var/run/chargecap.sock` and
`/Library/LaunchDaemons/...`, so running it for real (`daemon`, `install`,
`uninstall`) needs `sudo`. `status`/`limit` also talk to the daemon's socket
under `/var/run`, so they need `sudo` too until the daemon is installed with
non-root-readable permissions. `chargecap` (the menu-bar app) does not need
`sudo`.
