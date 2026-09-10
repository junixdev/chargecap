//! SMC (System Management Controller) access for battery charge control.
//!
//! [`Smc`] wraps a [`Driver`] and exposes typed helpers for every key the
//! `chargecap` daemon needs. Construction probes [`ALL_KEYS`] once, so
//! [`Smc::has_key`] and [`Smc::charge_control_mode`] are cheap afterwards.
//!
//! Apple Silicon only. Reads are unprivileged; writes need root.
//!
//! ```no_run
//! # fn main() -> Result<(), smc::SmcError> {
//! let mut smc = smc::Smc::new(smc::IoKitDriver::open()?);
//! println!("{}%", smc.battery_percent()?);
//! # Ok(()) }
//! ```

pub mod driver;
pub mod mock;

use std::collections::BTreeMap;
use std::fmt;

pub use driver::{fourcc, fourcc_str, Driver, IoKitDriver, KeyInfo, SmcKeyData};
pub use mock::MockDriver;
use proto::ChargeControlMode;

use driver::{CMD_READ_BYTES, CMD_READ_KEY_INFO, CMD_WRITE_BYTES, MAX_DATA_LEN};

/// Legacy charge gate, read and written. `0x00` allowed, `0x02` inhibited.
pub const KEY_CH0B: &str = "CH0B";
/// Legacy charge gate, written together with [`KEY_CH0B`].
pub const KEY_CH0C: &str = "CH0C";
/// Legacy charge gate on Tahoe firmware, 4 bytes little-endian.
pub const KEY_CHTE: &str = "CHTE";
/// Firmware limit activation. `0x02` active, `0x00` off.
pub const KEY_BFF0: &str = "bfF0";
/// Firmware upper percent, `u32` little-endian.
pub const KEY_BFD0: &str = "bfD0";
/// Firmware lower percent, `u32` little-endian.
pub const KEY_BFE0: &str = "bfE0";
/// Battery charge percent.
pub const KEY_BUIC: &str = "BUIC";
/// Power adapter present when the byte read as `i8` is above zero.
pub const KEY_ACW: &str = "AC-W";
/// MagSafe LED colour.
pub const KEY_ACLC: &str = "ACLC";
/// Adapter gate. `0x00` enabled, `0x01` disabled.
pub const KEY_CH0I: &str = "CH0I";
/// Adapter gate, second variant. `0x00` enabled, `0x01` disabled.
pub const KEY_CH0J: &str = "CH0J";
/// Adapter gate on Tahoe firmware. `0x00` enabled, `0x08` disabled.
pub const KEY_CHIE: &str = "CHIE";

/// Every key the capability probe looks for.
pub const ALL_KEYS: &[&str] = &[
    KEY_CH0B, KEY_CH0C, KEY_CHTE, KEY_BFF0, KEY_BFD0, KEY_BFE0, KEY_BUIC, KEY_ACW, KEY_ACLC,
    KEY_CH0I, KEY_CH0J, KEY_CHIE,
];

/// Legacy gate value that allows charging.
const CHARGE_ALLOW: u8 = 0x00;
/// Legacy gate value that inhibits charging.
const CHARGE_INHIBIT: u8 = 0x02;
/// `bfF0` value that activates the firmware limit.
const FIRMWARE_ACTIVE: u8 = 0x02;
/// `bfF0` value that clears the firmware limit.
const FIRMWARE_OFF: u8 = 0x00;
/// Adapter gate value that enables the adapter.
const ADAPTER_ON: u8 = 0x00;
/// Adapter gate value that disables the adapter on `CH0I`/`CH0J`.
const ADAPTER_OFF: u8 = 0x01;
/// Adapter gate value that disables the adapter on `CHIE`.
const ADAPTER_OFF_CHIE: u8 = 0x08;

