// No console window behind the app on Windows (release builds).
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod api;
mod rt;
mod ui;

use std::cell::Cell;

use adw::prelude::*;
use gtk::{gdk, gio, glib};

pub const APP_ID: &str = "io.github.eliavandini.Finsta";

thread_local! {
    /// Started with --background (at login): don't open the window.
    static START_HIDDEN: Cell<bool> = const { Cell::new(false) };
}

fn main() -> glib::ExitCode {
    gio::resources_register_include!("finsta.gresource").expect("failed to register resources");

    let app = adw::Application::builder().application_id(APP_ID).build();
    app.add_main_option(
        "background",
        glib::Char::from(b'b'),
        glib::OptionFlags::NONE,
        glib::OptionArg::None,
        "Start without opening the window, if Finsta runs in the background",
        None,
    );
    app.connect_handle_local_options(|_, options| {
        if options.contains("background") {
            START_HIDDEN.set(true);
        }
        std::ops::ControlFlow::Continue(())
    });

    app.connect_startup(|app| {
        let quit = gio::ActionEntry::builder("quit").activate(|app: &adw::Application, _, _| app.quit()).build();
        app.add_action_entries([quit]);
        app.set_accels_for_action("app.quit", &["<Control>q"]);
        app.set_accels_for_action("win.preferences", &["<Control>comma"]);
        gtk::Window::set_default_icon_name(APP_ID);
        let display = gdk::Display::default().expect("no display");
        gtk::IconTheme::for_display(&display).add_resource_path("/io/github/eliavandini/Finsta/icons");
        let css = gtk::CssProvider::new();
        css.load_from_resource("/io/github/eliavandini/Finsta/style.css");
        gtk::style_context_add_provider_for_display(
            &display,
            &css,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    });

    app.connect_activate(|app| {
        if let Some(window) = app.active_window() {
            window.present();
            return;
        }
        let hidden = START_HIDDEN.replace(false) && ui::prefs::get().run_in_background;
        ui::window::build(app, !hidden);
        ui::prefs::refresh_autostart();
    });

    app.run()
}
