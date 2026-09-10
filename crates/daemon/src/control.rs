//! The charge control loop.
//!
//! [`ControlLoop::tick`] is a pure function over an [`Smc`], a [`Config`] and
//! the current time. It reads the battery state, then opens or closes the
//! charge gate. Nothing here touches the socket, the log or the clock, so the
//! tests drive it with [`smc::MockDriver`] and an explicit `now`.

use std::time::{Duration, Instant};

use proto::ChargeControlMode;
use smc::{Driver, FirmwareLimit, Smc, SmcError};

use crate::config::Config;

/// Time between loop ticks.
pub const LOOP_INTERVAL: Duration = Duration::from_secs(10);

/// A gap longer than this between ticks counts as a missed tick. The machine
/// was most likely asleep, so the battery state is stale.
pub const MISSED_TICK_AFTER: Duration = Duration::from_secs(90);

/// What a tick did. Reported in the log, never on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// The gate was already where it belongs.
    Sailing,
    /// The gate was closed because the battery reached `upper`.
    Inhibited,
    /// The gate was opened because the battery fell below `lower`.
    Allowed,
    /// Ticks were missed, so the gate was closed before re-evaluating.
    InhibitedAfterMissedTicks,
    /// The firmware limit was written.
    FirmwareSet { lower: u8, upper: u8 },
    /// The firmware limit was deactivated.
    FirmwareCleared,
    /// This Mac has no charge control.
    Unsupported,
    /// The gate was closed because the machine is about to sleep.
    InhibitedForSleep,
}

/// A sleep or wake notification from the system.
///
/// The daemon reacts to both in legacy mode only: firmware mode holds the
/// band in hardware, so the machine keeps the limit while it sleeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerEvent {
    /// The machine is about to sleep. The control loop stops running.
    WillSleep,
    /// The machine has woken. The battery reading is stale.
    DidWake,
}

/// Loop state that survives between ticks.
#[derive(Debug, Default)]
pub struct ControlLoop {
    last_tick: Option<Instant>,
    resume_after_wake: bool,
}

impl ControlLoop {
    /// Returns a loop that has never ticked.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns a loop whose previous tick was at `last_tick`.
    #[cfg(test)]
    pub fn with_last_tick(last_tick: Instant) -> Self {
        Self {
            last_tick: Some(last_tick),
            resume_after_wake: false,
        }
    }

    /// Reports whether the gate was closed for sleep and not yet re-checked.
    pub fn resume_after_wake(&self) -> bool {
        self.resume_after_wake
    }

    /// Runs one tick of the control loop and returns what it did.
    ///
    /// The caller records the error and calls again on the next interval:
    /// one failed SMC call must never stop the loop.
    pub fn tick<D: Driver>(
        &mut self,
        smc: &mut Smc<D>,
        config: &Config,
        now: Instant,
    ) -> Result<Action, SmcError> {
        let missed = self
            .last_tick
            .is_some_and(|last| now.duration_since(last) > MISSED_TICK_AFTER);
        self.last_tick = Some(now);

        match smc.charge_control_mode() {
            ChargeControlMode::Unsupported => Ok(Action::Unsupported),
            ChargeControlMode::Firmware => tick_firmware(smc, config),
            ChargeControlMode::Legacy => tick_legacy(smc, config, missed),
        }
    }

    /// Handles one sleep or wake notification and returns what it did.
    ///
    /// Both events are no-ops unless the Mac is in legacy mode with a live
    /// limit: firmware mode holds the band in hardware, and an upper of 100
    /// means the user turned the limit off.
    pub fn on_power_event<D: Driver>(
        &mut self,
        smc: &mut Smc<D>,
        config: &Config,
        event: PowerEvent,
        now: Instant,
    ) -> Result<Action, SmcError> {
        if smc.charge_control_mode() != ChargeControlMode::Legacy
            || config.upper >= proto::MAX_UPPER
        {
            return Ok(Action::Sailing);
        }
        match event {
            PowerEvent::WillSleep => self.on_will_sleep(smc, config),
            PowerEvent::DidWake => self.on_did_wake(smc, config, now),
        }
    }

