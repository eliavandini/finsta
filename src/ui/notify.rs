//! Desktop notifications for new messages, while Finsta isn't the window
//! you're using (or runs in the background).

use std::rc::Rc;

use adw::prelude::*;
use gtk::gio;

use crate::api::{DmItem, Thread};
use crate::ui::prefs;
use crate::ui::window::Main;

fn id(thread_id: &str) -> String {
    format!("dm-{thread_id}")
}

/// Whether a new message in `thread` should notify now.
pub fn wanted(main: &Main, thread: &Thread) -> bool {
    let p = prefs::get();
    p.notify_messages && !thread.muted && !prefs::is_quiet(&thread.thread_id) && !main.window.is_active()
}

/// Notify about `item`, the newest message in `thread`. Clicking the
/// notification opens the conversation; a newer one replaces it.
pub fn message(main: &Rc<Main>, thread: &Thread, item: &DmItem) {
    let Some(app) = main.window.application() else { return };
    let sender = thread.user(&item.user_id).map(|u| u.display_name());
    let summary = item.content().summary();
    let (title, body) = if thread.is_group_chat() {
        (thread.title(), format!("{}: {summary}", sender.unwrap_or_else(|| "Someone".into())))
    } else {
        (sender.unwrap_or_else(|| thread.title()), summary)
    };
    let n = gio::Notification::new(&title);
    n.set_body(Some(&body));
    n.set_priority(gio::NotificationPriority::High);
    n.set_default_action_and_target_value("app.open-thread", Some(&thread.thread_id.to_variant()));
    app.send_notification(Some(&id(&thread.thread_id)), &n);
    eprintln!("finsta: notify: new message notification sent");
}

/// Take back a conversation's notification (it was opened).
pub fn withdraw(main: &Main, thread_id: &str) {
    if let Some(app) = main.window.application() {
        app.withdraw_notification(&id(thread_id));
    }
}
