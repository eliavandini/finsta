//! Sign in through Instagram's own login page in an embedded browser, then
//! keep the resulting cookies. The browser session is ephemeral: nothing is
//! written to disk by WebKit.

use std::cell::Cell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use gtk::glib;
use webkit6::prelude::*;

const LOGIN_URL: &str = "https://www.instagram.com/accounts/login/";

/// Calls `on_done` with the instagram.com cookies and the browser's user agent.
pub fn open(parent: &adw::ApplicationWindow, on_done: impl Fn(BTreeMap<String, String>, String) + 'static) {
    let network = webkit6::NetworkSession::new_ephemeral();
    let webview = webkit6::WebView::builder()
        .network_session(&network)
        .vexpand(true)
        .build();
    let Some(cookies) = network.cookie_manager() else {
        return;
    };

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&webview));
    let window = adw::Window::builder()
        .title("Sign In to Instagram")
        .transient_for(parent)
        .modal(true)
        .default_width(480)
        .default_height(780)
        .content(&toolbar)
        .build();

    // API calls reuse this, so they come from the same browser that signed in.
    let user_agent = webkit6::prelude::WebViewExt::settings(&webview)
        .and_then(|s| s.user_agent())
        .map(|ua| ua.to_string())
        .unwrap_or_default();
    let done = Rc::new(Cell::new(false));
    let on_done = Rc::new(on_done);
    let check = {
        let done = done.clone();
        let window = window.downgrade();
        let on_done = on_done.clone();
        let cookies = cookies.clone();
        Rc::new(move || {
            if done.get() {
                return;
            }
            let done = done.clone();
            let window = window.clone();
            let on_done = on_done.clone();
            let user_agent = user_agent.clone();
            cookies.cookies("https://www.instagram.com/", gtk::gio::Cancellable::NONE, move |res| {
                let Ok(list) = res else { return };
                let mut map = BTreeMap::new();
                for mut c in list {
                    if let (Some(name), Some(value)) = (c.name(), c.value()) {
                        map.insert(name.to_string(), value.to_string());
                    }
                }
                let logged_in = map.get("sessionid").is_some_and(|s| !s.is_empty())
                    && map.get("ds_user_id").is_some_and(|s| !s.is_empty());
                if logged_in && !done.replace(true) {
                    if let Some(w) = window.upgrade() {
                        w.close();
                    }
                    on_done(map, user_agent);
                }
            });
        })
    };

    {
        let check = check.clone();
        cookies.connect_changed(move |_| check());
    }
    {
        let check = check.clone();
        webview.connect_load_changed(move |_, event| {
            if event == webkit6::LoadEvent::Finished {
                check();
            }
        });
    }
    {
        let window = window.downgrade();
        glib::timeout_add_local(Duration::from_millis(1500), move || {
            if window.upgrade().is_none() || done.get() {
                return glib::ControlFlow::Break;
            }
            check();
            glib::ControlFlow::Continue
        });
    }

    webview.load_uri(LOGIN_URL);
    window.present();
}