    /// Closes the gate before sleep, so a sleeping Mac cannot overshoot.
    ///
    /// Below `lower` the gate stays open by default: the machine may need
    /// the charge, and the missed-tick guard closes the gate on the first
    /// tick after wake. Set `inhibit_on_sleep_always` to close it anyway.
    fn on_will_sleep<D: Driver>(
        &mut self,
        smc: &mut Smc<D>,
        config: &Config,
    ) -> Result<Action, SmcError> {
        if !smc.is_charging_allowed()? {
            return Ok(Action::Sailing);
        }
        let percent = smc.battery_percent()?;
        if percent < config.lower && !config.inhibit_on_sleep_always {
            return Ok(Action::Sailing);
        }
        smc.inhibit_charging()?;
        self.resume_after_wake = true;
        Ok(Action::InhibitedForSleep)
    }

    /// Re-evaluates the band as soon as the machine wakes.
    ///
    /// The missed-tick guard is skipped for this one tick: the gate state is
    /// known, because the daemon set it before sleep, so a fresh reading can
    /// be trusted at once.
    fn on_did_wake<D: Driver>(
        &mut self,
        smc: &mut Smc<D>,
        config: &Config,
        now: Instant,
    ) -> Result<Action, SmcError> {
        self.last_tick = None;
        let action = self.tick(smc, config, now);
        self.resume_after_wake = false;
        action
    }
}

/// Holds the band with the legacy gate and a hysteresis window.
fn tick_legacy<D: Driver>(
    smc: &mut Smc<D>,
    config: &Config,
    missed: bool,
) -> Result<Action, SmcError> {
    let allowed = smc.is_charging_allowed()?;

    // A limit of 100 means the user turned the limit off.
    if config.upper >= proto::MAX_UPPER {
        if !allowed {
            smc.allow_charging()?;
            return Ok(Action::Allowed);
        }
        return Ok(Action::Sailing);
    }

    // WARNING: after missed ticks the battery reading is stale and may
    // already be above the limit. Close the gate first and re-evaluate on
    // the next tick: an overshoot costs more than a late resume.
    if missed && allowed {
        smc.inhibit_charging()?;
        return Ok(Action::InhibitedAfterMissedTicks);
    }

    let percent = smc.battery_percent()?;
    if percent >= config.upper && allowed {
        smc.inhibit_charging()?;
        return Ok(Action::Inhibited);
    }
    if percent < config.lower && !allowed {
        smc.allow_charging()?;
        return Ok(Action::Allowed);
    }
    Ok(Action::Sailing)
}

