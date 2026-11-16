mod api;
mod rt;
mod ui;

use adw::prelude::*;
use gtk::{gdk, gio, glib};

pub const APP_ID: &str = "io.github.eliavandini.Finsta";

fn main() -> glib::ExitCode {
    gio::resources_register_include!("finsta.gresource").expect("failed to register resources");

    let app = adw::Application::builder().application_id(APP_ID).build();

    app.connect_startup(|_| {
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
        ui::window::build(app);
    });

    app.run()
}
