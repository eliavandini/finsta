//! The app's preferences: running in the background, starting at login and
//! notifications for new messages. Saved in ~/.config/finsta/settings.json
//! under "app".

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use adw::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ui::layout;
use crate::ui::window::Main;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
    /// Closing the window keeps Finsta running (Quit from the menu).
    pub run_in_background: bool,
    pub start_at_login: bool,
    /// Notify about new messages while Finsta isn't the focused window.
    pub notify_messages: bool,
    /// Conversations whose notifications are off in Finsta.
    pub quiet_threads: Vec<String>,
}

impl Default for Prefs {
    fn default() -> Self {
        Prefs { run_in_background: false, start_at_login: false, notify_messages: true, quiet_threads: Vec::new() }
    }
}

thread_local! {
    static PREFS: RefCell<Option<Prefs>> = const { RefCell::new(None) };
}

pub fn get() -> Prefs {
    PREFS.with_borrow_mut(|p| {
        p.get_or_insert_with(|| serde_json::from_value(layout::read_all()["app"].clone()).unwrap_or_default())
            .clone()
    })
}

pub fn update(f: impl FnOnce(&mut Prefs)) {
    let mut prefs = get();
    f(&mut prefs);
    PREFS.with_borrow_mut(|p| *p = Some(prefs.clone()));
    layout::write_key("app", serde_json::to_value(&prefs).unwrap_or_default());
}

pub fn is_quiet(thread_id: &str) -> bool {
    get().quiet_threads.iter().any(|t| t == thread_id)
}

pub fn set_quiet(thread_id: &str, quiet: bool) {
    update(|p| {
        p.quiet_threads.retain(|t| t != thread_id);
        if quiet {
            p.quiet_threads.push(thread_id.to_string());
        }
    });
}

fn autostart_file() -> Option<PathBuf> {
    Some(dirs::config_dir()?.join("autostart").join(format!("{}.desktop", crate::APP_ID)))
}

/// The program to start at login: this one, or the installed copy when
/// this is a development build.
fn program() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let installed = dirs::home_dir().map(|h| h.join(".local/bin/finsta"));
    let dev_build = exe.components().any(|c| c.as_os_str() == "target");
    match installed {
        Some(i) if dev_build && i.exists() => Some(i),
        _ => Some(exe),
    }
}

/// Start Finsta at login (in the background), or stop doing so.
pub fn set_autostart(on: bool) -> std::io::Result<()> {
    let Some(file) = autostart_file() else { return Ok(()) };
    if !on {
        return match std::fs::remove_file(&file) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        };
    }
    let Some(program) = program() else { return Ok(()) };
    let exec = program.to_string_lossy().replace('\\', "\\\\").replace('"', "\\\"");
    let entry = format!(
        "[Desktop Entry]\nType=Application\nName=Finsta\nComment=Start Finsta at login\nExec=\"{exec}\" --background\nIcon={}\nTerminal=false\nX-GNOME-Autostart-enabled=true\n",
        crate::APP_ID
    );
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&file, entry)
}

/// Keep the login entry pointing at this program (it may have moved).
pub fn refresh_autostart() {
    if get().start_at_login
        && let Err(e) = set_autostart(true)
    {
        eprintln!("finsta: couldn't update the login entry ({})", e.kind());
    }
}

fn switch(title: &str, subtitle: &str, on: bool) -> adw::SwitchRow {
    adw::SwitchRow::builder().title(title).subtitle(subtitle).active(on).build()
}

pub fn dialog(main: &Rc<Main>) {
    let prefs = get();
    let page = adw::PreferencesPage::new();

    let running = adw::PreferencesGroup::builder().title("Running").build();
    let background = switch(
        "Run in Background",
        "Closing the window keeps Finsta running, so new messages still notify you. Quit from the main menu.",
        prefs.run_in_background,
    );
    let login = switch(
        "Start at Login",
        "Starts Finsta when you log in; in the background if it runs in the background",
        prefs.start_at_login,
    );
    running.add(&background);
    running.add(&login);
    page.add(&running);

    let notifications = adw::PreferencesGroup::builder()
        .title("Notifications")
        .description("Conversations muted on Instagram, or in Finsta from a conversation's right-click menu, don't notify.")
        .build();
    let messages = switch("New Messages", "When Finsta isn't the window you're using", prefs.notify_messages);
    notifications.add(&messages);
    page.add(&notifications);

    background.connect_active_notify(|r| update(|p| p.run_in_background = r.is_active()));
    messages.connect_active_notify(|r| update(|p| p.notify_messages = r.is_active()));
    let weak = Rc::downgrade(main);
    let reverting = std::cell::Cell::new(false);
    login.connect_active_notify(move |r| {
        if reverting.get() {
            return;
        }
        let on = r.is_active();
        match set_autostart(on) {
            Ok(()) => update(|p| p.start_at_login = on),
            Err(e) => {
                eprintln!("finsta: couldn't change the login entry ({})", e.kind());
                if let Some(main) = weak.upgrade() {
                    main.toast("Couldn't change whether Finsta starts at login");
                }
                reverting.set(true);
                r.set_active(!on);
                reverting.set(false);
            }
        }
    });

    let dialog = adw::PreferencesDialog::builder().title("Preferences").build();
    dialog.add(&page);
    dialog.present(Some(&main.window));
}
