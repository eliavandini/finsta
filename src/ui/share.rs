//! Sharing a post, reel or story with people you message, or copying its
//! link. It's sent as a link (Instagram shows a preview of it); sending it
//! the way the app does needs a request format Finsta hasn't learned yet.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, glib};

use crate::api::{self, Recipient, Thread, User};
use crate::rt;
use crate::ui::util;
use crate::ui::window::Main;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Post,
    Reel,
    Story,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Kind::Post => "post",
            Kind::Reel => "reel",
            Kind::Story => "story",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Shared {
    pub url: String,
    pub kind: Kind,
}

/// Where messages go: Finsta's toasts, or the story viewer's notices.
pub type Notify = Rc<dyn Fn(&str)>;

#[derive(Clone)]
enum Target {
    Thread(Thread),
    User(User),
}

impl Target {
    fn key(&self) -> String {
        match self {
            Target::Thread(t) => format!("t:{}", t.thread_id),
            Target::User(u) => format!("u:{}", u.pk),
        }
    }

    fn recipient(&self) -> Recipient {
        match self {
            Target::Thread(t) => Recipient::Thread(t.thread_id.clone()),
            Target::User(u) => Recipient::User(u.pk.clone()),
        }
    }
}

pub fn button(main: &Rc<Main>, shared: Shared) -> gtk::Button {
    let button = gtk::Button::builder()
        .icon_name("share-symbolic")
        .tooltip_text(format!("Share This {}", capitalized(shared.kind.name())))
        .css_classes(["flat", "circular"])
        .build();
    let weak = Rc::downgrade(main);
    button.connect_clicked(move |_| {
        if let Some(main) = weak.upgrade() {
            open(&main, shared.clone(), None, None);
        }
    });
    button
}

fn capitalized(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
}

struct Sheet {
    main: std::rc::Weak<Main>,
    list: gtk::ListBox,
    send: gtk::Button,
    shown: RefCell<Vec<Target>>,
    selected: RefCell<Vec<Target>>,
    generation: std::cell::Cell<u64>,
}

impl Sheet {
    fn render(self: &Rc<Self>) {
        self.list.remove_all();
        let shown = self.shown.borrow();
        for target in shown.iter() {
            let row = adw::ActionRow::builder().title_lines(1).use_markup(false).activatable(true).build();
            match target {
                Target::Thread(t) => {
                    row.set_title(&t.title());
                    if t.is_group_chat() {
                        row.set_subtitle(&format!("Group · {} members", t.users.len() + 1));
                        let a = adw::Avatar::new(36, Some(&t.title()), false);
                        a.set_icon_name(Some("system-users-symbolic"));
                        row.add_prefix(&a);
                    } else if let Some(u) = t.users.first() {
                        row.set_subtitle(&format!("@{}", u.username));
                        row.add_prefix(&util::avatar(u, 36));
                    }
                }
                Target::User(u) => {
                    row.set_title(&u.display_name());
                    row.set_subtitle(&format!("@{}", u.username));
                    row.add_prefix(&util::avatar(u, 36));
                }
            }
            let key = target.key();
            let check = gtk::CheckButton::builder()
                .active(self.selected.borrow().iter().any(|s| s.key() == key))
                .valign(gtk::Align::Center)
                .build();
            check.add_css_class("selection-mode");
            row.add_suffix(&check);
            row.set_activatable_widget(Some(&check));
            let weak = Rc::downgrade(self);
            let target = target.clone();
            check.connect_toggled(move |c| {
                let Some(sheet) = weak.upgrade() else { return };
                let key = target.key();
                let mut selected = sheet.selected.borrow_mut();
                selected.retain(|s| s.key() != key);
                if c.is_active() {
                    selected.push(target.clone());
                }
                let n = selected.len();
                drop(selected);
                sheet.send.set_sensitive(n > 0);
                sheet.send.set_label(if n > 1 { "Send Separately" } else { "Send" });
            });
            self.list.append(&row);
        }
        if shown.is_empty() {
            self.list.append(
                &gtk::Label::builder()
                    .label("No one found")
                    .css_classes(["dim-label"])
                    .margin_top(18)
                    .margin_bottom(18)
                    .build(),
            );
        }
    }