/// Everything that can go wrong talking to the SMC.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SmcError {
    /// Opening the IOKit connection failed with this `IOReturn`.
    Open(i32),
    /// The `AppleSMC` service is not present.
    ServiceNotFound,
    /// A struct call failed with this `IOReturn`.
    Io(i32),
    /// The write needs root (`kIOReturnNotPrivileged`).
    NotPrivileged,
    /// This machine does not have the key.
    KeyNotFound(String),
    /// The SMC returned a nonzero result code.
    Smc { key: String, result: u8 },
    /// The key is not 4 ASCII characters.
    InvalidKey(String),
    /// The payload length does not match what the SMC reports for the key.
    UnexpectedSize {
        key: String,
        expected: usize,
        got: usize,
    },
    /// The call needs a different charge control mode.
    WrongMode {
        mode: ChargeControlMode,
        need: &'static str,
    },
    /// This machine does not have the hardware the call needs.
    Unsupported(&'static str),
    /// The requested limits are out of range.
    InvalidLimit(String),
}

impl fmt::Display for SmcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Open(rc) => write!(f, "cannot open AppleSMC: IOReturn 0x{rc:08x}"),
            Self::ServiceNotFound => write!(f, "AppleSMC service not found"),
            Self::Io(rc) => write!(f, "SMC call failed: IOReturn 0x{rc:08x}"),
            Self::NotPrivileged => write!(f, "SMC write needs root"),
            Self::KeyNotFound(key) => write!(f, "SMC key {key} not found on this Mac"),
            Self::Smc { key, result } => write!(f, "SMC key {key} returned result 0x{result:02x}"),
            Self::InvalidKey(key) => write!(f, "invalid SMC key {key:?}: need 4 ASCII characters"),
            Self::UnexpectedSize { key, expected, got } => {
                write!(f, "SMC key {key} takes {expected} bytes, got {got}")
            }
            Self::WrongMode { mode, need } => {
                write!(f, "charge control mode is {mode:?}, this call needs {need}")
            }
            Self::Unsupported(what) => write!(f, "this Mac has no {what}"),
            Self::InvalidLimit(msg) => write!(f, "invalid charge limit: {msg}"),
        }
    }
}

impl std::error::Error for SmcError {}

/// The state of the firmware charge limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FirmwareLimit {
    pub active: bool,
    pub lower: u8,
    pub upper: u8,
}

/// MagSafe LED colour, as stored in `ACLC`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum MagsafeLed {
    /// The system drives the LED.
    System = 0x00,
    Off = 0x01,
    Green = 0x03,
    Orange = 0x04,
}

impl MagsafeLed {
    /// Decodes a raw `ACLC` byte. `0x02` also reads as [`MagsafeLed::Green`].
    pub fn from_byte(byte: u8) -> Self {
        match byte {
            0x01 => Self::Off,
            0x02 | 0x03 => Self::Green,
            0x04 => Self::Orange,
            _ => Self::System,
        }
    }
}

/// Which legacy gate this Mac uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LegacyGate {
    /// `CH0B` and `CH0C`, one byte each.
    Ch0,
    /// `CHTE`, four bytes.
    Chte,
}

/// Typed SMC access over a [`Driver`].
#[derive(Debug)]
pub struct Smc<D: Driver> {
    driver: D,
    present: BTreeMap<String, KeyInfo>,
    mode: ChargeControlMode,
}

impl<D: Driver> Smc<D> {
    /// Wraps `driver` and probes [`ALL_KEYS`] once.
    pub fn new(driver: D) -> Self {
        let mut smc = Self {
            driver,
            present: BTreeMap::new(),
            mode: ChargeControlMode::Unsupported,
        };
        for key in ALL_KEYS {
            if let Ok(info) = smc.query_key_info(key) {
                if info.data_size > 0 {
                    smc.present.insert((*key).to_string(), info);
                }
            }
        }
        smc.mode = smc.compute_mode();
        smc
    }

    /// Returns true when the probe found `key` on this Mac.
    pub fn has_key(&self, key: &str) -> bool {
        self.present.contains_key(key)
    }

    /// Returns the size and type of `key`, from the probe cache if possible.
    pub fn key_info(&mut self, key: &str) -> Result<KeyInfo, SmcError> {
        if let Some(info) = self.present.get(key) {
            return Ok(*info);
        }
        let info = self.query_key_info(key)?;
        if info.data_size == 0 {
            return Err(SmcError::KeyNotFound(key.to_string()));
        }
        Ok(info)
    }

