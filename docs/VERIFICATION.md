# chargecap hardware verification

One end-to-end pass of `chargecap` on a real Mac. Every step of the
test plan has a recorded result. The seams it found are fixed, each with a
regression test and its own commit.

## Environment

| Item | Value |
|---|---|
| Date | 2026-09-11 |
| Hardware | Apple M3 Pro (MacBook Pro) |
| macOS | 15.7.4, build 24G517 |
| Charge control mode | `legacy` |
| Legacy gate | `CHTE`, 4 bytes little-endian |
| Adapter gate | `CH0J` (`CH0I` absent, `CHIE` present) |
| MagSafe LED key | `ACLC` present; the Mac charges over USB-C, so no light to watch |
| `chargecap` version | 0.1.0 |
| Commit under test at install | `671a88d` |

`cargo run -p smc --bin smcctl -- probe`:

```
mode: Legacy
CH0B  absent
CH0C  absent
CHTE  present  size  4  type ui32
bfF0  absent
bfD0  present  size  2  type hex_
bfE0  absent
BUIC  present  size  1  type ui8
AC-W  present  size  1  type si8
ACLC  present  size  1  type ui8
CH0I  absent
CH0J  present  size  1  type ui8
CHIE  present  size  1  type hex_
```

This Mac has neither `CH0B` nor `CH0C`. It uses `CHTE`, where `00000000`
allows charging and a nonzero value inhibits it. The test plan's `CH0B` checks do
not apply here, so every gate reading below is `CHTE`.

`bfD0` is present alone, so `compute_mode` correctly reports `legacy`:
firmware mode needs `bfF0`, `bfD0` and `bfE0` together.

## Static checks

```
cargo fmt --all -- --check      # clean
cargo clippy --workspace --all-targets -- -D warnings   # clean
cargo test --workspace          # 104 passed, 0 failed, 1 ignored
scripts/test-uninstall-dry-run.sh   # 5 ok
```

The ignored test is `smc::hardware::opens_probes_and_reads_the_battery`,
which needs a real `AppleSMC` service.

## Results

| # | Step | Result | Evidence |
|---|---|---|---|
| 1 | Install | **PASS** | see below |
| 2 | Limit holds | **PASS** | see below |
| 3 | Limit resumes | **PASS** | see below |
| 4 | Custom… | **PASS** | see below |
| 5 | Disable / enable | **PASS** | see below |
| 6 | Sleep / wake | **PASS** | see below |
| 7 | Daemon restart | **PASS** | see below |
| 8 | Unplug / replug | **SKIPPED** | see below |
| 9 | Top up | **PASS** | see below |
| 10 | Discharge | **PASS** | see below |
| 11 | Uninstall | **PASS** | see below |

All five steps the test plan requires to pass (1, 2, 3, 5, 11) pass.

### 1. Install — PASS