    fn search(self: &Rc<Self>, query: &str) {
        self.generation.set(self.generation.get() + 1);
        let Some(main) = self.main.upgrade() else { return };
        let q = query.trim().to_lowercase();
        let threads = main.sidebar.get().map(|s| s.threads()).unwrap_or_default();
        let mut shown: Vec<Target> = threads
            .into_iter()
            .filter(|t| {
                q.is_empty()
                    || t.title().to_lowercase().contains(&q)
                    || t.users.iter().any(|u| {
                        u.username.to_lowercase().contains(&q) || u.full_name.to_lowercase().contains(&q)
                    })
            })
            .take(if q.is_empty() { 20 } else { 8 })
            .map(Target::Thread)
            .collect();
        // Keep what's selected in view.
        for s in self.selected.borrow().iter() {
            if !shown.iter().any(|t| t.key() == s.key()) {
                shown.insert(0, s.clone());
            }
        }
        self.shown.replace(shown);
        self.render();
        if q.is_empty() || q.contains(' ') {
            return;
        }
        let generation = self.generation.get();
        let client = main.client.clone();
        let query = query.trim().to_string();
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let res = rt::bg(async move { client.search_contacts(&query).await }).await;
            let Some(sheet) = weak.upgrade() else { return };
            if sheet.generation.get() != generation {
                return;
            }
            match res {
                Ok(users) => {
                    let mut shown = sheet.shown.borrow_mut();
                    for u in users.into_iter().take(12) {
                        let known = shown.iter().any(|t| match t {
                            Target::Thread(t) => !t.is_group_chat() && t.users.iter().any(|x| x.pk == u.pk),
                            Target::User(x) => x.pk == u.pk,
                        });
                        if !known {
                            shown.push(Target::User(u));
                        }
                    }
                    drop(shown);
                    sheet.render();
                }
                Err(e) => eprintln!("finsta: share: people search failed: {e}"),
            }
        });
    }
}