    /// Reads the raw bytes of `key`.
    pub fn read(&mut self, key: &str) -> Result<Vec<u8>, SmcError> {
        let info = self.key_info(key)?;
        let len = (info.data_size as usize).min(MAX_DATA_LEN);
        let mut input = SmcKeyData {
            key: fourcc(key)?,
            data8: CMD_READ_BYTES,
            ..SmcKeyData::default()
        };
        input.key_info.data_size = info.data_size;
        let output = self.driver.call(input)?;
        Ok(output.bytes[..len].to_vec())
    }

    /// Writes `bytes` to `key`. Needs root.
    pub fn write(&mut self, key: &str, bytes: &[u8]) -> Result<(), SmcError> {
        let info = self.key_info(key)?;
        if info.data_size as usize != bytes.len() || bytes.len() > MAX_DATA_LEN {
            return Err(SmcError::UnexpectedSize {
                key: key.to_string(),
                expected: info.data_size as usize,
                got: bytes.len(),
            });
        }
        let mut input = SmcKeyData {
            key: fourcc(key)?,
            data8: CMD_WRITE_BYTES,
            ..SmcKeyData::default()
        };
        input.key_info.data_size = info.data_size;
        input.bytes[..bytes.len()].copy_from_slice(bytes);
        self.driver.call(input)?;
        Ok(())
    }

    // -- charge control ----------------------------------------------------

    /// Returns the charge control mode found by the probe.
    pub fn charge_control_mode(&self) -> ChargeControlMode {
        self.mode
    }

    /// Returns true when the legacy gate allows charging.
    pub fn is_charging_allowed(&mut self) -> Result<bool, SmcError> {
        match self.legacy_gate()? {
            LegacyGate::Ch0 => Ok(self.read_u8(KEY_CH0B)? == CHARGE_ALLOW),
            LegacyGate::Chte => Ok(self.read(KEY_CHTE)?.iter().all(|b| *b == 0)),
        }
    }

    /// Opens the legacy gate.
    pub fn allow_charging(&mut self) -> Result<(), SmcError> {
        self.set_legacy_gate(true)
    }

    /// Closes the legacy gate.
    pub fn inhibit_charging(&mut self) -> Result<(), SmcError> {
        self.set_legacy_gate(false)
    }

    /// Reads the firmware limit state.
    pub fn firmware_limit(&mut self) -> Result<FirmwareLimit, SmcError> {
        self.require_mode(ChargeControlMode::Firmware, "firmware charge control")?;
        let active = self.read_u8(KEY_BFF0)? == FIRMWARE_ACTIVE;
        let upper = self.read_u32_le(KEY_BFD0)?;
        let lower = self.read_u32_le(KEY_BFE0)?;
        Ok(FirmwareLimit {
            active,
            lower: lower.min(u8::MAX as u32) as u8,
            upper: upper.min(u8::MAX as u32) as u8,
        })
    }

    /// Sets and activates the firmware limit.
    ///
    /// The firmware needs this write order: `bfD0` (upper), `bfE0` (lower),
    /// then `bfF0`.
    pub fn set_firmware_limit(&mut self, lower: u8, upper: u8) -> Result<(), SmcError> {
        if lower >= upper || upper > 100 {
            return Err(SmcError::InvalidLimit(format!(
                "need lower < upper <= 100, got lower {lower}, upper {upper}"
            )));
        }
        self.require_mode(ChargeControlMode::Firmware, "firmware charge control")?;
        self.write(KEY_BFD0, &u32::from(upper).to_le_bytes())?;
        self.write(KEY_BFE0, &u32::from(lower).to_le_bytes())?;
        self.write(KEY_BFF0, &[FIRMWARE_ACTIVE])
    }

    /// Deactivates the firmware limit.
    pub fn clear_firmware_limit(&mut self) -> Result<(), SmcError> {
        self.require_mode(ChargeControlMode::Firmware, "firmware charge control")?;
        self.write(KEY_BFF0, &[FIRMWARE_OFF])
    }

    /// Returns the machine to unrestricted charging.
    pub fn reset_charge_control(&mut self) -> Result<(), SmcError> {
        match self.mode {
            ChargeControlMode::Legacy => self.allow_charging(),
            ChargeControlMode::Firmware => self.clear_firmware_limit(),
            ChargeControlMode::Unsupported => Err(SmcError::WrongMode {
                mode: self.mode,
                need: "legacy or firmware charge control",
            }),
        }
    }

