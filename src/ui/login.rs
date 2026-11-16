//! Sign-in screen.

use std::collections::BTreeMap;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

use crate::api::{Client, Session};
use crate::rt;

pub fn page(window: &adw::ApplicationWindow, on_success: Rc<dyn Fn(Session)>) -> gtk::Widget {
    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(18)
        .build();

    let error = gtk::Label::builder()
        .wrap(true)
        .justify(gtk::Justification::Center)
        .css_classes(["error"])
        .visible(false)
        .build();
    let spinner = adw::Spinner::builder().visible(false).height_request(32).build();

    // Validate a set of cookies by fetching the account's profile.
    let finish: Rc<dyn Fn(BTreeMap<String, String>, String)> = {
        let error = error.clone();
        let spinner = spinner.clone();
        let content = content.clone();
        Rc::new(move |cookies, user_agent| {
            error.set_visible(false);
            let session = match Session::from_cookies(cookies) {
                Ok(mut s) => {
                    s.user_agent = user_agent;
                    s
                }
                Err(e) => {
                    error.set_label(&e.to_string());
                    error.set_visible(true);
                    return;
                }
            };
            spinner.set_visible(true);
            content.set_sensitive(false);
            let client = Client::new(session);
            let error = error.clone();
            let spinner = spinner.clone();
            let content = content.clone();
            let on_success = on_success.clone();
            glib::spawn_future_local(async move {
                let c = client.clone();
                let res = rt::bg(async move {
                    let uid = c.user_id();
                    c.user_info(&uid).await
                })
                .await;
                spinner.set_visible(false);
                content.set_sensitive(true);
                match res {
                    Ok(user) => {
                        let mut session = client.session();
                        session.username = user.username;
                        if let Err(e) = session.save() {
                            eprintln!("finsta: could not save session: {e:#}");
                        }
                        on_success(session);
                    }
                    // The login itself may well be fine; Instagram just isn't
                    // answering right now. Signing in again would only add
                    // another fresh session, so keep this one.
                    Err(e) if crate::api::is_rate_limited(&e) || e.downcast_ref::<reqwest::Error>().is_some() => {
                        eprintln!("finsta: couldn't check the new session ({e:#}); keeping it");
                        let session = client.session();
                        if let Err(e) = session.save() {
                            eprintln!("finsta: could not save session: {e:#}");
                        }
                        on_success(session);
                    }
                    Err(e) => {
                        eprintln!("finsta: sign-in check failed: {e:#}");
                        let msg = if crate::api::is_login_required(&e) {
                            "Instagram didn't accept this session. Sign in again to get a fresh one.".to_string()
                        } else {
                            e.to_string()
                        };
                        error.set_label(&msg);
                        error.set_visible(true);
                    }
                }
            });
        })
    };

    #[cfg(feature = "webkit")]
    {
        let button = gtk::Button::builder()
            .label("Sign In with Instagram")
            .css_classes(["pill", "suggested-action"])
            .halign(gtk::Align::Center)
            .build();
        let finish = finish.clone();
        let window = window.clone();
        button.connect_clicked(move |_| {
            let finish = finish.clone();
            crate::ui::weblogin::open(&window, move |cookies, ua| finish(cookies, ua));
        });
        content.append(&button);
        content.append(
            &gtk::Label::builder()
                .label("You'll sign in on Instagram's own login page, including two-factor authentication. Finsta never sees your password.")
                .wrap(true)
                .justify(gtk::Justification::Center)
                .css_classes(["caption", "dim-label"])
                .build(),
        );
    }
    #[cfg(not(feature = "webkit"))]
    let _ = window;

    let group = adw::PreferencesGroup::builder()
        .title("Use a Session Cookie")
        .description("Sign in to instagram.com in a browser, open its developer tools and copy the values of the “sessionid” and “csrftoken” cookies.")
        .build();
    let sessionid = adw::PasswordEntryRow::builder().title("sessionid").build();
    let csrftoken = adw::EntryRow::builder().title("csrftoken (optional)").build();
    group.add(&sessionid);
    group.add(&csrftoken);
    content.append(&group);
    let cookie_button = gtk::Button::builder()
        .label("Continue")
        .css_classes(["pill"])
        .halign(gtk::Align::Center)
        .build();
    {
        let finish = finish.clone();
        let sid = sessionid.clone();
        let csrf_row = csrftoken.clone();
        let submit = Rc::new(move || {
            let mut cookies = BTreeMap::new();
            // Accept a pasted "sessionid=..." or "csrftoken=...; ..." as well.
            let value = |text: glib::GString, name: &str| {
                let t = text.trim();
                let t = t.strip_prefix(name).and_then(|r| r.strip_prefix('=')).unwrap_or(t);
                t.split(';').next().unwrap_or_default().trim().trim_matches('"').to_string()
            };
            cookies.insert("sessionid".to_string(), value(sid.text(), "sessionid"));
            let csrf = value(csrf_row.text(), "csrftoken");
            if !csrf.is_empty() {
                cookies.insert("csrftoken".to_string(), csrf);
            }
            finish(cookies, String::new());
        });
        let s = submit.clone();
        cookie_button.connect_clicked(move |_| s());
        let s = submit.clone();
        sessionid.connect_entry_activated(move |_| s());
        csrftoken.connect_entry_activated(move |_| submit());
    }
    content.append(&cookie_button);
    content.append(&spinner);
    content.append(&error);

    let status = adw::StatusPage::builder()
        .icon_name("camera-photo-symbolic")
        .title("Finsta")
        .description("A calm Instagram client: your direct messages, plus posts and stories from people you follow. No reels, no explore, no endless suggestions.")
        .child(&adw::Clamp::builder().maximum_size(440).child(&content).build())
        .build();
    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&status)
        .vexpand(true)
        .build();
    let toolbar = adw::ToolbarView::new();
    let header = adw::HeaderBar::builder().show_title(false).build();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&scroller));
    toolbar.upcast()
}
