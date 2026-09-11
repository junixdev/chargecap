//! `chargecap` — the menu-bar app.
//!
//! A status-bar item shows the battery percent and the charge state. Its menu
//! picks the limit, turns the limit off and on, toggles launch at login,
//! checks for updates, opens the daemon log and quits. Every command goes to
//! `chargecapd` over the Unix socket; this binary never touches the SMC.

use std::path::PathBuf;
use std::process::Command;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use objc2::MainThreadMarker;
use proto::{MagsafeLedMode, Status};
use tao::event::Event;
use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy};
use tao::platform::macos::{ActivationPolicy, EventLoopExtMacOS};
use tray_icon::menu::{
    CheckMenuItem, Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem, Submenu,
};
use tray_icon::{TrayIcon, TrayIconBuilder};

use app::state::AppState;
use app::update::{self, Release, Version};
use app::{client, dialog, launch_agent, state, ui};

/// How often the worker thread asks the daemon for its status.
const POLL_INTERVAL: Duration = Duration::from_secs(5);

/// Menu id prefix for the preset rows.
const PRESET_PREFIX: &str = "preset:";
const MAGSAFE_PREFIX: &str = "magsafe:";
const ID_CUSTOM: &str = "custom";
const ID_ENABLED: &str = "enabled";
const ID_DISCHARGE: &str = "discharge";
const ID_TOP_UP: &str = "top_up";
const ID_LOGIN: &str = "login";
const ID_LOG: &str = "log";
const ID_UPDATE: &str = "update";
const ID_AUTO_UPDATE: &str = "auto_update";
const ID_QUIT: &str = "quit";

/// What the event loop reacts to.
enum UserEvent {
    /// A fresh poll result: the status, or the error text to show.
    Status(Box<Result<Status, String>>),
    /// A menu row was clicked.
    Menu(MenuEvent),
    /// An update check finished. `manual` is true when the user asked for it.
    UpdateCheck {
        result: Result<Option<Release>, String>,
        manual: bool,
    },
    /// The download-and-install flow finished.
    UpdateDone(Result<(), String>),
}