    // -- battery and adapter -----------------------------------------------

    /// Reads the battery charge percent.
    pub fn battery_percent(&mut self) -> Result<u8, SmcError> {
        self.read_u8(KEY_BUIC)
    }

    /// Returns true when a power adapter is attached.
    pub fn is_plugged_in(&mut self) -> Result<bool, SmcError> {
        Ok(self.read_u8(KEY_ACW)? as i8 > 0)
    }

    /// Returns true when this Mac can gate the power adapter.
    pub fn has_adapter_control(&self) -> bool {
        self.adapter_key().is_some()
    }

    /// Returns true when the power adapter is enabled.
    pub fn is_adapter_enabled(&mut self) -> Result<bool, SmcError> {
        let key = self
            .adapter_key()
            .ok_or(SmcError::Unsupported("adapter control"))?;
        Ok(self.read_u8(key)? == ADAPTER_ON)
    }

    /// Enables or disables the power adapter. Needs root.
    pub fn set_adapter_enabled(&mut self, on: bool) -> Result<(), SmcError> {
        let key = self
            .adapter_key()
            .ok_or(SmcError::Unsupported("adapter control"))?;
        let off = if key == KEY_CHIE {
            ADAPTER_OFF_CHIE
        } else {
            ADAPTER_OFF
        };
        let value = if on { ADAPTER_ON } else { off };
        self.write(key, &[value])
    }

    // -- MagSafe LED --------------------------------------------------------

    /// Returns true when this Mac has a controllable MagSafe LED.
    pub fn has_magsafe_led(&self) -> bool {
        self.has_key(KEY_ACLC)
    }

    /// Reads the MagSafe LED colour.
    pub fn magsafe_led(&mut self) -> Result<MagsafeLed, SmcError> {
        if !self.has_magsafe_led() {
            return Err(SmcError::Unsupported("MagSafe LED"));
        }
        Ok(MagsafeLed::from_byte(self.read_u8(KEY_ACLC)?))
    }

    /// Sets the MagSafe LED colour. Needs root.
    pub fn set_magsafe_led(&mut self, state: MagsafeLed) -> Result<(), SmcError> {
        if !self.has_magsafe_led() {
            return Err(SmcError::Unsupported("MagSafe LED"));
        }
        self.write(KEY_ACLC, &[state as u8])
    }

    // -- internals ----------------------------------------------------------

    /// Asks the SMC for the size and type of `key`, bypassing the cache.
    fn query_key_info(&mut self, key: &str) -> Result<KeyInfo, SmcError> {
        let input = SmcKeyData {
            key: fourcc(key)?,
            data8: CMD_READ_KEY_INFO,
            ..SmcKeyData::default()
        };
        Ok(self.driver.call(input)?.key_info)
    }

    /// Applies the mode precedence to the probe result.
    fn compute_mode(&self) -> ChargeControlMode {
        if self.has_key(KEY_BFF0) && self.has_key(KEY_BFD0) && self.has_key(KEY_BFE0) {
            ChargeControlMode::Firmware
        } else if (self.has_key(KEY_CH0B) && self.has_key(KEY_CH0C)) || self.has_key(KEY_CHTE) {
            ChargeControlMode::Legacy
        } else {
            ChargeControlMode::Unsupported
        }
    }

    fn require_mode(&self, want: ChargeControlMode, need: &'static str) -> Result<(), SmcError> {
        if self.mode == want {
            Ok(())
        } else {
            Err(SmcError::WrongMode {
                mode: self.mode,
                need,
            })
        }
    }

    fn legacy_gate(&self) -> Result<LegacyGate, SmcError> {
        self.require_mode(ChargeControlMode::Legacy, "legacy charge control")?;
        if self.has_key(KEY_CH0B) && self.has_key(KEY_CH0C) {
            Ok(LegacyGate::Ch0)
        } else {
            Ok(LegacyGate::Chte)
        }
    }