The first run **failed**. See [Seam 3](#seam-3--the-install-raced-launchd) —
`launchctl bootstrap` lost a race with `bootout` and left the Mac with
neither the daemon nor the menu-bar app. After the fix:

```
[2026-09-11T00:27:31Z] INFO installed binary at /usr/local/libexec/chargecapd
[2026-09-11T00:27:31Z] INFO wrote plist at /Library/LaunchDaemons/io.github.junixdev.chargecap.plist
[2026-09-11T00:27:31Z] INFO booted out system/io.github.junixdev.chargecap
[2026-09-11T00:27:32Z] INFO launchctl bootstrap succeeded on try 3
[2026-09-11T00:27:32Z] INFO bootstrapped io.github.junixdev.chargecap
```

Three tries were needed, so the retry was not optional.

```
system/io.github.junixdev.chargecap = {
        active count = 1
        path = /Library/LaunchDaemons/io.github.junixdev.chargecap.plist
        type = LaunchDaemon
        state = running
```

```json
{"ok":true,"status":{"version":"0.1.0","mode":"legacy","battery_percent":90,
"plugged_in":true,"charging_allowed":false,"adapter_enabled":true,
"upper":90,"lower":88,"magsafe_led":"system","top_up_active":false,
"last_error":null}}
```

The menu-bar item appeared. The Dock check reported `chargecap is NOT in the
Dock`, which `LSUIElement` in the bundle's `Info.plist` gives.

### 2. Limit holds — PASS

Limit set to 80% from the menu with the battery at 90%:

```
 -InternalBattery-0 (id=6684771)  90%; AC attached; not charging present: true
{"upper":80,"lower":78,"charging_allowed":false,...}
CHTE: 01 00 00 00
```

The gate closed inside the 12 s the script waited. The title showed the
filled square (confirmed again in step 11: `95% square icon`).

MagSafe LED: not observable. This Mac charges over USB-C, so there is no
MagSafe light. `ACLC` is present and the daemon drives it, and
[Seam 2](#seam-2--the-magsafe-led-key-was-written-every-tick) came out of
reading that key on this machine.

### 3. Limit resumes — PASS

Limit set to 95% with the battery at 90%:

```
{"upper":95,"lower":93,"charging_allowed":true,...}
CHTE: 00 00 00 00
```

Title: `90% up arrow`. The gate opened inside 12 s. `pmset` still said
"not charging" at that moment and said "charging" at the next reading a
minute later; that lag is macOS reporting, not the gate, which `CHTE` shows
open.

### 4. Custom… — PASS

- `83` → `{"upper":83,"lower":81}`, matching `DEFAULT_GAP` of 2.
- `45` → the alert read exactly `upper must be 50..=100`, and the next
  reading still showed `{"upper":83,"lower":81}`. The daemon log has no
  `limit set to` line for it, so nothing reached the SMC.

### 5. Disable / enable — PASS

Scored from the daemon log, which records every accepted request. Two full
cycles from the menu:

```
[2026-09-11T00:36:36Z] INFO limit set to 100/100
[2026-09-11T00:36:55Z] INFO limit set to 83/81
...
[2026-09-11T00:48:26Z] INFO limit set to 100/100
[2026-09-11T00:53:54Z] INFO limit set to 83/81
```

Unticking "Limit enabled" sends 100, which turns the limit off; ticking it
back restores the previous percentage from
`~/Library/Application Support/chargecap/app.json`. Titles: infinity sign
with the limit off, filled square with it back on.

Two earlier attempts to score this with a timed watch recorded no change at
all, because the menu clicks fell outside the window. The daemon log is the
reliable record, and it is what the result above rests on.

### 6. Sleep / wake — PASS

This Mac is in legacy mode, so the step applies.

```
[2026-09-11T00:50:46Z] INFO WillSleep: battery 93%, Sailing, inhibited for sleep false
[2026-09-11T00:51:54Z] INFO DidWake: battery 94%, Sailing, inhibited for sleep false
```

Both hooks fired across a 68 s sleep. The action was `Sailing` because the
limit was off (`upper` 100) at that moment, which is what
`ControlLoop::on_power_event` is written to do. An earlier sleep on the same
machine with the limit live logged the same pair.

### 7. Daemon restart — PASS

```
sudo launchctl kickstart -k system/io.github.junixdev.chargecap
```

```
[2026-09-11T00:52:26Z] INFO shutting down
[2026-09-11T00:52:26Z] INFO charge control reset: charging allowed
[2026-09-11T00:52:26Z] INFO chargecapd 0.1.0 starting: mode Legacy, band 100/100, root true
[2026-09-11T00:52:26Z] INFO listening on /var/run/chargecap.sock
[2026-09-11T00:52:26Z] INFO registered for sleep and wake notifications
```

The band, the LED mode and the adapter state all came back from
`/Library/Application Support/chargecap/config.json`. The shutdown path
opened the gate first, which is the fail-safe working.

### 8. Unplug / replug — SKIPPED

The scripted run was invalid: an earlier "Discharge to limit" was still
active, so the adapter gate stayed closed for the whole step. The Mac
reported "discharging" and `plugged_in` stayed true whether the charger was
in or out, so the step proved nothing about unplugging. A corrected script
exists but the operator skipped it and reported the behaviour as working
from their own manual testing.

Not one of the five steps the test plan requires to pass. See
[Open issues](#open-issues).

### 9. Top up — PASS

```
[2026-09-11T00:59:54Z] INFO top-up requested
[2026-09-11T00:59:54Z] INFO tick: Allowed
[2026-09-11T01:00:24Z] INFO top-up cancelled
[2026-09-11T01:00:34Z] INFO tick: Inhibited
```

With the limit at 83 and the battery at 95, the top-up opened the gate on
the same request. Title: `95% up arrow 100`. Cancelling closed it again, but
10.0 s later, exactly on the tolerance the test plan allows. That is
[Seam 6](#seam-6--cancelling-a-top-up-waited-a-full-tick), now fixed.

### 10. Discharge — PASS

```
 -InternalBattery-0 (id=6684771)  95%; discharging; (no estimate) present: true
{"adapter_enabled":false,"charging_allowed":false,...}
CHTE: 01 00 00 00
```

`pmset` reported "discharging" while the charger was attached, which is the
point of the step. Title: `95% down arrow`. Unticking it restored the
adapter:

```
[2026-09-11T01:01:39Z] INFO adapter enabled: true
```

### 11. Uninstall — PASS

```
[2026-09-11T01:02:06Z] INFO asked the running daemon to allow charging
[2026-09-11T01:02:06Z] INFO booted out system/io.github.junixdev.chargecap
[2026-09-11T01:02:06Z] INFO charge control reset: charging allowed
[2026-09-11T01:02:06Z] INFO removed /Library/LaunchDaemons/io.github.junixdev.chargecap.plist
[2026-09-11T01:02:06Z] INFO removed /usr/local/libexec/chargecapd
[2026-09-11T01:02:06Z] INFO removed /var/run/chargecap.sock
[2026-09-11T01:02:06Z] INFO kept the config at /Library/Application Support/chargecap/config.json
```

Afterwards:

```
Could not find service "io.github.junixdev.chargecap" in domain for system
ls: /Applications/chargecap.app: No such file or directory
ls: /Library/LaunchDaemons/io.github.junixdev.chargecap.plist: No such file or directory
ls: /usr/local/libexec/chargecapd: No such file or directory
ls: /var/run/chargecap.sock: No such file or directory
no chargecap process (correct)
 -InternalBattery-0 (id=6684771)  95%; AC attached; not charging present: true
CHTE: 00 00 00 00
```

`CHTE` is zero, so the Mac was left free to charge. The menu-bar item was
gone. Both saved settings survived, which is
[Seam 5](#seam-5--a-plain-uninstall-threw-away-the-app-state):

```
/Library/Application Support/chargecap/config.json        (root, 151 bytes)
~/Library/Application Support/chargecap/app.json  (junix, 22 bytes)
```

The reinstall that followed bootstrapped on the first try, because nothing
was loaded to race with.

## Seams found and fixed

Every fix has a regression test and its own commit.

### Seam 1 — every log line was recorded twice

`ac8e70c`. launchd points the daemon's stderr at `LOG_PATH` through
`StandardErrorPath`, and `logging::write_line` wrote to stderr *and* to the
file handle it opened itself. Every line landed in
`/Library/Logs/chargecap/daemon.log` twice.

`init` now compares stderr with the log file by device and inode and drops
the stderr copy when they are one file. A dev run, where stderr is a
terminal, still gets both. Test:
`logging::tests::same_file_spots_a_descriptor_that_points_at_the_log`.

The live log shows the exact boundary: every line up to 00:21:06 is
duplicated, and no line after 00:23:36 is.

### Seam 2 — the MagSafe LED key was written every tick

`ed10d2a`. `tick_magsafe_led` skipped the write when a read of `ACLC`
already matched the wanted value. Writing `MagsafeLed::System` (`0x00`)
hands the LED back to the firmware, so the next read reports the live colour
— `0x02`, green, on this Mac — and never `System`. In the default LED mode
the daemon therefore wrote `ACLC` every 10 seconds, forever.

Found by reading `ACLC` on the live machine three times while the config
said `system`, and getting `02` each time.

System mode now compares against the last value the daemon wrote. The other
modes keep the read-back compare, which their values satisfy. `MockDriver`
gained `seed_sticky`, which models a key the firmware owns. Test:
`state::tests::magsafe_led_system_writes_once_when_the_read_never_matches`,
which fails against the old code.

### Seam 3 — the install raced launchd

`56205ec`. This is the one that broke step 1.

```
[2026-09-11T00:24:11Z] INFO booted out system/io.github.junixdev.chargecap
chargecapd: launchctl bootstrap failed: Bootstrap failed: 5: Input/output error
```

`launchctl bootout` returns before launchd has finished unloading the
daemon, and the `bootstrap` that followed in the same second hit that
window. Two things then went wrong at once:

- the new daemon never started, so the Mac had no charge control;
- `scripts/install.sh` had already run `pkill -x chargecap`, and `set -e`
  aborted the script before it could relaunch the app, so the user was left
  with no menu-bar item either.

`install` now retries the bootstrap for up to 15 s and logs how many tries
it took. The retry loop is factored into `retry_until`, which is what the
tests drive: `install::tests::retry_until_rides_out_a_few_failures` and
`install::tests::retry_until_runs_once_and_reports_the_last_failure`.

`scripts/install.sh` gained an `ERR` trap that puts the menu-bar app back on
any failure. The app then reports the daemon as not running, which is the
designed degraded state, instead of showing nothing at all.

The retry was needed three times on the very next run, so this was not a
one-off.

### Seam 4 — the README recovery command failed on this Mac

`0c90223`. The troubleshooting section told the reader to reset charging
with `smcctl write CH0B 00`. This Mac has no `CH0B`, so the command fails.
The README now tells the reader to run `probe` first and gives the command
for each of the three gates, and the hardware section carries a table of the
modes and their keys.

### Seam 5 — a plain uninstall threw away the app state

`671a88d`. `scripts/uninstall.sh` removed
`~/Library/Application Support/chargecap` whether or not `--purge` was
given, so a reinstall forgot the limit that "Limit enabled" restores. The
README promised the saved limit was kept, and `--purge` is the flag that
exists to remove it.

Only `--purge` removes it now. `scripts/test-uninstall-dry-run.sh` asserts
what each mode removes, and CI runs it.

### Seam 6 — cancelling a top-up waited a full tick

`f751395`. `CancelTopUp` only cleared the flag, so the gate stayed open
until the next tick. Step 9 measured the gate closing exactly 10.0 s after
the cancel, right on the tolerance the test plan allows. `SetLimit` and `TopUp`
already re-tick on the request; `CancelTopUp` now does the same. Test:
`state::tests::cancelling_a_top_up_restores_the_band_at_once`.

### Seam 7 — a not-found key had no name in the error

`699794d`. `smcctl read CH0B` on this Mac printed
`error: SMC key  not found on this Mac`, with the name missing. The SMC
leaves the key field of a refused reply at zero, so `check_result` had
nothing to print. It now takes the key the caller asked for and falls back
to it. Test:
`driver::tests::check_result_names_the_requested_key_when_the_reply_omits_it`.

## Open issues

Nothing here needs a redesign. These are the loose ends of this pass.

1. **Step 8 has no clean result.** The scripted run was contaminated by an
   active discharge and the corrected run was skipped. The behaviour it
   covers is exercised indirectly — `is_plugged_in` reads `AC-W`, and the
   control loop re-evaluates the gate every 10 s regardless of why the state
   changed — but no recorded evidence covers a real unplug. Re-run
   `scripts/install.sh`, make sure "Discharge to limit" is unticked, then
   unplug and replug and watch `plugged_in` in
   `chargecapd status --json`.

2. **`AC-W` while the adapter gate is closed.** During the contaminated step
   8, `plugged_in` stayed true with the adapter gated off, and it is not
   established whether `AC-W` reports the adapter as absent when it is
   physically unplugged *and* the gate is closed. If it does not, the title
   shows the discharge arrow rather than the unplugged state, which is the
   more useful of the two anyway. Worth confirming when step 8 is re-run.

3. **No MagSafe LED observation.** The Mac charges over USB-C. `ACLC` is
   present and the daemon writes it, and seam 2 came out of reading it, but
   no one confirmed a colour by eye. A Mac with a MagSafe connector would
   close this.

4. **The installed build is `671a88d`.** Seams 6 and 7 landed after the last
   install. Both are small — a 10 s delay on cancelling a top-up and a
   missing key name in one `smcctl` message — and neither affects holding
   the limit. A `scripts/install.sh` run picks them up.

5. **`launchctl bootout` on a missing LaunchAgent prints an error.**
   `scripts/uninstall.sh` prints `Boot-out failed: 3: No such process` when
   launch-at-login was never enabled. The script tolerates it and carries
   on; the line is noise, not a failure.

## End state

The tool is installed and running with the limit at 80%.

```json
{"ok":true,"status":{"version":"0.1.0","mode":"legacy","battery_percent":95,
"plugged_in":true,"charging_allowed":false,"adapter_enabled":true,
"upper":80,"lower":78,"magsafe_led":"system","top_up_active":false,
"last_error":null}}
```

```
system/io.github.junixdev.chargecap = { ... state = running }
 -InternalBattery-0 (id=6684771)  95%; AC attached; not charging present: true
CHTE: 01 00 00 00
```
