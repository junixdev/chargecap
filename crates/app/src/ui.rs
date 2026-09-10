//! Pure label helpers for the status-bar title and the menu rows.
//!
//! Every function takes `Option<&Status>`, where `None` means the daemon did
//! not answer. No AppKit here, so the tests can cover all of it.

use proto::{ChargeControlMode, Status, MAX_UPPER, MIN_UPPER};

/// Step between two preset limits, in percent.
const PRESET_STEP: u8 = 5;

/// Shown in the status bar when the daemon does not answer.
pub const UNKNOWN_TITLE: &str = "⚠︎";

/// Shown in the first menu row when the daemon does not answer.
pub const NOT_RUNNING_ROW: &str = "Daemon not running — run: sudo chargecapd install";

/// Returns the preset limits offered in the menu: 50, 55, ... 95.
///
/// 100 is not a preset; the "Limit enabled" row turns the limit off instead.
pub fn presets() -> Vec<u8> {
    (MIN_UPPER..MAX_UPPER)
        .step_by(PRESET_STEP as usize)
        .collect()
}

/// Returns the status-bar title for `status`.
///
/// | State | Title |
/// |---|---|
/// | no daemon | `⚠︎` |
/// | limit off (`upper == 100`) | `93% ∞` |
/// | unplugged | `64%` |
/// | plugged in, charging blocked | `80% ■` |
/// | plugged in, charging below the limit | `78% ↑` |
pub fn title(status: Option<&Status>) -> String {
    let Some(status) = status else {
        return UNKNOWN_TITLE.to_string();
    };
    let percent = status.battery_percent;
    if !status.adapter_enabled {
        return format!("{percent}% ↓");
    }
    if status.top_up_active {
        return format!("{percent}% ↑ 100");
    }
    if status.upper >= MAX_UPPER {
        return format!("{percent}% ∞");
    }
    if !status.plugged_in {
        return format!("{percent}%");
    }
    if !status.charging_allowed {
        return format!("{percent}% ■");
    }
    if percent < status.upper {
        return format!("{percent}% ↑");
    }
    format!("{percent}%")
}

/// Returns the first menu row, also used as the tooltip.
pub fn summary(status: Option<&Status>) -> String {
    let Some(status) = status else {
        return format!("🔋 {NOT_RUNNING_ROW}");
    };
    let percent = status.battery_percent;
    let state = if status.upper >= MAX_UPPER {
        "Limit off".to_string()
    } else if !status.plugged_in {
        "On battery".to_string()
    } else if !status.charging_allowed {
        format!("Holding at {}%", status.upper)
    } else if percent < status.upper {
        format!("Charging to {}%", status.upper)
    } else {
        format!("At the {}% limit", status.upper)
    };
    format!("🔋 {percent}% · {state}")
}

/// Returns the disabled "Charge limit" row.
pub fn limit_row(status: Option<&Status>) -> String {
    match status {
        Some(status) if status.upper >= MAX_UPPER => "Charge limit: off".to_string(),
        Some(status) => format!("Charge limit: {}%", status.upper),
        None => "Charge limit: unknown".to_string(),
    }
}

/// Returns the disabled daemon row at the bottom of the menu.
pub fn daemon_row(status: Option<&Status>) -> String {
    match status {
        Some(status) => format!(
            "Daemon: running · {} · v{}",
            mode_name(status.mode),
            status.version
        ),
        None => NOT_RUNNING_ROW.to_string(),
    }
}

fn mode_name(mode: ChargeControlMode) -> &'static str {
    match mode {
        ChargeControlMode::Legacy => "legacy",
        ChargeControlMode::Firmware => "firmware",
        ChargeControlMode::Unsupported => "unsupported",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proto::MagsafeLedMode;

    fn status(percent: u8, plugged_in: bool, charging_allowed: bool, upper: u8) -> Status {
        Status {
            version: "0.1.0".to_string(),
            mode: ChargeControlMode::Legacy,
            battery_percent: percent,
            plugged_in,
            charging_allowed,
            adapter_enabled: true,
            upper,
            lower: upper.saturating_sub(proto::DEFAULT_GAP),
            magsafe_led: MagsafeLedMode::System,
            top_up_active: false,
            last_error: None,
        }
    }

    #[test]
    fn title_discharging() {
        let mut s = status(78, true, false, 80);
        s.adapter_enabled = false;
        assert_eq!(title(Some(&s)), "78% ↓");
    }

    #[test]
    fn title_topping_up() {
        let mut s = status(78, true, true, 80);
        s.top_up_active = true;
        assert_eq!(title(Some(&s)), "78% ↑ 100");
    }

    #[test]
    fn title_discharging_wins_over_top_up() {
        let mut s = status(78, true, false, 80);
        s.adapter_enabled = false;
        s.top_up_active = true;
        assert_eq!(title(Some(&s)), "78% ↓");
    }

    #[test]
    fn title_charging_below_the_limit() {
        assert_eq!(title(Some(&status(78, true, true, 80))), "78% ↑");
    }

    #[test]
    fn title_holding_at_the_limit() {
        assert_eq!(title(Some(&status(80, true, false, 80))), "80% ■");
    }

    #[test]
    fn title_unplugged() {
        assert_eq!(title(Some(&status(64, false, false, 80))), "64%");
    }

    #[test]
    fn title_limit_off() {
        assert_eq!(title(Some(&status(93, true, true, 100))), "93% ∞");
    }

    #[test]
    fn title_without_a_daemon() {
        assert_eq!(title(None), "⚠︎");
    }

    #[test]
    fn presets_run_from_50_to_95_in_fives() {
        assert_eq!(presets(), vec![50, 55, 60, 65, 70, 75, 80, 85, 90, 95]);
    }

    #[test]
    fn summary_reads_as_the_first_row() {
        assert_eq!(
            summary(Some(&status(78, true, false, 80))),
            "🔋 78% · Holding at 80%"
        );
        assert_eq!(
            summary(Some(&status(64, false, false, 80))),
            "🔋 64% · On battery"
        );
        assert!(summary(None).contains("Daemon not running"));
    }

    #[test]
    fn limit_row_shows_off_at_100() {
        assert_eq!(
            limit_row(Some(&status(90, true, true, 100))),
            "Charge limit: off"
        );
        assert_eq!(
            limit_row(Some(&status(78, true, true, 80))),
            "Charge limit: 80%"
        );
        assert_eq!(limit_row(None), "Charge limit: unknown");
    }

    #[test]
    fn daemon_row_names_the_mode_and_version() {
        assert_eq!(
            daemon_row(Some(&status(78, true, true, 80))),
            "Daemon: running · legacy · v0.1.0"
        );
        assert_eq!(daemon_row(None), NOT_RUNNING_ROW);
    }
}
