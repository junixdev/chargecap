//! Wire contract and file paths shared by `chargecapd` and `chargecap`.
//!
//! Frames are newline-delimited JSON objects: one [`Request`] or [`Response`]
//! per line, written with [`write_request`]/[`write_response`] and read back
//! with [`read_line_json`].

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const SOCKET_PATH: &str = "/var/run/chargecap.sock";
pub const CONFIG_PATH: &str = "/Library/Application Support/chargecap/config.json";
pub const LOG_PATH: &str = "/Library/Logs/chargecap/daemon.log";
pub const DAEMON_LABEL: &str = "io.github.junixdev.chargecap";
pub const DAEMON_PLIST_PATH: &str = "/Library/LaunchDaemons/io.github.junixdev.chargecap.plist";
pub const APP_LABEL: &str = "io.github.junixdev.chargecap.app"; // LaunchAgent for launch-at-login
pub const DEFAULT_UPPER: u8 = 80;
pub const DEFAULT_GAP: u8 = 2; // lower = upper - DEFAULT_GAP
pub const MIN_UPPER: u8 = 50;
pub const MAX_UPPER: u8 = 100; // 100 means "limit disabled"

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Request {
    Status,
    SetLimit {
        upper: u8,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        lower: Option<u8>,
    },
    SetAdapter {
        enabled: bool,
    },
    SetMagsafeLed {
        mode: MagsafeLedMode,
    },
    TopUp,
    CancelTopUp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MagsafeLedMode {
    System,
    Off,
    Reflect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChargeControlMode {
    Legacy,
    Firmware,
    Unsupported,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Status {
    pub version: String,
    pub mode: ChargeControlMode,
    pub battery_percent: u8,
    pub plugged_in: bool,
    pub charging_allowed: bool,
    pub adapter_enabled: bool,
    pub upper: u8,
    pub lower: u8,
    pub magsafe_led: MagsafeLedMode,
    pub top_up_active: bool,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Response {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<Status>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}
impl Response {
    pub fn ok(status: Status) -> Self {
        Self {
            ok: true,
            status: Some(status),
            error: None,
        }
    }
    pub fn err(msg: impl Into<String>) -> Self {
        Self {
            ok: false,
            status: None,
            error: Some(msg.into()),
        }
    }
}

/// Validates and fills in a `set_limit` request, returning `(upper, lower)`.
///
/// `upper` must be in `MIN_UPPER..=MAX_UPPER`. `lower` defaults to
/// `upper - DEFAULT_GAP` (saturating). `upper == MAX_UPPER` disables the
/// limit entirely, so `lower` is forced to `MAX_UPPER` too. Otherwise `lower`
/// must be in `(MIN_UPPER - 10)..upper`.
pub fn validate_limits(upper: u8, lower: Option<u8>) -> Result<(u8, u8), String> {
    if !(MIN_UPPER..=MAX_UPPER).contains(&upper) {
        return Err("upper must be 50..=100".to_string());
    }
    if upper == MAX_UPPER {
        return Ok((upper, MAX_UPPER));
    }
    let lower = lower.unwrap_or_else(|| upper.saturating_sub(DEFAULT_GAP));
    if lower < MIN_UPPER - 10 || lower >= upper {
        return Err("lower must be below upper".to_string());
    }
    Ok((upper, lower))
}

/// Writes `r` as one JSON object followed by a newline.
pub fn write_request(w: &mut impl std::io::Write, r: &Request) -> std::io::Result<()> {
    serde_json::to_writer(&mut *w, r)?;
    w.write_all(b"\n")
}

/// Writes `r` as one JSON object followed by a newline.
pub fn write_response(w: &mut impl std::io::Write, r: &Response) -> std::io::Result<()> {
    serde_json::to_writer(&mut *w, r)?;
    w.write_all(b"\n")
}

/// Reads one newline-terminated JSON object and deserializes it as `T`.
pub fn read_line_json<T: serde::de::DeserializeOwned>(
    r: &mut impl std::io::BufRead,
) -> std::io::Result<T> {
    let mut line = String::new();
    r.read_line(&mut line)?;
    serde_json::from_str(line.trim_end()).map_err(std::io::Error::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status_example() -> Status {
        Status {
            version: "0.1.0".to_string(),
            mode: ChargeControlMode::Legacy,
            battery_percent: 78,
            plugged_in: true,
            charging_allowed: false,
            adapter_enabled: true,
            upper: 80,
            lower: 78,
            magsafe_led: MagsafeLedMode::System,
            top_up_active: false,
            last_error: None,
        }
    }

    #[test]
    fn requests_round_trip() {
        let cases: Vec<(Request, &str)> = vec![
            (Request::Status, r#"{"cmd":"status"}"#),
            (
                Request::SetLimit {
                    upper: 80,
                    lower: None,
                },
                r#"{"cmd":"set_limit","upper":80}"#,
            ),
            (
                Request::SetLimit {
                    upper: 80,
                    lower: Some(75),
                },
                r#"{"cmd":"set_limit","upper":80,"lower":75}"#,
            ),
            (
                Request::SetAdapter { enabled: false },
                r#"{"cmd":"set_adapter","enabled":false}"#,
            ),
            (
                Request::SetMagsafeLed {
                    mode: MagsafeLedMode::Reflect,
                },
                r#"{"cmd":"set_magsafe_led","mode":"reflect"}"#,
            ),
            (Request::TopUp, r#"{"cmd":"top_up"}"#),
        ];
        for (req, wire) in cases {
            assert_eq!(serde_json::to_string(&req).unwrap(), wire);
            assert_eq!(serde_json::from_str::<Request>(wire).unwrap(), req);
        }
    }

    #[test]
    fn responses_round_trip() {
        let ok = Response::ok(status_example());
        let ok_wire = r#"{"ok":true,"status":{"version":"0.1.0","mode":"legacy","battery_percent":78,"plugged_in":true,"charging_allowed":false,"adapter_enabled":true,"upper":80,"lower":78,"magsafe_led":"system","top_up_active":false,"last_error":null}}"#;
        assert_eq!(serde_json::to_string(&ok).unwrap(), ok_wire);
        assert_eq!(serde_json::from_str::<Response>(ok_wire).unwrap(), ok);

        let err = Response::err("upper must be 50..=100");
        let err_wire = r#"{"ok":false,"error":"upper must be 50..=100"}"#;
        assert_eq!(serde_json::to_string(&err).unwrap(), err_wire);
        assert_eq!(serde_json::from_str::<Response>(err_wire).unwrap(), err);
    }

    #[test]
    fn validate_limits_defaults_lower() {
        assert_eq!(validate_limits(80, None), Ok((80, 78)));
    }

    #[test]
    fn validate_limits_100_disables() {
        assert_eq!(validate_limits(100, None), Ok((100, 100)));
    }

    #[test]
    fn validate_limits_rejects_out_of_range_upper() {
        assert_eq!(
            validate_limits(49, None),
            Err("upper must be 50..=100".to_string())
        );
    }

    #[test]
    fn validate_limits_rejects_lower_above_upper() {
        assert_eq!(
            validate_limits(80, Some(85)),
            Err("lower must be below upper".to_string())
        );
    }
}