    fn set_legacy_gate(&mut self, allow: bool) -> Result<(), SmcError> {
        match self.legacy_gate()? {
            LegacyGate::Ch0 => {
                let value = if allow { CHARGE_ALLOW } else { CHARGE_INHIBIT };
                self.write(KEY_CH0B, &[value])?;
                self.write(KEY_CH0C, &[value])
            }
            LegacyGate::Chte => {
                let value: u32 = if allow { 0 } else { 1 };
                self.write(KEY_CHTE, &value.to_le_bytes())
            }
        }
    }

    /// Returns the adapter gate this Mac uses, preferring `CH0I`, then
    /// `CH0J`, then the Tahoe `CHIE`.
    fn adapter_key(&self) -> Option<&'static str> {
        [KEY_CH0I, KEY_CH0J, KEY_CHIE]
            .into_iter()
            .find(|key| self.has_key(key))
    }

    fn read_u8(&mut self, key: &str) -> Result<u8, SmcError> {
        let bytes = self.read(key)?;
        bytes.first().copied().ok_or(SmcError::UnexpectedSize {
            key: key.to_string(),
            expected: 1,
            got: 0,
        })
    }

    fn read_u32_le(&mut self, key: &str) -> Result<u32, SmcError> {
        let bytes = self.read(key)?;
        let array: [u8; 4] =
            bytes
                .get(..4)
                .and_then(|s| s.try_into().ok())
                .ok_or(SmcError::UnexpectedSize {
                    key: key.to_string(),
                    expected: 4,
                    got: bytes.len(),
                })?;
        Ok(u32::from_le_bytes(array))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `CH0B`/`CH0C` Mac: legacy gate, battery, adapter present.
    fn legacy_mock() -> MockDriver {
        let mock = MockDriver::new();
        mock.seed(KEY_CH0B, &[0x00])
            .seed(KEY_CH0C, &[0x00])
            .seed(KEY_BUIC, &[78])
            .seed(KEY_ACW, &[0x01]);
        mock
    }

    #[test]
    fn key_data_struct_is_80_bytes() {
        assert_eq!(std::mem::size_of::<SmcKeyData>(), 80);
    }

    #[test]
    fn fourcc_round_trips() {
        for key in ALL_KEYS {
            let encoded = fourcc(key).unwrap();
            assert_eq!(fourcc_str(encoded), *key);
        }
        assert_eq!(fourcc(KEY_CH0B).unwrap(), u32::from_be_bytes(*b"CH0B"));
    }

    #[test]
    fn fourcc_rejects_bad_keys() {
        assert!(matches!(fourcc("CH0"), Err(SmcError::InvalidKey(_))));
        assert!(matches!(fourcc("CH0BB"), Err(SmcError::InvalidKey(_))));
    }

    #[test]
    fn legacy_mode_is_detected() {
        let smc = Smc::new(legacy_mock());
        assert_eq!(smc.charge_control_mode(), ChargeControlMode::Legacy);
        assert!(smc.has_key(KEY_CH0B));
        assert!(!smc.has_key(KEY_BFF0));
    }

    #[test]
    fn legacy_inhibit_writes_both_keys_in_order() {
        let mock = legacy_mock();
        let mut smc = Smc::new(mock.clone());
        smc.inhibit_charging().unwrap();
        assert_eq!(
            mock.writes(),
            vec![
                (KEY_CH0B.to_string(), vec![0x02]),
                (KEY_CH0C.to_string(), vec![0x02]),
            ]
        );
        assert!(!smc.is_charging_allowed().unwrap());
    }

    #[test]
    fn legacy_allow_writes_zero_to_both_keys() {
        let mock = legacy_mock();
        let mut smc = Smc::new(mock.clone());
        smc.inhibit_charging().unwrap();
        mock.clear_writes();
        smc.allow_charging().unwrap();
        assert_eq!(
            mock.writes(),
            vec![
                (KEY_CH0B.to_string(), vec![0x00]),
                (KEY_CH0C.to_string(), vec![0x00]),
            ]
        );
        assert!(smc.is_charging_allowed().unwrap());
    }

    #[test]
    fn battery_percent_decodes() {
        let mut smc = Smc::new(legacy_mock());
        assert_eq!(smc.battery_percent().unwrap(), 78);
    }

    #[test]
    fn plugged_in_reads_signed_byte() {
        let mock = legacy_mock();
        let mut smc = Smc::new(mock.clone());
        assert!(smc.is_plugged_in().unwrap());
        mock.seed(KEY_ACW, &[0xFF]);
        assert!(!smc.is_plugged_in().unwrap());
    }

    #[test]
    fn reset_charge_control_opens_the_legacy_gate() {
        let mock = legacy_mock();
        let mut smc = Smc::new(mock.clone());
        smc.inhibit_charging().unwrap();
        mock.clear_writes();
        smc.reset_charge_control().unwrap();
        assert_eq!(mock.writes().len(), 2);
        assert!(smc.is_charging_allowed().unwrap());
    }

    #[test]
    fn chte_only_mac_is_legacy() {
        let mock = MockDriver::new();
        mock.seed(KEY_CHTE, &[0x00, 0x00, 0x00, 0x00]);
        let mut smc = Smc::new(mock.clone());
        assert_eq!(smc.charge_control_mode(), ChargeControlMode::Legacy);
        assert!(smc.is_charging_allowed().unwrap());

        smc.inhibit_charging().unwrap();
        assert_eq!(
            mock.writes(),
            vec![(KEY_CHTE.to_string(), vec![0x01, 0x00, 0x00, 0x00])]
        );
        assert!(!smc.is_charging_allowed().unwrap());

        mock.clear_writes();
        smc.allow_charging().unwrap();
        assert_eq!(
            mock.writes(),
            vec![(KEY_CHTE.to_string(), vec![0x00, 0x00, 0x00, 0x00])]
        );
    }

    #[test]
    fn firmware_keys_win_over_legacy_keys() {
        let mock = MockDriver::new();
        mock.seed(KEY_BFF0, &[0x00])
            .seed(KEY_BFD0, &[0, 0, 0, 0])
            .seed(KEY_BFE0, &[0, 0, 0, 0])
            .seed(KEY_CH0B, &[0x00]);
        let mut smc = Smc::new(mock.clone());
        assert_eq!(smc.charge_control_mode(), ChargeControlMode::Firmware);

        smc.set_firmware_limit(78, 80).unwrap();
        assert_eq!(
            mock.writes(),
            vec![
                (KEY_BFD0.to_string(), vec![0x50, 0x00, 0x00, 0x00]),
                (KEY_BFE0.to_string(), vec![0x4E, 0x00, 0x00, 0x00]),
                (KEY_BFF0.to_string(), vec![0x02]),
            ]
        );
        assert_eq!(
            smc.firmware_limit().unwrap(),
            FirmwareLimit {
                active: true,
                lower: 78,
                upper: 80,
            }
        );

        mock.clear_writes();
        smc.clear_firmware_limit().unwrap();
        assert_eq!(mock.writes(), vec![(KEY_BFF0.to_string(), vec![0x00])]);
        assert!(!smc.firmware_limit().unwrap().active);
    }

    #[test]
    fn firmware_mac_rejects_legacy_calls() {
        let mock = MockDriver::new();
        mock.seed(KEY_BFF0, &[0x00])
            .seed(KEY_BFD0, &[0, 0, 0, 0])
            .seed(KEY_BFE0, &[0, 0, 0, 0])
            .seed(KEY_CH0B, &[0x00]);
        let mut smc = Smc::new(mock);
        assert!(matches!(
            smc.allow_charging(),
            Err(SmcError::WrongMode { .. })
        ));
        assert!(matches!(
            smc.is_charging_allowed(),
            Err(SmcError::WrongMode { .. })
        ));
    }

    #[test]
    fn firmware_limit_range_is_validated() {
        let mock = MockDriver::new();
        mock.seed(KEY_BFF0, &[0x00])
            .seed(KEY_BFD0, &[0, 0, 0, 0])
            .seed(KEY_BFE0, &[0, 0, 0, 0]);
        let mut smc = Smc::new(mock.clone());
        for (lower, upper) in [(80, 80), (81, 80), (98, 101)] {
            assert!(matches!(
                smc.set_firmware_limit(lower, upper),
                Err(SmcError::InvalidLimit(_))
            ));
        }
        assert!(mock.writes().is_empty());
    }

    #[test]
    fn unsupported_mac_rejects_every_control_call() {
        let mock = MockDriver::new();
        mock.seed(KEY_BUIC, &[50]);
        let mut smc = Smc::new(mock);
        assert_eq!(smc.charge_control_mode(), ChargeControlMode::Unsupported);
        assert!(matches!(
            smc.is_charging_allowed(),
            Err(SmcError::WrongMode { .. })
        ));
        assert!(matches!(
            smc.allow_charging(),
            Err(SmcError::WrongMode { .. })
        ));
        assert!(matches!(
            smc.inhibit_charging(),
            Err(SmcError::WrongMode { .. })
        ));
        assert!(matches!(
            smc.firmware_limit(),
            Err(SmcError::WrongMode { .. })
        ));
        assert!(matches!(
            smc.set_firmware_limit(78, 80),
            Err(SmcError::WrongMode { .. })
        ));
        assert!(matches!(
            smc.clear_firmware_limit(),
            Err(SmcError::WrongMode { .. })
        ));
        assert!(matches!(
            smc.reset_charge_control(),
            Err(SmcError::WrongMode { .. })
        ));
    }

    #[test]
    fn adapter_uses_the_first_present_key() {
        let mock = MockDriver::new();
        mock.seed(KEY_CH0J, &[0x00]);
        let mut smc = Smc::new(mock.clone());
        assert!(smc.has_adapter_control());
        assert!(smc.is_adapter_enabled().unwrap());
        smc.set_adapter_enabled(false).unwrap();
        assert_eq!(mock.writes(), vec![(KEY_CH0J.to_string(), vec![0x01])]);
        assert!(!smc.is_adapter_enabled().unwrap());
    }

    #[test]
    fn chie_adapter_uses_its_own_disabled_value() {
        let mock = MockDriver::new();
        mock.seed(KEY_CHIE, &[0x00]);
        let mut smc = Smc::new(mock.clone());
        smc.set_adapter_enabled(false).unwrap();
        assert_eq!(mock.writes(), vec![(KEY_CHIE.to_string(), vec![0x08])]);
        assert!(!smc.is_adapter_enabled().unwrap());
    }

    #[test]
    fn adapter_calls_fail_without_the_hardware() {
        let mut smc = Smc::new(legacy_mock());
        assert!(!smc.has_adapter_control());
        assert!(matches!(
            smc.is_adapter_enabled(),
            Err(SmcError::Unsupported(_))
        ));
    }

    #[test]
    fn magsafe_led_round_trips() {
        let mock = MockDriver::new();
        mock.seed(KEY_ACLC, &[0x00]);
        let mut smc = Smc::new(mock.clone());
        assert!(smc.has_magsafe_led());
        smc.set_magsafe_led(MagsafeLed::Orange).unwrap();
        assert_eq!(mock.writes(), vec![(KEY_ACLC.to_string(), vec![0x04])]);
        assert_eq!(smc.magsafe_led().unwrap(), MagsafeLed::Orange);

        mock.seed(KEY_ACLC, &[0x02]);
        assert_eq!(smc.magsafe_led().unwrap(), MagsafeLed::Green);
    }

    #[test]
    fn magsafe_calls_fail_without_the_hardware() {
        let mut smc = Smc::new(legacy_mock());
        assert!(!smc.has_magsafe_led());
        assert!(matches!(smc.magsafe_led(), Err(SmcError::Unsupported(_))));
    }

    #[test]
    fn missing_keys_report_key_not_found() {
        let mut smc = Smc::new(MockDriver::new());
        assert!(matches!(smc.read(KEY_BUIC), Err(SmcError::KeyNotFound(_))));
        assert!(matches!(
            smc.battery_percent(),
            Err(SmcError::KeyNotFound(_))
        ));
    }

    #[test]
    fn write_rejects_the_wrong_length() {
        let mut smc = Smc::new(legacy_mock());
        assert!(matches!(
            smc.write(KEY_CH0B, &[0x00, 0x00]),
            Err(SmcError::UnexpectedSize { .. })
        ));
    }
}
