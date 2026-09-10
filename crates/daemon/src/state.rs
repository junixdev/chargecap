//! Daemon state: the SMC handle, the config and the control loop.
//!
//! [`Daemon`] is the one place that mutates state. The loop thread and every
//! socket connection reach it through the same `Mutex`, so a request and a
//! tick can never interleave.

use std::path::{Path, PathBuf};
use std::time::Instant;

use proto::{ChargeControlMode, MagsafeLedMode, Request, Response, Status};
use smc::{Driver, MagsafeLed, Smc};

use crate::config::Config;
use crate::control::{Action, ControlLoop, PowerEvent};
use crate::logging;

/// Everything one running daemon owns.
#[derive(Debug)]
pub struct Daemon<D: Driver> {
    smc: Smc<D>,
    config: Config,
    control: ControlLoop,
    config_path: PathBuf,
    last_error: Option<String>,
}

impl<D: Driver> Daemon<D> {
    /// Builds a daemon around `smc`, saving config changes to `config_path`.
    pub fn new(smc: Smc<D>, config: Config, config_path: impl Into<PathBuf>) -> Self {
        Self {
            smc,
            config,
            control: ControlLoop::new(),
            config_path: config_path.into(),
            last_error: None,
        }
    }

    /// Returns the current config.
    #[cfg(test)]
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// Runs one control tick. Every SMC error is logged and stored, and the
    /// loop keeps running.
    pub fn tick(&mut self) {
        let effective = self.effective_config();
        match self.control.tick(&mut self.smc, &effective, Instant::now()) {
            Ok(Action::Sailing) => {}
            Ok(action) => {
                self.last_error = None;
                logging::info(format!("tick: {action:?}"));
            }
            Err(err) => self.record_error(format!("tick failed: {err}")),
        }
        self.tick_discharge();
        self.tick_top_up();
        self.tick_magsafe_led();
    }

    /// Returns the config the charge-band control loop should see this tick.
    ///
    /// A top-up in progress behaves as if the limit were off: the band stays
    /// untouched in `self.config`, so it comes straight back once the top-up
    /// ends.
    fn effective_config(&self) -> Config {
        if self.config.top_up_active {
            Config {
                upper: proto::MAX_UPPER,
                lower: proto::MAX_UPPER,
                ..self.config.clone()
            }
        } else {
            self.config.clone()
        }
    }

    /// Re-enables the adapter once discharging has reached the limit.
    fn tick_discharge(&mut self) {
        if self.config.adapter_enabled || !self.smc.has_adapter_control() {
            return;
        }
        let Some(percent) = self.read(|smc| smc.battery_percent()) else {
            return;
        };
        if percent > self.config.upper {
            return;
        }
        if let Err(err) = self.smc.set_adapter_enabled(true) {
            self.record_error(format!("cannot re-enable the adapter: {err}"));
            return;
        }
        self.config.adapter_enabled = true;
        self.save_config();
        logging::info("discharge reached limit");
    }

    /// Ends a top-up once the battery is full or the Mac is unplugged.
    fn tick_top_up(&mut self) {
        if !self.config.top_up_active {
            return;
        }
        let percent = self.read(|smc| smc.battery_percent()).unwrap_or(0);
        let plugged_in = self.read(|smc| smc.is_plugged_in()).unwrap_or(true);
        if percent < 100 && plugged_in {
            return;
        }
        self.config.top_up_active = false;
        self.save_config();
        logging::info("top-up finished");
    }

    /// Drives the MagSafe LED toward the configured mode, writing only when
    /// the colour actually needs to change.
    fn tick_magsafe_led(&mut self) {
        if !self.smc.has_magsafe_led() {
            return;
        }
        let mode = self.smc.charge_control_mode();
        let percent = self.read(|smc| smc.battery_percent()).unwrap_or(0);
        let plugged_in = self.read(|smc| smc.is_plugged_in()).unwrap_or(false);
        let allowed = self.charging_allowed(mode, percent);
        let want = match self.config.magsafe_led {
            MagsafeLedMode::System => MagsafeLed::System,
            MagsafeLedMode::Off => MagsafeLed::Off,
            MagsafeLedMode::Reflect if !plugged_in => MagsafeLed::System,
            MagsafeLedMode::Reflect if allowed => MagsafeLed::Orange,
            MagsafeLedMode::Reflect => MagsafeLed::Green,
        };
        if self.read(|smc| smc.magsafe_led()) == Some(want) {
            return;
        }
        if let Err(err) = self.smc.set_magsafe_led(want) {
            self.record_error(format!("cannot set the MagSafe LED: {err}"));
        }
    }

