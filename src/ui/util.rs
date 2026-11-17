use std::rc::Rc;

use adw::prelude::*;
use chrono::{Datelike, Local, TimeZone};
use gtk::glib;

use crate::api::User;
use crate::ui::images;

/// "now", "5m", "3h", "2d", "4w", or a date for anything older.
pub fn relative_time(ts_secs: i64) -> String {
    if ts_secs <= 0 {
        return String::new();
    }
    let diff = Local::now().timestamp() - ts_secs;
    match diff {
        d if d < 60 => "now".into(),
        d if d < 3600 => format!("{}m", d / 60),
        d if d < 86_400 => format!("{}h", d / 3600),
        d if d < 7 * 86_400 => format!("{}d", d / 86_400),
        d if d < 30 * 86_400 => format!("{}w", d / (7 * 86_400)),
        _ => Local
            .timestamp_opt(ts_secs, 0)
            .single()
            .map(|t| t.format("%-d %b %Y").to_string())
            .unwrap_or_default(),
    }
}

/// A label for a gap in a conversation: "Today 14:02", "Mon 09:13", "3 Nov 2026, 18:40".
pub fn chat_time(ts_secs: i64) -> String {
    let Some(t) = Local.timestamp_opt(ts_secs, 0).single() else {
        return String::new();
    };
    let now = Local::now();
    let days = (now.date_naive() - t.date_naive()).num_days();
    if days == 0 {
        format!("Today {}", t.format("%H:%M"))
    } else if days == 1 {
        format!("Yesterday {}", t.format("%H:%M"))
    } else if days < 7 {
        t.format("%a %H:%M").to_string()
    } else if t.year() == now.year() {
        t.format("%-d %b, %H:%M").to_string()
    } else {
        t.format("%-d %b %Y, %H:%M").to_string()
    }
}

pub fn full_time(ts_secs: i64) -> String {
    Local
        .timestamp_opt(ts_secs, 0)
        .single()
        .map(|t| t.format("%A %-d %B %Y, %H:%M").to_string())
        .unwrap_or_default()
}

/// 1234 -> "1,234"; 15300 -> "15.3K"; 2_100_000 -> "2.1M"
pub fn count(n: i64) -> String {
    match n {
        n if n >= 1_000_000 => format!("{:.1}M", n as f64 / 1_000_000.0).replace(".0M", "M"),
        n if n >= 10_000 => format!("{:.1}K", n as f64 / 1_000.0).replace(".0K", "K"),
        n if n >= 1_000 => format!("{},{:03}", n / 1000, n % 1000),
        n => n.to_string(),
    }
}

pub fn plural(n: i64, one: &str, many: &str) -> String {
    format!("{} {}", count(n), if n == 1 { one } else { many })
}

pub fn avatar(user: &User, size: i32) -> adw::Avatar {
    let avatar = adw::Avatar::new(size, Some(&user.display_name()), true);
    images::load_avatar(&avatar, Some(user.profile_pic_url.clone()));
    avatar
}

pub fn clear_box(b: &gtk::Box) {
    while let Some(child) = b.first_child() {
        b.remove(&child);
    }
}

pub fn open_uri(widget: &impl IsA<gtk::Widget>, uri: &str) {
    let window = widget.root().and_downcast::<gtk::Window>();
    gtk::UriLauncher::new(uri).launch(window.as_ref(), gtk::gio::Cancellable::NONE, |_| {});
}

/// Escape `text` for Pango markup and turn URLs into links.
pub fn linkify(text: &str) -> String {
    thread_local! {
        static RE: regex::Regex = regex::Regex::new(r"https?://[^\s<>]+").unwrap();
    }
    RE.with(|re| {
        let mut out = String::new();
        let mut last = 0;
        for m in re.find_iter(text) {
            out.push_str(&glib::markup_escape_text(&text[last..m.start()]));
            let url = glib::markup_escape_text(m.as_str());
            out.push_str(&format!("<a href=\"{url}\">{url}</a>"));
            last = m.end();
        }
        out.push_str(&glib::markup_escape_text(&text[last..]));
        out
    })
}

pub fn spinner_box() -> gtk::Box {
    let b = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .halign(gtk::Align::Center)
        .margin_top(24)
        .margin_bottom(24)
        .build();
    let spinner = adw::Spinner::new();
    spinner.set_size_request(32, 32);
    b.append(&spinner);
    b
}

/// Attach `state` to `obj` so it lives exactly as long as the widget does.
/// Closures inside the widget tree should only hold `Weak` references to it.
pub fn attach<T: 'static>(obj: &impl IsA<glib::Object>, state: Rc<T>) {
    // SAFETY: the key is unique to this function and only ever stores `Rc<T>`
    // values that are never read back.
    unsafe {
        obj.as_ref().set_data::<Rc<T>>("finsta-state", state);
    }
}

/// How much of `widget` (0 to 1) is on screen, inside its scrolled views.
pub fn visible_fraction(widget: &impl IsA<gtk::Widget>) -> f32 {
    let p = widget.as_ref();
    if !p.is_mapped() || p.height() <= 0 {
        return 0.0;
    }
    let Some(root) = p.root() else { return 0.0 };
    let root = root.upcast_ref::<gtk::Widget>();
    if root.downcast_ref::<gtk::Window>().is_some_and(|w| w.is_suspended() || !w.is_visible()) {
        return 0.0;
    }
    let Some(b) = p.compute_bounds(root) else { return 0.0 };
    let (mut top, mut bottom) = (0.0f32, root.height() as f32);
    let mut ancestor = p.parent();
    while let Some(w) = ancestor {
        if w.is::<gtk::ScrolledWindow>()
            && let Some(sb) = w.compute_bounds(root)
        {
            top = top.max(sb.y());
            bottom = bottom.min(sb.y() + sb.height());
        }
        ancestor = w.parent();
    }
    let visible = (b.y() + b.height()).min(bottom) - b.y().max(top);
    (visible / b.height()).clamp(0.0, 1.0)
}

type Waiting = (glib::WeakRef<gtk::Widget>, Box<dyn FnOnce()>);

thread_local! {
    static WAITING: std::cell::RefCell<Vec<Waiting>> = const { std::cell::RefCell::new(Vec::new()) };
    static WATCHING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Run `f` once, the first time `widget` scrolls into view (for loading
/// things only when they're looked at).
pub fn when_visible(widget: &impl IsA<gtk::Widget>, f: impl FnOnce() + 'static) {
    WAITING.with(|w| w.borrow_mut().push((widget.as_ref().downgrade(), Box::new(f))));
    if WATCHING.replace(true) {
        return;
    }
    glib::timeout_add_local(std::time::Duration::from_millis(400), || {
        let ready: Vec<Box<dyn FnOnce()>> = WAITING.with(|w| {
            let mut w = w.borrow_mut();
            w.retain(|(widget, _)| widget.upgrade().is_some());
            let mut ready = Vec::new();
            let mut i = 0;
            while i < w.len() {
                if w[i].0.upgrade().is_some_and(|x| visible_fraction(&x) > 0.15) {
                    ready.push(w.remove(i).1);
                } else {
                    i += 1;
                }
            }
            ready
        });
        for f in ready {
            f();
        }
        let more = WAITING.with(|w| !w.borrow().is_empty());
        if !more {
            WATCHING.set(false);
        }
        glib::ControlFlow::from(more)
    });
}
