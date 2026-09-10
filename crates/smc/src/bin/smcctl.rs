//! `smcctl` — a small development tool for the SMC keys `chargecap` uses.
//!
//! ```text
//! smcctl probe              list every known key, present or absent
//! smcctl read <KEY>         print the raw bytes as hex
//! smcctl write <KEY> <HEX>  write raw bytes (needs sudo)
//! smcctl status             print mode, battery, adapter and gate state
//! ```

use std::process::ExitCode;

use smc::{IoKitDriver, Smc, SmcError, ALL_KEYS};

const USAGE: &str = "usage: smcctl <probe|read <KEY>|write <KEY> <HEX>|status>";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let command = args.first().map(String::as_str);
    let result = match (command, args.len()) {
        (Some("probe"), 1) => probe(),
        (Some("read"), 2) => read(&args[1]),
        (Some("write"), 3) => write(&args[1], &args[2]),
        (Some("status"), 1) => status(),
        _ => {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(SmcError::NotPrivileged) => {
            eprintln!("error: SMC writes need root. Run smcctl again with sudo.");
            ExitCode::FAILURE
        }
        Err(err) => {
            eprintln!("error: {err}");
            ExitCode::FAILURE
        }
    }
}

fn open() -> Result<Smc<IoKitDriver>, SmcError> {
    Ok(Smc::new(IoKitDriver::open()?))
}

fn probe() -> Result<(), SmcError> {
    let mut smc = open()?;
    println!("mode: {:?}", smc.charge_control_mode());
    for key in ALL_KEYS {
        match smc.key_info(key) {
            Ok(info) => println!(
                "{key}  present  size {:>2}  type {}",
                info.data_size,
                smc::fourcc_str(info.data_type)
            ),
            Err(SmcError::KeyNotFound(_)) => println!("{key}  absent"),
            Err(err) => println!("{key}  error    {err}"),
        }
    }
    Ok(())
}

fn read(key: &str) -> Result<(), SmcError> {
    let bytes = open()?.read(key)?;
    println!("{}", hex(&bytes));
    Ok(())
}

fn write(key: &str, value: &str) -> Result<(), SmcError> {
    let bytes = parse_hex(value)?;
    open()?.write(key, &bytes)?;
    println!("{key} = {}", hex(&bytes));
    Ok(())
}

fn status() -> Result<(), SmcError> {
    let mut smc = open()?;
    println!("mode:             {:?}", smc.charge_control_mode());
    println!("battery percent:  {}", show(smc.battery_percent()));
    println!("plugged in:       {}", show(smc.is_plugged_in()));
    println!("charging allowed: {}", show(smc.is_charging_allowed()));
    if smc.charge_control_mode() == proto::ChargeControlMode::Firmware {
        match smc.firmware_limit() {
            Ok(limit) => println!(
                "firmware limit:   active {} lower {} upper {}",
                limit.active, limit.lower, limit.upper
            ),
            Err(err) => println!("firmware limit:   n/a ({err})"),
        }
    }
    println!("adapter control:  {}", smc.has_adapter_control());
    if smc.has_adapter_control() {
        println!("adapter enabled:  {}", show(smc.is_adapter_enabled()));
    }
    println!(
        "magsafe led:      {}",
        show(smc.magsafe_led().map(|led| format!("{led:?}")))
    );
    Ok(())
}

/// Renders an optional reading without failing the whole command.
fn show<T: std::fmt::Display>(value: Result<T, SmcError>) -> String {
    match value {
        Ok(value) => value.to_string(),
        Err(err) => format!("n/a ({err})"),
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn parse_hex(text: &str) -> Result<Vec<u8>, SmcError> {
    let cleaned: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    if cleaned.is_empty() || !cleaned.len().is_multiple_of(2) {
        return Err(SmcError::InvalidLimit(format!(
            "{text:?} is not an even-length hex string"
        )));
    }
    (0..cleaned.len())
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(&cleaned[i..i + 2], 16)
                .map_err(|_| SmcError::InvalidLimit(format!("{text:?} is not hex")))
        })
        .collect()
}