    /// Handles one sleep or wake notification.
    ///
    /// Every event is logged with the battery percent and the action taken,
    /// even a no-op: the log is the only record that the hooks are live.
    pub fn on_power_event(&mut self, event: PowerEvent) {
        let percent = self.read(|smc| smc.battery_percent()).unwrap_or(0);
        let inhibited_for_sleep = self.control.resume_after_wake();
        match self
            .control
            .on_power_event(&mut self.smc, &self.config, event, Instant::now())
        {
            Ok(action) => {
                self.last_error = None;
                logging::info(format!(
                    "{event:?}: battery {percent}%, {action:?}, inhibited for sleep {inhibited_for_sleep}"
                ));
            }
            Err(err) => self.record_error(format!("{event:?} failed: {err}")),
        }
    }

    /// Applies one request and returns the response to write back.
    pub fn apply(&mut self, request: Request) -> Response {
        match request {
            Request::Status => {}
            Request::SetLimit { upper, lower } => {
                let (upper, lower) = match proto::validate_limits(upper, lower) {
                    Ok(limits) => limits,
                    Err(message) => return Response::err(message),
                };
                self.config.upper = upper;
                self.config.lower = lower;
                self.save_config();
                logging::info(format!("limit set to {upper}/{lower}"));
                // The new band takes effect at once, not on the next tick.
                self.tick();
            }
            Request::SetAdapter { enabled } => {
                if !self.smc.has_adapter_control() {
                    return Response::err("adapter control not supported");
                }
                if let Err(err) = self.smc.set_adapter_enabled(enabled) {
                    return Response::err(err.to_string());
                }
                self.config.adapter_enabled = enabled;
                self.save_config();
                logging::info(format!("adapter enabled: {enabled}"));
            }
            Request::SetMagsafeLed { mode } => {
                self.config.magsafe_led = mode;
                self.save_config();
                logging::info(format!("MagSafe LED: {mode:?}"));
            }
            Request::TopUp => {
                self.config.top_up_active = true;
                self.save_config();
                logging::info("top-up requested");
                // The new band takes effect at once, not on the next tick.
                self.tick();
            }
            Request::CancelTopUp => {
                self.config.top_up_active = false;
                self.save_config();
                logging::info("top-up cancelled");
            }
        }
        Response::ok(self.status())
    }

    /// Reads the live state and returns it as a [`Status`].
    ///
    /// A failed read never fails the response: the field falls back and the
    /// message goes to `last_error`.
    pub fn status(&mut self) -> Status {
        let mode = self.smc.charge_control_mode();
        let battery_percent = self.read(|smc| smc.battery_percent()).unwrap_or(0);
        let plugged_in = self.read(|smc| smc.is_plugged_in()).unwrap_or(false);
        let charging_allowed = self.charging_allowed(mode, battery_percent);
        Status {
            version: proto::VERSION.to_string(),
            mode,
            battery_percent,
            plugged_in,
            charging_allowed,
            adapter_enabled: self.config.adapter_enabled,
            upper: self.config.upper,
            lower: self.config.lower,
            magsafe_led: self.config.magsafe_led,
            top_up_active: self.config.top_up_active,
            last_error: self.last_error.clone(),
        }
    }

    /// Opens the charge gate, re-enables the adapter and returns the MagSafe
    /// LED to the system.
    ///
    /// Called on SIGTERM/SIGINT and by `uninstall`. The machine must never
    /// be left discharging, with charging switched off, or with the LED
    /// showing a state the daemon chose.
    pub fn reset_charge_control(&mut self) {
        if let Err(err) = self.smc.reset_charge_control() {
            logging::error(format!("cannot reset charge control: {err}"));
        } else {
            logging::info("charge control reset: charging allowed");
        }
        if self.smc.has_adapter_control() {
            if let Err(err) = self.smc.set_adapter_enabled(true) {
                logging::error(format!("cannot re-enable the adapter: {err}"));
            }
        }
        if self.smc.has_magsafe_led() {
            if let Err(err) = self.smc.set_magsafe_led(MagsafeLed::System) {
                logging::error(format!("cannot reset MagSafe LED: {err}"));
            }
        }
    }

    /// Reports whether the machine may charge right now.
    fn charging_allowed(&mut self, mode: ChargeControlMode, percent: u8) -> bool {
        match mode {
            ChargeControlMode::Legacy => self
                .read(|smc| smc.is_charging_allowed())
                .unwrap_or_default(),
            ChargeControlMode::Firmware => match self.read(|smc| smc.firmware_limit()) {
                Some(limit) => !(limit.active && percent >= limit.upper),
                None => false,
            },
            ChargeControlMode::Unsupported => true,
        }
    }