/// Hands the band to the firmware. The SMC holds it, so never poll-toggle.
fn tick_firmware<D: Driver>(smc: &mut Smc<D>, config: &Config) -> Result<Action, SmcError> {
    let current = smc.firmware_limit()?;

    if config.upper >= proto::MAX_UPPER {
        if current.active {
            smc.clear_firmware_limit()?;
            return Ok(Action::FirmwareCleared);
        }
        return Ok(Action::Sailing);
    }

    let want = FirmwareLimit {
        active: true,
        lower: config.lower,
        upper: config.upper,
    };
    if current == want {
        return Ok(Action::Sailing);
    }
    smc.set_firmware_limit(config.lower, config.upper)?;
    Ok(Action::FirmwareSet {
        lower: config.lower,
        upper: config.upper,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use smc::{MockDriver, KEY_ACW, KEY_BFD0, KEY_BFE0, KEY_BFF0, KEY_BUIC, KEY_CH0B, KEY_CH0C};

    /// A legacy Mac at `percent`, plugged in, with the gate open or closed.
    fn legacy_mock(percent: u8, allowed: bool) -> MockDriver {
        let gate = if allowed { 0x00 } else { 0x02 };
        let mock = MockDriver::new();
        mock.seed(KEY_CH0B, &[gate])
            .seed(KEY_CH0C, &[gate])
            .seed(KEY_BUIC, &[percent])
            .seed(KEY_ACW, &[0x01]);
        mock
    }

    /// A firmware Mac with the limit off.
    fn firmware_mock() -> MockDriver {
        let mock = MockDriver::new();
        mock.seed(KEY_BFF0, &[0x00])
            .seed(KEY_BFD0, &[0, 0, 0, 0])
            .seed(KEY_BFE0, &[0, 0, 0, 0])
            .seed(KEY_BUIC, &[80])
            .seed(KEY_ACW, &[0x01]);
        mock
    }

    fn band(upper: u8, lower: u8) -> Config {
        Config {
            upper,
            lower,
            ..Config::default()
        }
    }

    /// Runs one tick with fresh timing and returns the action.
    fn tick(loop_: &mut ControlLoop, smc: &mut Smc<MockDriver>, config: &Config) -> Action {
        loop_.tick(smc, config, Instant::now()).unwrap()
    }

    /// Delivers one power event with fresh timing and returns the action.
    fn power(
        loop_: &mut ControlLoop,
        smc: &mut Smc<MockDriver>,
        config: &Config,
        event: PowerEvent,
    ) -> Action {
        loop_
            .on_power_event(smc, config, event, Instant::now())
            .unwrap()
    }

    #[test]
    fn legacy_holds_the_band() {
        let config = band(80, 78);

        // 79%, gate open: inside the band, so leave the gate alone.
        let mock = legacy_mock(79, true);
        let mut smc = Smc::new(mock.clone());
        assert_eq!(
            tick(&mut ControlLoop::new(), &mut smc, &config),
            Action::Sailing
        );
        assert!(mock.writes().is_empty());

        // 80%, gate open: at the limit, so close the gate.
        let mock = legacy_mock(80, true);
        let mut smc = Smc::new(mock.clone());
        assert_eq!(
            tick(&mut ControlLoop::new(), &mut smc, &config),
            Action::Inhibited
        );
        assert_eq!(
            mock.writes(),
            vec![
                (KEY_CH0B.to_string(), vec![0x02]),
                (KEY_CH0C.to_string(), vec![0x02]),
            ]
        );

        // 77%, gate closed: below the band, so open the gate.
        let mock = legacy_mock(77, false);
        let mut smc = Smc::new(mock.clone());
        assert_eq!(
            tick(&mut ControlLoop::new(), &mut smc, &config),
            Action::Allowed
        );
        assert_eq!(
            mock.writes(),
            vec![
                (KEY_CH0B.to_string(), vec![0x00]),
                (KEY_CH0C.to_string(), vec![0x00]),
            ]
        );

        // 79%, gate closed: inside the band, so leave the gate alone.
        let mock = legacy_mock(79, false);
        let mut smc = Smc::new(mock.clone());
        assert_eq!(
            tick(&mut ControlLoop::new(), &mut smc, &config),
            Action::Sailing
        );
        assert!(mock.writes().is_empty());
    }

    #[test]
    fn legacy_upper_100_opens_the_gate_once() {
        let config = band(100, 100);
        let mock = legacy_mock(60, false);
        let mut smc = Smc::new(mock.clone());
        let mut control = ControlLoop::new();

        assert_eq!(tick(&mut control, &mut smc, &config), Action::Allowed);
        assert_eq!(mock.writes().len(), 2);

        mock.clear_writes();
        assert_eq!(tick(&mut control, &mut smc, &config), Action::Sailing);
        assert!(mock.writes().is_empty());
    }

    #[test]
    fn firmware_writes_the_band_once() {
        let config = band(80, 78);
        let mock = firmware_mock();
        let mut smc = Smc::new(mock.clone());
        let mut control = ControlLoop::new();

        assert_eq!(
            tick(&mut control, &mut smc, &config),
            Action::FirmwareSet {
                lower: 78,
                upper: 80
            }
        );
        assert_eq!(
            mock.writes(),
            vec![
                (KEY_BFD0.to_string(), vec![80, 0, 0, 0]),
                (KEY_BFE0.to_string(), vec![78, 0, 0, 0]),
                (KEY_BFF0.to_string(), vec![0x02]),
            ]
        );

        // The limit already matches, so the loop must not poll-toggle it.
        mock.clear_writes();
        assert_eq!(tick(&mut control, &mut smc, &config), Action::Sailing);
        assert!(mock.writes().is_empty());

        // A limit of 100 clears the firmware band.
        mock.clear_writes();
        let off = band(100, 100);
        assert_eq!(tick(&mut control, &mut smc, &off), Action::FirmwareCleared);
        assert_eq!(mock.writes(), vec![(KEY_BFF0.to_string(), vec![0x00])]);

        mock.clear_writes();
        assert_eq!(tick(&mut control, &mut smc, &off), Action::Sailing);
        assert!(mock.writes().is_empty());
    }

    #[test]
    fn missed_ticks_close_the_gate_first() {
        let config = band(80, 78);
        let mock = legacy_mock(60, true);
        let mut smc = Smc::new(mock.clone());

        let now = Instant::now();
        let stale = now
            .checked_sub(Duration::from_secs(120))
            .expect("test host booted more than 120 s ago");
        let mut control = ControlLoop::with_last_tick(stale);

        assert_eq!(
            control.tick(&mut smc, &config, now).unwrap(),
            Action::InhibitedAfterMissedTicks
        );
        assert_eq!(
            mock.writes(),
            vec![
                (KEY_CH0B.to_string(), vec![0x02]),
                (KEY_CH0C.to_string(), vec![0x02]),
            ]
        );

        // The next tick has fresh timing, so the band applies again.
        mock.clear_writes();
        assert_eq!(tick(&mut control, &mut smc, &config), Action::Allowed);
        assert_eq!(
            mock.writes(),
            vec![
                (KEY_CH0B.to_string(), vec![0x00]),
                (KEY_CH0C.to_string(), vec![0x00]),
            ]
        );
    }

    #[test]
    fn unsupported_mac_does_nothing() {
        let mock = MockDriver::new();
        mock.seed(KEY_BUIC, &[50]);
        let mut smc = Smc::new(mock.clone());
        assert_eq!(
            tick(&mut ControlLoop::new(), &mut smc, &band(80, 78)),
            Action::Unsupported
        );
        assert!(mock.writes().is_empty());
    }

    #[test]
    fn smc_errors_reach_the_caller() {
        // A legacy Mac with no battery key: the read fails, the tick returns
        // the error, and the daemon keeps running.
        let mock = MockDriver::new();
        mock.seed(KEY_CH0B, &[0x00]).seed(KEY_CH0C, &[0x00]);
        let mut smc = Smc::new(mock);
        let result = ControlLoop::new().tick(&mut smc, &band(80, 78), Instant::now());
        assert!(matches!(result, Err(SmcError::KeyNotFound(_))));
    }

    /// Test 1: at 79% inside a live band, sleep closes the gate.
    #[test]
    fn will_sleep_closes_the_gate_inside_the_band() {
        let config = band(80, 78);
        let mock = legacy_mock(79, true);
        let mut smc = Smc::new(mock.clone());
        let mut control = ControlLoop::new();

        assert_eq!(
            power(&mut control, &mut smc, &config, PowerEvent::WillSleep),
            Action::InhibitedForSleep
        );
        assert_eq!(
            mock.writes(),
            vec![
                (KEY_CH0B.to_string(), vec![0x02]),
                (KEY_CH0C.to_string(), vec![0x02]),
            ]
        );
        assert!(control.resume_after_wake());
    }

    /// Test 2: below `lower` the default config leaves the gate open.
    #[test]
    fn will_sleep_leaves_the_gate_open_below_lower() {
        let config = band(80, 78);
        let mock = legacy_mock(60, true);
        let mut smc = Smc::new(mock.clone());
        let mut control = ControlLoop::new();

        assert_eq!(
            power(&mut control, &mut smc, &config, PowerEvent::WillSleep),
            Action::Sailing
        );
        assert!(mock.writes().is_empty());
        assert!(!control.resume_after_wake());
    }

    /// Test 3: `inhibit_on_sleep_always` closes the gate below `lower` too.
    #[test]
    fn will_sleep_always_closes_the_gate_when_configured() {
        let config = Config {
            inhibit_on_sleep_always: true,
            ..band(80, 78)
        };
        let mock = legacy_mock(60, true);
        let mut smc = Smc::new(mock.clone());
        let mut control = ControlLoop::new();

        assert_eq!(
            power(&mut control, &mut smc, &config, PowerEvent::WillSleep),
            Action::InhibitedForSleep
        );
        assert_eq!(
            mock.writes(),
            vec![
                (KEY_CH0B.to_string(), vec![0x02]),
                (KEY_CH0C.to_string(), vec![0x02]),
            ]
        );
        assert!(control.resume_after_wake());
    }

    /// Test 4: wake re-checks the band at once, whatever the tick clock says.
    #[test]
    fn did_wake_re_checks_the_band_at_once() {
        let config = band(80, 78);

        // Still inside the band, so the gate stays closed.
        let mock = legacy_mock(79, true);
        let mut smc = Smc::new(mock.clone());
        let mut control = ControlLoop::new();
        power(&mut control, &mut smc, &config, PowerEvent::WillSleep);
        mock.clear_writes();
        mock.seed(KEY_BUIC, &[79]);
        assert_eq!(
            power(&mut control, &mut smc, &config, PowerEvent::DidWake),
            Action::Sailing
        );
        assert!(mock.writes().is_empty());
        assert!(!control.resume_after_wake());

        // Below `lower` after the sleep, so the gate opens on wake.
        let mock = legacy_mock(79, true);
        let mut smc = Smc::new(mock.clone());
        let mut control = ControlLoop::new();
        power(&mut control, &mut smc, &config, PowerEvent::WillSleep);
        mock.clear_writes();
        mock.seed(KEY_BUIC, &[70]);
        assert_eq!(
            power(&mut control, &mut smc, &config, PowerEvent::DidWake),
            Action::Allowed
        );
        assert_eq!(
            mock.writes(),
            vec![
                (KEY_CH0B.to_string(), vec![0x00]),
                (KEY_CH0C.to_string(), vec![0x00]),
            ]
        );
        assert!(!control.resume_after_wake());
    }

    /// Test 4, continued: the wake tick ignores the missed-tick guard, so it
    /// never closes a gate the sleep hook already handled.
    #[test]
    fn did_wake_ignores_the_missed_tick_guard() {
        let config = band(80, 78);
        let mock = legacy_mock(60, false);
        let mut smc = Smc::new(mock.clone());

        let now = Instant::now();
        let stale = now
            .checked_sub(Duration::from_secs(3600))
            .expect("test host booted more than an hour ago");
        let mut control = ControlLoop::with_last_tick(stale);

        // A plain tick would close the gate first. The wake tick trusts the
        // fresh reading and opens it.
        assert_eq!(
            control
                .on_power_event(&mut smc, &config, PowerEvent::DidWake, now)
                .unwrap(),
            Action::Allowed
        );
        assert_eq!(
            mock.writes(),
            vec![
                (KEY_CH0B.to_string(), vec![0x00]),
                (KEY_CH0C.to_string(), vec![0x00]),
            ]
        );
    }

    /// Test 5: firmware Macs hold the band in hardware, so both events pass.
    #[test]
    fn firmware_mode_ignores_sleep_and_wake() {
        let config = band(80, 78);
        let mock = firmware_mock();
        let mut smc = Smc::new(mock.clone());
        let mut control = ControlLoop::new();

        for event in [PowerEvent::WillSleep, PowerEvent::DidWake] {
            assert_eq!(
                power(&mut control, &mut smc, &config, event),
                Action::Sailing
            );
            assert!(mock.writes().is_empty(), "{event:?} wrote to the SMC");
        }
    }

    /// Test 6: with the limit off there is nothing to protect.
    #[test]
    fn upper_100_ignores_sleep_and_wake() {
        let config = band(100, 100);
        let mock = legacy_mock(95, false);
        let mut smc = Smc::new(mock.clone());
        let mut control = ControlLoop::new();

        for event in [PowerEvent::WillSleep, PowerEvent::DidWake] {
            assert_eq!(
                power(&mut control, &mut smc, &config, event),
                Action::Sailing
            );
            assert!(mock.writes().is_empty(), "{event:?} wrote to the SMC");
        }
    }

    /// An SMC failure during a power event reaches the caller, which logs it.
    #[test]
    fn power_event_errors_reach_the_caller() {
        let mock = MockDriver::new();
        mock.seed(KEY_CH0B, &[0x00]).seed(KEY_CH0C, &[0x00]);
        let mut smc = Smc::new(mock);
        let result = ControlLoop::new().on_power_event(
            &mut smc,
            &band(80, 78),
            PowerEvent::WillSleep,
            Instant::now(),
        );
        assert!(matches!(result, Err(SmcError::KeyNotFound(_))));
    }
}