/// What the update row shows.
enum UpdateRow<'a> {
    Idle,
    Checking,
    Available(&'a Version),
    Installing(&'a Version),
}

fn main() -> Result<()> {
    let mut event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    // Accessory keeps the app out of the Dock and off the menu bar.
    event_loop.set_activation_policy(ActivationPolicy::Accessory);

    let menu_proxy = event_loop.create_proxy();
    MenuEvent::set_event_handler(Some(move |event| {
        let _ = menu_proxy.send_event(UserEvent::Menu(event));
    }));

    let socket = client::socket_path();
    let state_path = state::state_path();
    let plist_path = launch_agent::plist_path();
    let binary = std::env::current_exe()?;

    let mut app_state = state::load(&state_path);
    let mut ui = Ui::build(
        launch_agent::is_enabled(&plist_path),
        app_state.check_updates,
    )?;
    ui.apply(None);

    let refresh = spawn_poller(&event_loop, socket.clone());
    let proxy = event_loop.create_proxy();
    let current = Version::parse(update::CURRENT).expect("the crate version is semver");
    let mut latest: Option<Status> = None;
    let mut available: Option<Release> = None;
    let mut checking = false;
    let mut installing = false;

    event_loop.run(move |event, _target, control_flow| {
        *control_flow = ControlFlow::Wait;
        match event {
            Event::UserEvent(UserEvent::Status(result)) => {
                latest = (*result).ok();
                ui.apply(latest.as_ref());
                if app_state.check_updates
                    && !checking
                    && !installing
                    && available.is_none()
                    && now_secs() >= app_state.last_update_check + update::CHECK_INTERVAL.as_secs()
                {
                    app_state.last_update_check = now_secs();
                    save_state(&state_path, &app_state);
                    checking = true;
                    ui.set_update(UpdateRow::Checking);
                    spawn_check(&proxy, current.clone(), false);
                }
            }
            Event::UserEvent(UserEvent::UpdateCheck { result, manual }) => {
                checking = false;
                let mtm = MainThreadMarker::new().expect("the event loop runs on the main thread");
                match result {
                    Ok(Some(release)) => {
                        ui.set_update(UpdateRow::Available(&release.version));
                        available = Some(release);
                        if manual {
                            if let Some(release) = available.as_ref() {
                                if offer_install(mtm, release, &binary) {
                                    installing = true;
                                    ui.set_update(UpdateRow::Installing(&release.version));
                                    spawn_install(&proxy, release.clone());
                                }
                            }
                        }
                    }
                    Ok(None) => {
                        ui.set_update(UpdateRow::Idle);
                        if manual {
                            dialog::notice(
                                mtm,
                                "You are up to date",
                                &format!("chargecap {} is the newest version.", update::CURRENT),
                            );
                        }
                    }
                    Err(err) => {
                        ui.set_update(UpdateRow::Idle);
                        if manual {
                            dialog::error(mtm, &format!("Cannot check for updates. {err}"));
                        } else {
                            eprintln!("chargecap: update check failed: {err}");
                        }
                    }
                }
            }
            Event::UserEvent(UserEvent::UpdateDone(result)) => {
                installing = false;
                match result {
                    // The new instance is starting; this one steps aside.
                    Ok(()) => *control_flow = ControlFlow::Exit,
                    Err(err) => {
                        let mtm = MainThreadMarker::new()
                            .expect("the event loop runs on the main thread");
                        match available.as_ref() {
                            Some(release) => ui.set_update(UpdateRow::Available(&release.version)),
                            None => ui.set_update(UpdateRow::Idle),
                        }
                        dialog::error(mtm, &format!("The update did not finish. {err}"));
                    }
                }
            }
            Event::UserEvent(UserEvent::Menu(menu_event)) => {
                let Some(action) = Action::from_id(&menu_event.id) else {
                    return;
                };
                if matches!(action, Action::Quit) {
                    *control_flow = ControlFlow::Exit;
                    return;
                }
                if matches!(action, Action::Update) {
                    if checking || installing {
                        return;
                    }
                    let mtm =
                        MainThreadMarker::new().expect("the event loop runs on the main thread");
                    match available.as_ref() {
                        Some(release) => {
                            if offer_install(mtm, release, &binary) {
                                installing = true;
                                ui.set_update(UpdateRow::Installing(&release.version));
                                spawn_install(&proxy, release.clone());
                            }
                        }
                        None => {
                            checking = true;
                            ui.set_update(UpdateRow::Checking);
                            spawn_check(&proxy, current.clone(), true);
                        }
                    }
                    return;
                }
                if matches!(action, Action::ToggleAutoUpdate) {
                    app_state.check_updates = !app_state.check_updates;
                    ui.set_auto_update(app_state.check_updates);
                    save_state(&state_path, &app_state);
                    return;
                }
                {
                    let mtm =
                        MainThreadMarker::new().expect("the event loop runs on the main thread");
                    handle(
                        action,
                        mtm,
                        &socket,
                        &state_path,
                        &plist_path,
                        &binary,
                        &mut app_state,
                        latest.as_ref(),
                        &mut ui,
                        &refresh,
                    );
                }
            }
            _ => {}
        }
    })
}

/// Starts the worker thread that polls `status` every [`POLL_INTERVAL`].
///
/// The returned sender asks for one extra poll right away.
fn spawn_poller(event_loop: &tao::event_loop::EventLoop<UserEvent>, socket: PathBuf) -> Sender<()> {
    let (tx, rx) = mpsc::channel::<()>();
    let proxy = event_loop.create_proxy();
    std::thread::spawn(move || loop {
        let result = client::status(&socket).map_err(|err| err.to_string());
        if proxy
            .send_event(UserEvent::Status(Box::new(result)))
            .is_err()
        {
            return; // the event loop is gone
        }
        match rx.recv_timeout(POLL_INTERVAL) {
            Ok(()) | Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
    });
    tx
}

/// Seconds since the Unix epoch, or 0 when the clock is before it.
fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn save_state(state_path: &std::path::Path, app_state: &AppState) {
    if let Err(err) = state::save(state_path, app_state) {
        eprintln!("chargecap: cannot save the app state: {err}");
    }
}

/// Asks GitHub for a newer release on a worker thread.
fn spawn_check(proxy: &EventLoopProxy<UserEvent>, current: Version, manual: bool) {
    let proxy = proxy.clone();
    std::thread::spawn(move || {
        let result = update::check(&current).map_err(|err| err.to_string());
        let _ = proxy.send_event(UserEvent::UpdateCheck { result, manual });
    });
}

/// Downloads and installs `release` on a worker thread.
fn spawn_install(proxy: &EventLoopProxy<UserEvent>, release: Release) {
    let proxy = proxy.clone();
    std::thread::spawn(move || {
        let result = release
            .dmg_url
            .as_deref()
            .ok_or_else(|| "the release has no disk image".to_string())
            .and_then(|url| update::download(url).map_err(|err| err.to_string()))
            .and_then(|dmg| {
                update::install(&dmg, std::path::Path::new(update::APP_PATH))
                    .map_err(|err| err.to_string())
            });
        let _ = proxy.send_event(UserEvent::UpdateDone(result));
    });
}

/// Tells the user about `release` and asks to install it.
///
/// Returns true when the in-place install should start. When the app does
/// not run from `/Applications` or the release has no disk image, the
/// release page opens in the browser instead and this returns false.
fn offer_install(mtm: MainThreadMarker, release: &Release, binary: &std::path::Path) -> bool {
    let in_place = release.dmg_url.is_some()
        && update::bundle_of(binary).as_deref() == Some(std::path::Path::new(update::APP_PATH));
    if !in_place {
        let open = dialog::confirm(
            mtm,
            &format!("chargecap {} is available", release.version),
            "Open the release page to download it?",
            "Open page",
        );
        if open {
            if let Err(err) = Command::new("open").arg(&release.page_url).status() {
                dialog::error(mtm, &format!("cannot open the browser: {err}"));
            }
        }
        return false;
    }
    dialog::confirm(
        mtm,
        &format!("chargecap {} is available", release.version),
        &format!(
            "You have {}. The update downloads the new version, replaces the app in \
             Applications, and asks for your password once to update the background \
             helper. chargecap restarts when it is done.",
            update::CURRENT
        ),
        "Install and restart",
    )
}

/// One menu row the user can click.
enum Action {
    /// Set the limit to this percentage.
    Preset(u8),
    Custom,
    ToggleEnabled,
    ToggleDischarge,
    ToggleTopUp,
    SetMagsafeLed(MagsafeLedMode),
    ToggleLogin,
    OpenLog,
    /// Check for updates, or install the one already found.
    Update,
    ToggleAutoUpdate,
    Quit,
}

impl Action {
    fn from_id(id: &MenuId) -> Option<Self> {
        let id = id.as_ref();
        if let Some(rest) = id.strip_prefix(PRESET_PREFIX) {
            return rest.parse().ok().map(Action::Preset);
        }
        if let Some(rest) = id.strip_prefix(MAGSAFE_PREFIX) {
            let mode = match rest {
                "system" => MagsafeLedMode::System,
                "off" => MagsafeLedMode::Off,
                "reflect" => MagsafeLedMode::Reflect,
                _ => return None,
            };
            return Some(Action::SetMagsafeLed(mode));
        }
        match id {
            ID_CUSTOM => Some(Action::Custom),
            ID_ENABLED => Some(Action::ToggleEnabled),
            ID_DISCHARGE => Some(Action::ToggleDischarge),
            ID_TOP_UP => Some(Action::ToggleTopUp),
            ID_LOGIN => Some(Action::ToggleLogin),
            ID_LOG => Some(Action::OpenLog),
            ID_UPDATE => Some(Action::Update),
            ID_AUTO_UPDATE => Some(Action::ToggleAutoUpdate),
            ID_QUIT => Some(Action::Quit),
            _ => None,
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn handle(
    action: Action,
    mtm: MainThreadMarker,
    socket: &std::path::Path,
    state_path: &std::path::Path,
    plist_path: &std::path::Path,
    binary: &std::path::Path,
    app_state: &mut AppState,
    latest: Option<&Status>,
    ui: &mut Ui,
    refresh: &Sender<()>,
) {
    match action {
        Action::Preset(upper) => set_limit(mtm, socket, state_path, app_state, upper),
        Action::Custom => {
            let current = latest.map(|s| s.upper).unwrap_or(app_state.last_upper);
            let Some(text) = dialog::ask_custom_limit(mtm, current) else {
                return;
            };
            match parse_limit(&text) {
                Ok(upper) => set_limit(mtm, socket, state_path, app_state, upper),
                Err(message) => dialog::error(mtm, &message),
            }
        }
        Action::ToggleEnabled => match latest {
            Some(status) if status.upper >= proto::MAX_UPPER => {
                set_limit(mtm, socket, state_path, app_state, app_state.last_upper)
            }
            Some(_) => set_limit(mtm, socket, state_path, app_state, proto::MAX_UPPER),
            None => dialog::error(mtm, ui::NOT_RUNNING_ROW),
        },
        Action::ToggleDischarge => match latest {
            Some(status) => send_request(
                mtm,
                socket,
                proto::Request::SetAdapter {
                    enabled: !status.adapter_enabled,
                },
            ),
            None => dialog::error(mtm, ui::NOT_RUNNING_ROW),
        },
        Action::ToggleTopUp => match latest {
            Some(status) if status.top_up_active => {
                send_request(mtm, socket, proto::Request::CancelTopUp)
            }
            Some(_) => send_request(mtm, socket, proto::Request::TopUp),
            None => dialog::error(mtm, ui::NOT_RUNNING_ROW),
        },
        Action::SetMagsafeLed(mode) => match latest {
            Some(_) => send_request(mtm, socket, proto::Request::SetMagsafeLed { mode }),
            None => dialog::error(mtm, ui::NOT_RUNNING_ROW),
        },
        Action::ToggleLogin => {
            let result = if launch_agent::is_enabled(plist_path) {
                launch_agent::disable(plist_path)
            } else {
                launch_agent::enable(plist_path, binary)
            };
            if let Err(err) = result {
                dialog::error(mtm, &err.to_string());
            }
            ui.set_login(launch_agent::is_enabled(plist_path));
        }
        Action::OpenLog => {
            if let Err(err) = Command::new("open").arg(proto::LOG_PATH).status() {
                dialog::error(mtm, &format!("cannot open the log: {err}"));
            }
        }
        // Handled by the caller: these need the update state and the proxy.
        Action::Update | Action::ToggleAutoUpdate | Action::Quit => {}
    }
    let _ = refresh.send(());
}

/// Sends `set_limit` and remembers the choice when it is a real limit.
fn set_limit(
    mtm: MainThreadMarker,
    socket: &std::path::Path,
    state_path: &std::path::Path,
    app_state: &mut AppState,
    upper: u8,
) {
    match client::set_limit(socket, upper) {
        Ok(_) => {
            if upper < proto::MAX_UPPER && app_state.last_upper != upper {
                app_state.last_upper = upper;
                if let Err(err) = state::save(state_path, app_state) {
                    eprintln!("chargecap: cannot save the app state: {err}");
                }
            }
        }
        Err(err) => dialog::error(mtm, &err.to_string()),
    }
}

/// Sends `request` to the daemon, showing an error alert if it fails.
fn send_request(mtm: MainThreadMarker, socket: &std::path::Path, request: proto::Request) {
    if let Err(err) = client::send(socket, &request) {
        dialog::error(mtm, &err.to_string());
    }
}

/// Parses and validates the text from the "Custom…" dialog.
fn parse_limit(text: &str) -> Result<u8, String> {
    let trimmed = text.trim().trim_end_matches('%').trim();
    let upper: u8 = trimmed
        .parse()
        .map_err(|_| format!("\"{trimmed}\" is not a whole number"))?;
    proto::validate_limits(upper, None)?;
    Ok(upper)
}

/// The tray item and every menu row the app updates.
struct Ui {
    tray: TrayIcon,
    summary: MenuItem,
    limit: MenuItem,
    presets: Vec<(u8, CheckMenuItem)>,
    enabled: CheckMenuItem,
    discharge: CheckMenuItem,
    top_up: CheckMenuItem,
    magsafe_led: Vec<(MagsafeLedMode, CheckMenuItem)>,
    login: CheckMenuItem,
    daemon: MenuItem,
    update: MenuItem,
    auto_update: CheckMenuItem,
}

impl Ui {
    fn build(login_enabled: bool, auto_update_enabled: bool) -> Result<Self> {
        let menu = Menu::new();
        let summary = MenuItem::new(ui::summary(None), false, None);
        let limit = MenuItem::new(ui::limit_row(None), false, None);
        menu.append(&summary)?;
        menu.append(&PredefinedMenuItem::separator())?;
        menu.append(&limit)?;

        let mut presets = Vec::new();
        for percent in ui::presets() {
            let item = CheckMenuItem::with_id(
                format!("{PRESET_PREFIX}{percent}"),
                format!("{percent}%"),
                true,
                false,
                None,
            );
            menu.append(&item)?;
            presets.push((percent, item));
        }
        let custom = MenuItem::with_id(ID_CUSTOM, "Custom…", true, None);
        menu.append(&custom)?;
        menu.append(&PredefinedMenuItem::separator())?;

        let enabled = CheckMenuItem::with_id(ID_ENABLED, "Limit enabled", true, false, None);
        menu.append(&enabled)?;

        let discharge =
            CheckMenuItem::with_id(ID_DISCHARGE, "Discharge to limit", true, false, None);
        let top_up = CheckMenuItem::with_id(ID_TOP_UP, "Top up to 100% once", true, false, None);
        menu.append(&discharge)?;
        menu.append(&top_up)?;

        let magsafe_menu = Submenu::new("MagSafe LED", true);
        let magsafe_led: Vec<(MagsafeLedMode, CheckMenuItem)> = vec![
            (
                MagsafeLedMode::System,
                CheckMenuItem::with_id(
                    format!("{MAGSAFE_PREFIX}system"),
                    "System",
                    true,
                    true,
                    None,
                ),
            ),
            (
                MagsafeLedMode::Off,
                CheckMenuItem::with_id(format!("{MAGSAFE_PREFIX}off"), "Off", true, false, None),
            ),
            (
                MagsafeLedMode::Reflect,
                CheckMenuItem::with_id(
                    format!("{MAGSAFE_PREFIX}reflect"),
                    "Reflect charging",
                    true,
                    false,
                    None,
                ),
            ),
        ];
        for (_, item) in &magsafe_led {
            magsafe_menu.append(item)?;
        }
        menu.append(&magsafe_menu)?;

        let login = CheckMenuItem::with_id(ID_LOGIN, "Launch at login", true, login_enabled, None);
        menu.append(&login)?;
        menu.append(&PredefinedMenuItem::separator())?;

        let daemon = MenuItem::new(ui::daemon_row(None), false, None);
        let log = MenuItem::with_id(ID_LOG, "Open log", true, None);
        menu.append(&daemon)?;
        menu.append(&log)?;
        menu.append(&PredefinedMenuItem::separator())?;

        let version = MenuItem::new(format!("chargecap {}", update::CURRENT), false, None);
        let update = MenuItem::with_id(ID_UPDATE, "Check for updates…", true, None);
        let auto_update = CheckMenuItem::with_id(
            ID_AUTO_UPDATE,
            "Check for updates automatically",
            true,
            auto_update_enabled,
            None,
        );
        menu.append(&version)?;
        menu.append(&update)?;
        menu.append(&auto_update)?;
        menu.append(&PredefinedMenuItem::separator())?;

        let quit = MenuItem::with_id(ID_QUIT, "Quit", true, None);
        menu.append(&quit)?;

        let tray = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_title(ui::UNKNOWN_TITLE)
            .with_tooltip(ui::summary(None))
            .build()?;

        Ok(Self {
            tray,
            summary,
            limit,
            presets,
            enabled,
            discharge,
            top_up,
            magsafe_led,
            login,
            daemon,
            update,
            auto_update,
        })
    }

    /// Rewrites every label and checkmark for `status`.
    fn apply(&mut self, status: Option<&Status>) {
        let summary = ui::summary(status);
        self.tray.set_title(Some(ui::title(status)));
        let _ = self.tray.set_tooltip(Some(&summary));
        self.summary.set_text(&summary);
        self.limit.set_text(ui::limit_row(status));
        self.daemon.set_text(ui::daemon_row(status));

        let upper = status.map(|s| s.upper);
        for (percent, item) in &self.presets {
            item.set_checked(upper == Some(*percent));
            item.set_enabled(status.is_some());
        }
        self.enabled
            .set_checked(matches!(upper, Some(upper) if upper < proto::MAX_UPPER));
        self.enabled.set_enabled(status.is_some());

        self.discharge
            .set_checked(matches!(status, Some(s) if !s.adapter_enabled));
        self.discharge.set_enabled(status.is_some());
        self.top_up
            .set_checked(matches!(status, Some(s) if s.top_up_active));
        self.top_up.set_enabled(status.is_some());

        let led = status.map(|s| s.magsafe_led);
        for (mode, item) in &self.magsafe_led {
            item.set_checked(led == Some(*mode));
            item.set_enabled(status.is_some());
        }
    }

    fn set_login(&mut self, enabled: bool) {
        self.login.set_checked(enabled);
    }

    fn set_auto_update(&mut self, enabled: bool) {
        self.auto_update.set_checked(enabled);
    }

    fn set_update(&mut self, row: UpdateRow<'_>) {
        let (text, enabled) = match row {
            UpdateRow::Idle => ("Check for updates…".to_string(), true),
            UpdateRow::Checking => ("Checking for updates…".to_string(), false),
            UpdateRow::Available(version) => (format!("Update to {version}…"), true),
            UpdateRow::Installing(version) => (format!("Installing {version}…"), false),
        };
        self.update.set_text(text);
        self.update.set_enabled(enabled);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_limit_accepts_a_plain_number() {
        assert_eq!(parse_limit(" 75 "), Ok(75));
        assert_eq!(parse_limit("75%"), Ok(75));
    }

    #[test]
    fn parse_limit_rejects_text_and_out_of_range_values() {
        assert!(parse_limit("abc").unwrap_err().contains("whole number"));
        assert_eq!(parse_limit("49"), Err("upper must be 50..=100".to_string()));
    }

    #[test]
    fn action_ids_round_trip() {
        assert!(matches!(
            Action::from_id(&MenuId::new("preset:80")),
            Some(Action::Preset(80))
        ));
        assert!(matches!(
            Action::from_id(&MenuId::new(ID_QUIT)),
            Some(Action::Quit)
        ));
        assert!(Action::from_id(&MenuId::new("nope")).is_none());
    }
}