    /// Runs one SMC read, recording the error instead of returning it.
    fn read<T>(&mut self, call: impl FnOnce(&mut Smc<D>) -> Result<T, smc::SmcError>) -> Option<T> {
        match call(&mut self.smc) {
            Ok(value) => Some(value),
            Err(err) => {
                self.record_error(err.to_string());
                None
            }
        }
    }

    fn save_config(&mut self) {
        let path: &Path = &self.config_path.clone();
        if let Err(err) = self.config.save(path) {
            self.record_error(format!("cannot save config to {}: {err}", path.display()));
        }
    }

    fn record_error(&mut self, message: String) {
        logging::error(&message);
        self.last_error = Some(message);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proto::MagsafeLedMode;
    use smc::{
        MockDriver, KEY_ACLC, KEY_ACW, KEY_BFD0, KEY_BFE0, KEY_BFF0, KEY_BUIC, KEY_CH0B, KEY_CH0C,
        KEY_CH0J, KEY_CHIE,
    };

    fn daemon() -> (Daemon<MockDriver>, MockDriver, PathBuf) {
        let mock = MockDriver::new();
        mock.seed(KEY_CH0B, &[0x00])
            .seed(KEY_CH0C, &[0x00])
            .seed(KEY_BUIC, &[79])
            .seed(KEY_ACW, &[0x01]);
        let path = std::env::temp_dir().join(format!(
            "chargecap-state-{}-{:?}.json",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_file(&path);
        let daemon = Daemon::new(Smc::new(mock.clone()), Config::default(), path.clone());
        (daemon, mock, path)
    }

    #[test]
    fn status_reports_the_live_state() {
        let (mut daemon, _mock, path) = daemon();
        let status = daemon.status();
        assert_eq!(status.mode, ChargeControlMode::Legacy);
        assert_eq!(status.battery_percent, 79);
        assert!(status.plugged_in);
        assert!(status.charging_allowed);
        assert_eq!(status.upper, 80);
        assert_eq!(status.lower, 78);
        assert_eq!(status.version, proto::VERSION);
        assert_eq!(status.last_error, None);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn set_limit_takes_effect_at_once() {
        let (mut daemon, mock, path) = daemon();
        let response = daemon.apply(Request::SetLimit {
            upper: 79,
            lower: None,
        });
        assert!(response.ok);
        let status = response.status.unwrap();
        assert_eq!((status.upper, status.lower), (79, 77));
        // 79% has reached the new limit, so the gate closed on this request.
        assert_eq!(
            mock.writes(),
            vec![
                (KEY_CH0B.to_string(), vec![0x02]),
                (KEY_CH0C.to_string(), vec![0x02]),
            ]
        );
        assert!(!status.charging_allowed);
        assert_eq!(Config::load(&path).unwrap().upper, 79);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn a_bad_limit_changes_nothing() {
        let (mut daemon, mock, path) = daemon();
        let response = daemon.apply(Request::SetLimit {
            upper: 49,
            lower: None,
        });
        assert!(!response.ok);
        assert_eq!(response.error.as_deref(), Some("upper must be 50..=100"));
        assert_eq!(daemon.config().upper, 80);
        assert!(mock.writes().is_empty());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn contract_commands_are_persisted_and_echoed() {
        // 90% is above the default limit, so discharge stays off even once
        // `TopUp` re-ticks the loop.
        let mock = MockDriver::new();
        mock.seed(KEY_CH0B, &[0x00])
            .seed(KEY_CH0C, &[0x00])
            .seed(KEY_BUIC, &[90])
            .seed(KEY_ACW, &[0x01])
            .seed(KEY_CH0J, &[0x00]);
        let path = std::env::temp_dir().join(format!(
            "chargecap-state-contract-{}-{:?}.json",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_file(&path);
        let mut daemon = Daemon::new(Smc::new(mock), Config::default(), path.clone());

        assert!(daemon.apply(Request::SetAdapter { enabled: false }).ok);
        assert!(
            daemon
                .apply(Request::SetMagsafeLed {
                    mode: MagsafeLedMode::Off
                })
                .ok
        );
        let status = daemon.apply(Request::TopUp).status.unwrap();
        assert!(!status.adapter_enabled);
        assert_eq!(status.magsafe_led, MagsafeLedMode::Off);
        assert!(status.top_up_active);

        let saved = Config::load(&path).unwrap();
        assert!(!saved.adapter_enabled);
        assert!(saved.top_up_active);

        let status = daemon.apply(Request::CancelTopUp).status.unwrap();
        assert!(!status.top_up_active);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn discharge_writes_the_adapter_gate_and_re_enables_at_the_limit() {
        // The gate starts inhibited, matching the band at 90% with the
        // default 80% limit, so the band itself writes nothing below.
        let mock = MockDriver::new();
        mock.seed(KEY_CH0B, &[0x02])
            .seed(KEY_CH0C, &[0x02])
            .seed(KEY_BUIC, &[90])
            .seed(KEY_ACW, &[0x01])
            .seed(KEY_CH0J, &[0x00]);
        let path = std::env::temp_dir().join(format!(
            "chargecap-state-discharge-{}-{:?}.json",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_file(&path);
        let mut daemon = Daemon::new(Smc::new(mock.clone()), Config::default(), path.clone());

        let response = daemon.apply(Request::SetAdapter { enabled: false });
        assert!(response.ok);
        assert_eq!(mock.writes(), vec![(KEY_CH0J.to_string(), vec![0x01])]);
        assert!(!Config::load(&path).unwrap().adapter_enabled);

        // Still above the limit: the tick leaves the adapter disabled.
        mock.clear_writes();
        daemon.tick();
        assert!(mock.writes().is_empty());

        // The battery has reached the limit: the tick re-enables the adapter
        // and persists it.
        mock.seed(KEY_BUIC, &[80]);
        mock.clear_writes();
        daemon.tick();
        assert_eq!(mock.writes(), vec![(KEY_CH0J.to_string(), vec![0x00])]);
        assert!(Config::load(&path).unwrap().adapter_enabled);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn discharge_uses_the_chie_off_value_on_tahoe() {
        let mock = MockDriver::new();
        mock.seed(KEY_CH0B, &[0x00])
            .seed(KEY_CH0C, &[0x00])
            .seed(KEY_BUIC, &[90])
            .seed(KEY_ACW, &[0x01])
            .seed(KEY_CHIE, &[0x00]);
        let mut daemon = Daemon::new(Smc::new(mock.clone()), Config::default(), "/dev/null");

        assert!(daemon.apply(Request::SetAdapter { enabled: false }).ok);
        assert_eq!(mock.writes(), vec![(KEY_CHIE.to_string(), vec![0x08])]);
    }

    #[test]
    fn discharge_without_an_adapter_key_is_refused() {
        let (mut daemon, mock, path) = daemon();
        let response = daemon.apply(Request::SetAdapter { enabled: false });
        assert!(!response.ok);
        assert_eq!(
            response.error.as_deref(),
            Some("adapter control not supported")
        );
        assert!(mock.writes().is_empty());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn magsafe_led_reflects_the_charge_gate() {
        let mock = MockDriver::new();
        mock.seed(KEY_CH0B, &[0x00])
            .seed(KEY_CH0C, &[0x00])
            .seed(KEY_BUIC, &[70])
            .seed(KEY_ACW, &[0x01])
            .seed(KEY_ACLC, &[0x00]);
        let mut daemon = Daemon::new(Smc::new(mock.clone()), Config::default(), "/dev/null");
        daemon.apply(Request::SetMagsafeLed {
            mode: MagsafeLedMode::Reflect,
        });

        // Allowed and plugged in: Orange.
        daemon.tick();
        assert_eq!(mock.value(KEY_ACLC), Some(vec![0x04]));

        // Nothing changed, so the second tick writes nothing.
        mock.clear_writes();
        daemon.tick();
        assert!(mock.writes().is_empty());

        // Above the limit, so the control loop inhibits the gate: Green.
        mock.seed(KEY_BUIC, &[80]);
        daemon.tick();
        assert_eq!(mock.value(KEY_ACLC), Some(vec![0x03]));

        // Unplugged: System.
        mock.seed(KEY_ACW, &[0x00]);
        daemon.tick();
        assert_eq!(mock.value(KEY_ACLC), Some(vec![0x00]));
    }

    #[test]
    fn magsafe_led_off_and_system_write_once() {
        let mock = MockDriver::new();
        mock.seed(KEY_CH0B, &[0x00])
            .seed(KEY_CH0C, &[0x00])
            .seed(KEY_BUIC, &[70])
            .seed(KEY_ACW, &[0x01])
            .seed(KEY_ACLC, &[0x00]);
        let mut daemon = Daemon::new(Smc::new(mock.clone()), Config::default(), "/dev/null");

        daemon.apply(Request::SetMagsafeLed {
            mode: MagsafeLedMode::Off,
        });
        daemon.tick();
        assert_eq!(mock.writes(), vec![(KEY_ACLC.to_string(), vec![0x01])]);

        mock.clear_writes();
        daemon.tick();
        assert!(mock.writes().is_empty());

        daemon.apply(Request::SetMagsafeLed {
            mode: MagsafeLedMode::System,
        });
        mock.clear_writes();
        daemon.tick();
        assert_eq!(mock.writes(), vec![(KEY_ACLC.to_string(), vec![0x00])]);

        mock.clear_writes();
        daemon.tick();
        assert!(mock.writes().is_empty());
    }

    #[test]
    fn top_up_opens_the_legacy_gate_and_restores_the_limit_at_100() {
        let mock = MockDriver::new();
        mock.seed(KEY_CH0B, &[0x02])
            .seed(KEY_CH0C, &[0x02])
            .seed(KEY_BUIC, &[85])
            .seed(KEY_ACW, &[0x01]);
        let path = std::env::temp_dir().join(format!(
            "chargecap-state-topup-{}-{:?}.json",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_file(&path);
        let mut daemon = Daemon::new(Smc::new(mock.clone()), Config::default(), path.clone());

        let status = daemon.apply(Request::TopUp).status.unwrap();
        assert!(status.top_up_active);
        assert_eq!(
            mock.writes(),
            vec![
                (KEY_CH0B.to_string(), vec![0x00]),
                (KEY_CH0C.to_string(), vec![0x00]),
            ]
        );

        // The battery is full: the flag clears, and the real limit is
        // restored on the following tick, not this one.
        mock.seed(KEY_BUIC, &[100]);
        mock.clear_writes();
        daemon.tick();
        assert!(mock.writes().is_empty());
        assert!(!Config::load(&path).unwrap().top_up_active);

        mock.clear_writes();
        daemon.tick();
        assert_eq!(
            mock.writes(),
            vec![
                (KEY_CH0B.to_string(), vec![0x02]),
                (KEY_CH0C.to_string(), vec![0x02]),
            ]
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn top_up_clears_and_rearms_the_firmware_limit() {
        let mock = MockDriver::new();
        mock.seed(KEY_BFF0, &[0x02])
            .seed(KEY_BFD0, &[80, 0, 0, 0])
            .seed(KEY_BFE0, &[78, 0, 0, 0])
            .seed(KEY_BUIC, &[85])
            .seed(KEY_ACW, &[0x01]);
        let config = Config {
            upper: 80,
            lower: 78,
            ..Config::default()
        };
        let mut daemon = Daemon::new(Smc::new(mock.clone()), config, "/dev/null");

        let status = daemon.apply(Request::TopUp).status.unwrap();
        assert!(status.top_up_active);
        assert_eq!(mock.writes(), vec![(KEY_BFF0.to_string(), vec![0x00])]);

        mock.seed(KEY_BUIC, &[100]);
        mock.clear_writes();
        daemon.tick();
        assert!(mock.writes().is_empty());

        mock.clear_writes();
        daemon.tick();
        assert_eq!(
            mock.writes(),
            vec![
                (KEY_BFD0.to_string(), vec![80, 0, 0, 0]),
                (KEY_BFE0.to_string(), vec![78, 0, 0, 0]),
                (KEY_BFF0.to_string(), vec![0x02]),
            ]
        );
    }

    #[test]
    fn reset_charge_control_re_enables_the_adapter_and_the_led() {
        let mock = MockDriver::new();
        mock.seed(KEY_CH0B, &[0x02])
            .seed(KEY_CH0C, &[0x02])
            .seed(KEY_BUIC, &[90])
            .seed(KEY_ACW, &[0x01])
            .seed(KEY_CH0J, &[0x01])
            .seed(KEY_ACLC, &[0x04]);
        let mut daemon = Daemon::new(Smc::new(mock.clone()), Config::default(), "/dev/null");

        daemon.reset_charge_control();
        assert_eq!(mock.value(KEY_CH0J), Some(vec![0x00]));
        assert_eq!(mock.value(KEY_ACLC), Some(vec![0x00]));
        assert!(mock.value(KEY_CH0B).unwrap()[0] == 0x00);
    }

    #[test]
    fn smc_errors_are_reported_but_never_fatal() {
        // No battery key, so every status read of it fails.
        let mock = MockDriver::new();
        mock.seed(KEY_CH0B, &[0x00]).seed(KEY_CH0C, &[0x00]);
        let path = std::env::temp_dir().join("chargecap-state-error.json");
        let mut daemon = Daemon::new(Smc::new(mock), Config::default(), &path);
        daemon.tick();
        let status = daemon.status();
        assert_eq!(status.battery_percent, 0);
        assert!(status.last_error.is_some());
    }
}