/// The share sheet. `notify` reports what happened (Finsta's toasts by
/// default); `on_closed` runs when it closes.
pub fn open(main: &Rc<Main>, shared: Shared, notify: Option<Notify>, on_closed: Option<Box<dyn Fn()>>) {
    let notify: Notify = notify.unwrap_or_else(|| {
        let weak = Rc::downgrade(main);
        Rc::new(move |text: &str| {
            if let Some(main) = weak.upgrade() {
                main.toast(text);
            }
        })
    });
    let search = gtk::SearchEntry::builder()
        .placeholder_text("Search people and groups")
        .margin_top(12)
        .margin_start(12)
        .margin_end(12)
        .build();
    let list = gtk::ListBox::builder()
        .css_classes(["boxed-list"])
        .selection_mode(gtk::SelectionMode::None)
        .valign(gtk::Align::Start)
        .margin_top(12)
        .margin_bottom(12)
        .margin_start(12)
        .margin_end(12)
        .build();
    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&list)
        .build();
    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    content.append(&search);
    content.append(&scroller);

    let message = gtk::Entry::builder()
        .placeholder_text("Write a message…")
        .hexpand(true)
        .build();
    let send = gtk::Button::builder()
        .label("Send")
        .css_classes(["suggested-action"])
        .sensitive(false)
        .build();
    let bottom = gtk::Box::builder().spacing(8).css_classes(["composer"]).build();
    bottom.append(&message);
    bottom.append(&send);

    let copy = gtk::Button::builder()
        .icon_name("edit-copy-symbolic")
        .tooltip_text("Copy Link")
        .build();
    let header = adw::HeaderBar::new();
    header.pack_start(&copy);
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&content));
    toolbar.add_bottom_bar(&bottom);
    let dialog = adw::Dialog::builder()
        .title(format!("Share {}", capitalized(shared.kind.name())))
        .content_width(420)
        .content_height(600)
        .child(&toolbar)
        .build();
    if let Some(f) = on_closed {
        dialog.connect_closed(move |_| f());
    }

    let sheet = Rc::new(Sheet {
        main: Rc::downgrade(main),
        list,
        send: send.clone(),
        shown: RefCell::new(Vec::new()),
        selected: RefCell::new(Vec::new()),
        generation: std::cell::Cell::new(0),
    });
    sheet.search("");
    let weak = Rc::downgrade(&sheet);
    search.connect_search_changed(move |e| {
        if let Some(s) = weak.upgrade() {
            s.search(&e.text());
        }
    });

    {
        let url = shared.url.clone();
        let notify = notify.clone();
        let dialog = dialog.downgrade();
        copy.connect_clicked(move |b| {
            b.clipboard().set_text(&url);
            if let Some(d) = dialog.upgrade() {
                d.close();
            }
            notify("Link copied");
        });
    }

    let do_send = {
        let sheet = sheet.clone();
        let dialog = dialog.downgrade();
        let message = message.clone();
        let main = Rc::downgrade(main);
        move || {
            let targets = sheet.selected.borrow().clone();
            if targets.is_empty() || !sheet.send.is_sensitive() {
                return;
            }
            let Some(main) = main.upgrade() else { return };
            sheet.send.set_sensitive(false);
            message.set_sensitive(false);
            let text = message.text().trim().to_string();
            let client = main.client.clone();
            let url = shared.url.clone();
            let kind = shared.kind;
            let notify = notify.clone();
            let dialog = dialog.clone();
            let weak = Rc::downgrade(&main);
            glib::spawn_future_local(async move {
                let total = targets.len();
                let recipients: Vec<Recipient> = targets.iter().map(Target::recipient).collect();
                let res = rt::bg(async move {
                    let mut sent = 0;
                    let mut last_err = None;
                    for to in &recipients {
                        let r = async {
                            client.send_text(to, &url, None).await?;
                            if !text.is_empty() {
                                client.send_text(to, &text, None).await?;
                            }
                            anyhow::Ok(())
                        }
                        .await;
                        match r {
                            Ok(()) => sent += 1,
                            Err(e) => last_err = Some(e),
                        }
                    }
                    (sent, last_err)
                })
                .await;
                let (sent, err) = res;
                eprintln!("finsta: share: sent a {} link to {sent} of {total} conversations", kind.name());
                if let Some(d) = dialog.upgrade() {
                    d.close();
                }
                if let Some(e) = &err {
                    eprintln!("finsta: share failed: {e}");
                    if api::is_login_required(e)
                        && let Some(main) = weak.upgrade()
                    {
                        main.handle_error(e);
                        return;
                    }
                }
                notify(&match (sent, total) {
                    (0, _) => "Couldn't send it".to_string(),
                    (s, t) if s == t && t == 1 => "Sent".to_string(),
                    (s, t) if s == t => format!("Sent to {t} conversations"),
                    (s, t) => format!("Sent to {s} of {t} conversations"),
                });
            });
        }
    };
    let do_send = Rc::new(do_send);
    let f = do_send.clone();
    send.connect_clicked(move |_| f());
    message.connect_activate(move |_| do_send());

    // Escape in the search field closes the sheet, as elsewhere.
    let keys = gtk::EventControllerKey::new();
    let d = dialog.downgrade();
    keys.connect_key_pressed(move |_, key, _, _| {
        if key == gdk::Key::Escape
            && let Some(d) = d.upgrade()
        {
            d.close();
            return glib::Propagation::Stop;
        }
        glib::Propagation::Proceed
    });
    search.add_controller(keys);

    dialog.present(Some(&main.window));
    search.grab_focus();
}
