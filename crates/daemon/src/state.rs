//! Daemon state: the SMC handle, the config and the control loop.
//!
//! [`Daemon`] is the one place that mutates state. The loop thread and every
//! socket connection reach it through the same `Mutex`, so a request and a
//! tick can never interleave.

use std::path::{Path, PathBuf};
use std::time::Instant;

use proto::{ChargeControlMode, Request, Response, Status};
use smc::{Driver, Smc};

use crate::config::Config;
use crate::control::{Action, ControlLoop};
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
        match self
            .control
            .tick(&mut self.smc, &self.config, Instant::now())
        {
            Ok(Action::Sailing) => {}
            Ok(action) => {
                self.last_error = None;
                logging::info(format!("tick: {action:?}"));
            }
            Err(err) => self.record_error(format!("tick failed: {err}")),
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

    /// Opens the charge gate and returns the MagSafe LED to the system.
    ///
    /// Called on SIGTERM/SIGINT and by `uninstall`. The machine must never
    /// be left with charging switched off.
    pub fn reset_charge_control(&mut self) {
        if let Err(err) = self.smc.reset_charge_control() {
            logging::error(format!("cannot reset charge control: {err}"));
        } else {
            logging::info("charge control reset: charging allowed");
        }
        if self.smc.has_magsafe_led() {
            if let Err(err) = self.smc.set_magsafe_led(smc::MagsafeLed::System) {
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
    use smc::{MockDriver, KEY_ACW, KEY_BUIC, KEY_CH0B, KEY_CH0C};

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
        let (mut daemon, _mock, path) = daemon();
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
