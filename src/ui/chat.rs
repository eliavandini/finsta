//! A conversation (1:1 or group). Older messages load only when you press
//! "Load Older Messages"; new ones are polled while the chat is open.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::{Rc, Weak};

use adw::prelude::*;
use gtk::glib;
use serde_json::Value;

use crate::api::{Disappearing, DmContent, DmItem, Media, Recipient, Thread, User};
use crate::rt;
use crate::ui::menu::{self, Menu};
use crate::ui::ratio_bin::RatioBin;
use crate::ui::util;
use crate::ui::window::Main;
use crate::ui::media_viewer::{self, Ephemeral};
use crate::ui::images;
use crate::ui::video;

fn api_login_required(e: &anyhow::Error) -> bool {
    crate::api::is_login_required(e)
}

pub enum ChatTarget {
    Thread(Thread),
    /// Someone you may not have a conversation with yet.
    User(User),
}

struct Chat {
    main: Weak<Main>,
    viewer_id: String,
    page: glib::WeakRef<adw::NavigationPage>,
    thread_id: RefCell<Option<String>>,
    thread: RefCell<Thread>,
    /// Ascending by time.
    items: RefCell<Vec<DmItem>>,
    ids: RefCell<HashSet<String>>,
    pending: RefCell<Vec<DmItem>>,
    oldest_cursor: RefCell<Option<String>>,
    has_older: Cell<bool>,
    header_avatar: gtk::Box,
    title: gtk::Label,
    subtitle: gtk::Label,
    members_popover: gtk::Popover,
    scroller: gtk::ScrolledWindow,
    list: gtk::Box,
    older_button: gtk::Button,
    empty: gtk::Label,
    entry: gtk::Entry,
    send_button: gtk::Button,
    reply_bar: gtk::Box,
    reply_label: gtk::Label,
    /// The message the next one you send replies to.
    reply_to: RefCell<Option<(String, String)>>,
    loading: Cell<bool>,
    sending: Cell<bool>,
    stick_to_bottom: Cell<bool>,
    keep_distance_from_bottom: Cell<Option<f64>>,
    last_marked: RefCell<String>,
    /// New messages arrived while you weren't looking: mark them read when
    /// you are (so they still notify you and show as unread).
    seen_pending: Cell<bool>,
    active_handler: RefCell<Option<glib::SignalHandlerId>>,
    /// When the newest messages were last asked for.
    fetched: Cell<Option<std::time::Instant>>,
    /// The last sign of a live conversation (typing, sending, a new message,
    /// coming back to it): new messages are checked for faster after it.
    active_at: Cell<std::time::Instant>,
    /// Disappearing messages opened in this session: item id -> times opened.
    /// Guards against showing them again before Instagram's state catches up.
    local_views: RefCell<HashMap<String, u32>>,
    opening: Cell<bool>,
}

impl Drop for Chat {
    fn drop(&mut self) {
        if let (Some(id), Some(main)) = (self.active_handler.take(), self.main.upgrade()) {
            main.window.disconnect(id);
        }
    }
}

enum VisualState {
    View,
    Replay,
    Opened,
    Gone,
}

pub fn page(main: &Rc<Main>, target: ChatTarget) -> adw::NavigationPage {
    // Header strip
    let header = gtk::Box::builder().spacing(10).css_classes(["chat-header"]).build();
    let who = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    let header_avatar = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    let names = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .valign(gtk::Align::Center)
        .build();
    let title = gtk::Label::builder()
        .xalign(0.0)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .css_classes(["heading"])
        .build();
    let subtitle = gtk::Label::builder()
        .xalign(0.0)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .css_classes(["caption", "dim-label"])
        .build();
    names.append(&title);
    names.append(&subtitle);
    who.append(&header_avatar);
    who.append(&names);
    let who_button = gtk::MenuButton::builder()
        .child(&who)
        .css_classes(["flat"])
        .hexpand(false)
        .always_show_arrow(false)
        .build();
    let members_popover = gtk::Popover::new();
    who_button.set_popover(Some(&members_popover));
    header.append(&who_button);
    header.append(&gtk::Box::builder().hexpand(true).build());
    let refresh = gtk::Button::builder()
        .icon_name("view-refresh-symbolic")
        .tooltip_text("Check for New Messages")
        .css_classes(["flat", "circular"])
        .valign(gtk::Align::Center)
        .build();
    header.append(&refresh);

    // Messages
    let older_button = gtk::Button::builder()
        .label("Load Older Messages")
        .css_classes(["pill", "small"])
        .halign(gtk::Align::Center)
        .margin_bottom(8)
        .visible(false)
        .build();
    let list = gtk::Box::new(gtk::Orientation::Vertical, 3);
    let empty = gtk::Label::builder()
        .css_classes(["dim-label"])
        .margin_top(48)
        .wrap(true)
        .visible(false)
        .build();
    let column = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .margin_top(12)
        .margin_bottom(12)
        .margin_start(12)
        .margin_end(12)
        .valign(gtk::Align::End)
        .build();
    column.append(&older_button);
    column.append(&empty);
    column.append(&list);
    let clamp = adw::Clamp::builder().maximum_size(780).child(&column).build();
    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&clamp)
        .build();

    // Composer
    let entry = gtk::Entry::builder()
        .placeholder_text("Message…")
        .hexpand(true)
        // Type ":" and a name to search emoji.
        .enable_emoji_completion(true)
        .build();
    let send_button = gtk::Button::builder()
        .icon_name("mail-send-symbolic")
        .tooltip_text("Send")
        .css_classes(["suggested-action", "circular"])
        .build();
    let composer_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    composer_row.append(&entry);
    composer_row.append(&send_button);
    let reply_label = gtk::Label::builder()
        .xalign(0.0)
        .hexpand(true)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .css_classes(["caption", "dim-label"])
        .build();
    let reply_cancel = gtk::Button::builder()
        .icon_name("window-close-symbolic")
        .tooltip_text("Don't Reply")
        .css_classes(["flat", "circular"])
        .build();
    let reply_bar = gtk::Box::builder().spacing(6).visible(false).build();
    reply_bar.append(&reply_label);
    reply_bar.append(&reply_cancel);
    let composer_column = gtk::Box::new(gtk::Orientation::Vertical, 4);
    composer_column.append(&reply_bar);
    composer_column.append(&composer_row);
    let composer = adw::Clamp::builder()
        .maximum_size(780)
        .child(&composer_column)
        .css_classes(["composer"])
        .build();

    let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
    body.append(&header);
    body.append(&scroller);
    body.append(&composer);

    let page = adw::NavigationPage::builder().title("Conversation").child(&body).build();

    let (thread, thread_id) = match &target {
        ChatTarget::Thread(t) => (t.clone(), Some(t.thread_id.clone())),
        ChatTarget::User(u) => (
            Thread {
                users: vec![u.clone()],
                ..Default::default()
            },
            None,
        ),
    };

    let chat = Rc::new(Chat {
        main: Rc::downgrade(main),
        viewer_id: main.client.user_id(),
        page: page.downgrade(),
        thread_id: RefCell::new(thread_id),
        thread: RefCell::new(thread.clone()),
        items: RefCell::new(Vec::new()),
        ids: RefCell::new(HashSet::new()),
        pending: RefCell::new(Vec::new()),
        oldest_cursor: RefCell::new(None),
        has_older: Cell::new(false),
        header_avatar,
        title,
        subtitle,
        members_popover,
        scroller,
        list,
        older_button,
        empty,
        entry,
        send_button,
        reply_bar,
        reply_label,
        reply_to: RefCell::new(None),
        loading: Cell::new(false),
        sending: Cell::new(false),
        stick_to_bottom: Cell::new(true),
        keep_distance_from_bottom: Cell::new(None),
        last_marked: RefCell::new(String::new()),
        seen_pending: Cell::new(false),
        active_handler: RefCell::new(None),
        fetched: Cell::new(None),
        active_at: Cell::new(std::time::Instant::now()),
        local_views: RefCell::new(HashMap::new()),
        opening: Cell::new(false),
    });

    composer_row.prepend(&gif_button(&chat));
    let weak = Rc::downgrade(&chat);
    reply_cancel.connect_clicked(move |_| {
        if let Some(c) = weak.upgrade() {
            c.set_reply(None);
        }
    });
    chat.render_header();
    chat.connect(&refresh);
    // Show the preview messages from the inbox right away.
    chat.merge(thread.items.clone());
    chat.render();

    match target {
        ChatTarget::Thread(_) => chat.load_latest(true),
        ChatTarget::User(user) => chat.find_thread(user),
    }
    chat.start_polling();
    util::attach(&page, chat);
    page
}

impl Chat {
    fn connect(self: &Rc<Self>, refresh: &gtk::Button) {
        let weak = Rc::downgrade(self);
        if let Some(main) = self.main.upgrade() {
            let w = weak.clone();
            let id = main.window.connect_is_active_notify(move |_| {
                if let Some(c) = w.upgrade() {
                    c.back_in_view();
                }
            });
            self.active_handler.replace(Some(id));
        }
        refresh.connect_clicked(glib::clone!(
            #[strong]
            weak,
            move |_| {
                if let Some(c) = weak.upgrade() {
                    c.load_latest(false);
                }
            }
        ));
        self.older_button.connect_clicked(glib::clone!(
            #[strong]
            weak,
            move |_| {
                if let Some(c) = weak.upgrade() {
                    c.load_older();
                }
            }
        ));
        self.entry.connect_changed(glib::clone!(
            #[strong]
            weak,
            move |_| {
                if let Some(c) = weak.upgrade() {
                    c.mark_active();
                }
            }
        ));
        self.entry.connect_activate(glib::clone!(
            #[strong]
            weak,
            move |_| {
                if let Some(c) = weak.upgrade() {
                    c.send();
                }
            }
        ));
        self.send_button.connect_clicked(glib::clone!(
            #[strong]
            weak,
            move |_| {
                if let Some(c) = weak.upgrade() {
                    c.send();
                }
            }
        ));

        let adj = self.scroller.vadjustment();
        adj.connect_value_changed(glib::clone!(
            #[strong]
            weak,
            move |adj| {
                if let Some(c) = weak.upgrade() {
                    let distance = adj.upper() - adj.value() - adj.page_size();
                    c.stick_to_bottom.set(distance < 48.0);
                }
            }
        ));
        // Content size changed (new messages, images): keep the view anchored.
        adj.connect_changed(glib::clone!(
            #[strong]
            weak,
            move |adj| {
                let Some(c) = weak.upgrade() else { return };
                let before = adj.value();
                if let Some(distance) = c.keep_distance_from_bottom.take() {
                    adj.set_value(adj.upper() - adj.page_size() - distance);
                } else if c.stick_to_bottom.get() {
                    adj.set_value(adj.upper() - adj.page_size());
                }
                // This runs after the list was placed for the old position,
                // and a new placement asked for now gets lost: ask again
                // once this layout pass is over.
                if adj.value() != before
                    && let Some(viewport) = c.scroller.child()
                {
                    glib::idle_add_local_once(move || viewport.queue_allocate());
                }
            }
        ));
        if let Some(page) = self.page.upgrade() {
            page.connect_shown(glib::clone!(
                #[strong]
                weak,
                move |_| {
                    if let Some(c) = weak.upgrade() {
                        c.entry.grab_focus();
                        c.back_in_view();
                    }
                }
            ));
        }
    }

    fn start_polling(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        // Ticks every second but asks Instagram only when a check is due, so
        // coming back to the conversation or a new message speeds it up at once.
        glib::timeout_add_seconds_local(1, move || {
            let Some(c) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            let Some(page) = c.page.upgrade() else {
                return glib::ControlFlow::Break;
            };
            let due = c.fetched.get().is_none_or(|t| t.elapsed().as_secs() >= c.poll_delay());
            if due && page.is_mapped() && !c.loading.get() && c.thread_id.borrow().is_some() {
                c.load_latest(false);
            }
            glib::ControlFlow::Continue
        });
    }

    /// Seconds until the next check for new messages: often while you're in
    /// a live conversation, less as it goes quiet or you look elsewhere.
    fn poll_delay(&self) -> u64 {
        let quiet = self.active_at.get().elapsed().as_secs();
        match quiet {
            _ if !self.in_view() => 15,
            0..120 => 3,
            120..600 => 8,
            _ => 15,
        }
    }

    fn mark_active(&self) {
        self.active_at.set(std::time::Instant::now());
    }

    fn render_header(self: &Rc<Self>) {
        let thread = self.thread.borrow().clone();
        util::clear_box(&self.header_avatar);
        if thread.is_group_chat() {
            let a = adw::Avatar::new(36, Some(&thread.title()), false);
            a.set_icon_name(Some("system-users-symbolic"));
            self.header_avatar.append(&a);
            self.subtitle.set_label(&format!("{} members", thread.users.len() + 1));
        } else if let Some(u) = thread.users.first() {
            self.header_avatar.append(&util::avatar(u, 36));
            self.subtitle.set_label(&format!("@{}", u.username));
        }
        self.title.set_label(&thread.title());
        if let Some(page) = self.page.upgrade() {
            page.set_title(&thread.title());
        }

        // Popover with the people in this conversation.
        let list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .css_classes(["navigation-sidebar"])
            .build();
        for u in &thread.users {
            let row = gtk::Box::builder().spacing(8).margin_top(4).margin_bottom(4).build();
            row.append(&util::avatar(u, 28));
            let label = gtk::Label::builder()
                .label(format!("{} (@{})", u.display_name(), u.username))
                .xalign(0.0)
                .build();
            row.append(&label);
            list.append(&row);
        }
        let users = thread.users.clone();
        let weak = Rc::downgrade(self);
        list.connect_row_activated(move |_, row| {
            let Some(c) = weak.upgrade() else { return };
            c.members_popover.popdown();
            if let (Some(user), Some(main)) = (users.get(row.index() as usize), c.main.upgrade()) {
                main.open_profile(user.clone());
            }
        });
        let heading = gtk::Label::builder()
            .label("View Profile")
            .css_classes(["heading"])
            .margin_top(6)
            .margin_bottom(4)
            .build();
        let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
        column.append(&heading);
        column.append(&list);
        self.members_popover.set_child(Some(&column));
    }

    fn find_thread(self: &Rc<Self>, user: User) {
        let Some(main) = self.main.upgrade() else { return };
        let client = main.client.clone();
        let weak = Rc::downgrade(self);
        self.loading.set(true);
        self.list.append(&util::spinner_box());
        glib::spawn_future_local(async move {
            let pk = user.pk.clone();
            let res = rt::bg(async move { client.thread_with_user(&pk).await }).await;
            let Some(c) = weak.upgrade() else { return };
            c.loading.set(false);
            match res {
                Ok(Some(thread)) => {
                    c.adopt_thread(thread, true);
                }
                Ok(None) => {
                    c.empty.set_label(&format!(
                        "No messages with @{} yet. Say hi!",
                        user.username
                    ));
                    c.render();
                }
                Err(e) => {
                    c.render();
                    if let Some(main) = c.main.upgrade() {
                        main.handle_error(&e);
                    }
                }
            }
        });
    }

    /// Take in a freshly fetched thread (metadata + newest messages).
    fn adopt_thread(self: &Rc<Self>, thread: Thread, initial: bool) {
        let was_known = self.thread_id.borrow().is_some();
        self.thread_id.replace(Some(thread.thread_id.clone()));
        if initial || !was_known {
            self.oldest_cursor.replace(thread.oldest_cursor.clone());
            self.has_older.set(thread.has_older);
        }
        let users_changed = {
            let current = self.thread.borrow();
            current.users.len() != thread.users.len() || current.title() != thread.title()
        };
        let mut meta = thread.clone();
        if meta.users.is_empty() {
            meta.users = self.thread.borrow().users.clone();
        }
        self.thread.replace(Thread {
            items: Vec::new(),
            ..meta.clone()
        });
        if users_changed || !was_known {
            self.render_header();
        }
        let changed = self.merge(thread.items);
        if changed && !initial {
            self.mark_active();
        }
        if changed || initial || !self.pending.borrow().is_empty() {
            self.pending.borrow_mut().clear();
            self.render();
        }
        if let Some(sidebar) = self.main.upgrade().and_then(|m| m.sidebar.get().cloned()) {
            let mut summary = meta;
            summary.items = self.items.borrow().iter().rev().take(5).cloned().collect();
            sidebar.update_thread(summary);
        }
        if self.in_view() {
            self.seen_now();
        } else {
            self.seen_pending.set(true);
        }
    }

    /// You're looking at the conversation again: mark what came meanwhile
    /// read, and ask for newer messages if it's been a while (it only asks
    /// every 15 seconds while on screen, and not at all while hidden).
    fn back_in_view(self: &Rc<Self>) {
        if !self.in_view() {
            return;
        }
        self.mark_active();
        if self.seen_pending.get() {
            self.seen_now();
        }
        if self.fetched.get().is_some_and(|t| t.elapsed() >= std::time::Duration::from_secs(10)) {
            self.load_latest(false);
        }
    }

    /// The conversation is on screen in the window you're using.
    fn in_view(&self) -> bool {
        let active = self.main.upgrade().is_some_and(|m| m.window.is_active());
        active && self.page.upgrade().is_some_and(|p| p.is_mapped())
    }

    /// Mark the newest messages read, here and on Instagram.
    fn seen_now(self: &Rc<Self>) {
        self.seen_pending.set(false);
        self.mark_seen();
        if let Some(sidebar) = self.main.upgrade().and_then(|m| m.sidebar.get().cloned())
            && let Some(id) = self.thread_id.borrow().as_deref()
        {
            sidebar.mark_read_local(id);
        }
    }

    /// Add new messages and update ones whose reactions or opened-state
    /// changed. Returns whether anything visible changed.
    fn merge(&self, items: Vec<DmItem>) -> bool {
        let mut changed = false;
        let mut list = self.items.borrow_mut();
        let mut ids = self.ids.borrow_mut();
        for item in items {
            // "snippet:" items are inbox previews, not real messages.
            if item.item_id.is_empty() || item.item_id.starts_with("snippet:") {
                continue;
            }
            if ids.insert(item.item_id.clone()) {
                list.push(item);
                changed = true;
            } else if let Some(existing) = list.iter_mut().find(|i| i.item_id == item.item_id)
                && existing.state_key() != item.state_key()
            {
                *existing = item;
                changed = true;
            }
        }
        list.sort_by_key(|i| i.timestamp);
        changed
    }

    fn visual_state(&self, item: &DmItem, d: &Disappearing) -> VisualState {
        let local = self.local_views.borrow().get(&item.item_id).copied().unwrap_or(0);
        let server = u32::from(d.seen_user_ids.contains(&self.viewer_id));
        let views = local.max(server);
        if d.is_available() && views < d.allowed_views() {
            if views == 0 { VisualState::View } else { VisualState::Replay }
        } else if views > 0 {
            VisualState::Opened
        } else {
            VisualState::Gone
        }
    }

    /// Open a disappearing photo or video. The view is reported to Instagram
    /// first (so the sender sees "Opened"), and nothing is shown unless that
    /// succeeds. Photos are downloaded before reporting, so an expired link
    /// doesn't use up the view.
    fn open_disappearing(self: &Rc<Self>, item_id: String) {
        let Some(thread_id) = self.thread_id.borrow().clone() else { return };
        let Some(item) = self.items.borrow().iter().find(|i| i.item_id == item_id).cloned() else {
            return;
        };
        let DmContent::Disappearing(d) = item.content() else { return };
        let replay = match self.visual_state(&item, &d) {
            VisualState::View => false,
            VisualState::Replay => true,
            _ => return,
        };
        if self.opening.replace(true) {
            return;
        }
        let Some(main) = self.main.upgrade() else { return };
        let client = main.client.clone();
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let content = match (&d.video, &d.image) {
                (Some(url), _) => Some(Ephemeral::Video(url.clone())),
                (None, Some(url)) => images::texture_uncached(url).await.map(Ephemeral::Photo),
                (None, None) => None,
            };
            let Some(c) = weak.upgrade() else { return };
            let Some(main) = c.main.upgrade() else { return };
            let Some(content) = content else {
                c.opening.set(false);
                main.toast(&format!("This {} couldn't be loaded. It may have expired.", d.kind()));
                return;
            };
            let id = item_id.clone();
            let res = rt::bg(async move { client.mark_visual_seen(&thread_id, &id, replay).await }).await;
            c.opening.set(false);
            if let Err(e) = res {
                if crate::api::is_login_required(&e) {
                    main.handle_error(&e);
                } else {
                    main.toast(&format!("Couldn't open the {}: Instagram didn't confirm the view. {e}", d.kind()));
                }
                return;
            }
            let views = match c.visual_state(&item, &d) {
                VisualState::View => 1,
                _ => d.allowed_views(),
            };
            c.local_views.borrow_mut().insert(item_id, views);
            // The clicked button is about to be rebuilt; park focus in the
            // composer so closing the viewer doesn't scroll to some other widget.
            c.entry.grab_focus();
            c.render();
            let note = if views < d.allowed_views() {
                "You can replay this once"
            } else {
                "You can't open this again after closing"
            };
            media_viewer::open_disappearing(&main, content, note);
        });
    }

    fn load_latest(self: &Rc<Self>, initial: bool) {
        let Some(id) = self.thread_id.borrow().clone() else { return };
        if self.loading.replace(true) {
            return;
        }
        self.fetched.set(Some(std::time::Instant::now()));
        let Some(main) = self.main.upgrade() else { return };
        let client = main.client.clone();
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let res = rt::bg(async move { client.thread(&id, None).await }).await;
            let Some(c) = weak.upgrade() else { return };
            c.loading.set(false);
            match res {
                Ok(thread) => c.adopt_thread(thread, initial),
                Err(e) => {
                    if let Some(main) = c.main.upgrade()
                        && (initial || crate::api::is_login_required(&e))
                    {
                        main.handle_error(&e);
                    }
                }
            }
        });
    }

    fn load_older(self: &Rc<Self>) {
        let Some(id) = self.thread_id.borrow().clone() else { return };
        let Some(cursor) = self.oldest_cursor.borrow().clone() else { return };
        if self.loading.replace(true) {
            return;
        }
        let Some(main) = self.main.upgrade() else { return };
        self.older_button.set_sensitive(false);
        let client = main.client.clone();
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let res = rt::bg(async move { client.thread(&id, Some(cursor)).await }).await;
            let Some(c) = weak.upgrade() else { return };
            c.loading.set(false);
            c.older_button.set_sensitive(true);
            match res {
                Ok(thread) => {
                    c.oldest_cursor.replace(thread.oldest_cursor.clone());
                    c.has_older.set(thread.has_older && thread.oldest_cursor.is_some());
                    let adj = c.scroller.vadjustment();
                    c.keep_distance_from_bottom
                        .set(Some(adj.upper() - adj.value() - adj.page_size()));
                    c.stick_to_bottom.set(false);
                    c.merge(thread.items);
                    c.render();
                }
                Err(e) => {
                    if let Some(main) = c.main.upgrade() {
                        main.handle_error(&e);
                    }
                }
            }
        });
    }

    fn mark_seen(self: &Rc<Self>) {
        let Some(id) = self.thread_id.borrow().clone() else { return };
        let Some(last) = self.items.borrow().last().cloned() else { return };
        if last.user_id == self.viewer_id || *self.last_marked.borrow() == last.item_id {
            return;
        }
        self.last_marked.replace(last.item_id.clone());
        let Some(main) = self.main.upgrade() else { return };
        let client = main.client.clone();
        rt::runtime().spawn(async move {
            if let Err(e) = client.mark_seen(&id, &last.item_id).await {
                eprintln!("finsta: could not mark as seen: {e:#}");
            }
        });
    }

    /// Reply to `item` with the next message you send (None: don't).
    fn set_reply(&self, item: Option<&DmItem>) {
        let reply = item.map(|i| (i.item_id.clone(), i.content().summary()));
        if let Some((_, quote)) = &reply {
            self.reply_label.set_label(&format!("Replying to: {quote}"));
            self.entry.grab_focus();
        }
        self.reply_bar.set_visible(reply.is_some());
        self.reply_to.replace(reply);
    }

    fn send(self: &Rc<Self>) {
        let text = self.entry.text().trim().to_string();
        if text.is_empty() {
            return;
        }
        let mut extra = serde_json::Map::new();
        extra.insert("text".into(), Value::String(text.clone()));
        let t = text.clone();
        let started = self.deliver("text", extra, text, move |client, to, reply| async move {
            client.send_text(&to, &t, reply.as_deref()).await
        });
        if started {
            self.entry.set_text("");
        }
    }

    fn send_gif(self: &Rc<Self>, gif: crate::api::Gif) {
        let mut extra = serde_json::Map::new();
        extra.insert("animated_media".into(), serde_json::json!({ "images": { "fixed_height": { "url": gif.still } } }));
        self.deliver("animated_media", extra, String::new(), move |client, to, reply| async move {
            client.send_gif(&to, &gif, reply.as_deref()).await
        });
    }

    /// Show a message right away (`item_type` and `extra` as Instagram sends
    /// it back), send it with `send` (as a reply if one is chosen), then fetch
    /// the conversation so it shows for real. If sending fails, `text` goes
    /// back in the message box. Whether it started.
    fn deliver<F, Fut>(self: &Rc<Self>, item_type: &str, mut extra: serde_json::Map<String, Value>, text: String, send: F) -> bool
    where
        F: FnOnce(crate::api::Client, Recipient, Option<String>) -> Fut,
        Fut: std::future::Future<Output = anyhow::Result<Option<String>>> + Send + 'static,
    {
        if self.sending.get() {
            return false;
        }
        let Some(main) = self.main.upgrade() else { return false };
        let recipient = match self.thread_id.borrow().clone() {
            Some(id) => Recipient::Thread(id),
            None => match self.thread.borrow().users.first() {
                Some(u) => Recipient::User(u.pk.clone()),
                None => return false,
            },
        };
        self.sending.set(true);
        self.send_button.set_sensitive(false);
        self.mark_active();
        let reply = self.reply_to.borrow().clone();
        self.set_reply(None);
        if let Some((_, quote)) = &reply {
            extra.insert("replied_to_message".into(), serde_json::json!({ "text": quote }));
        }
        self.pending.borrow_mut().push(DmItem {
            item_id: format!("pending:{}", uuid::Uuid::new_v4()),
            user_id: self.viewer_id.clone(),
            timestamp: chrono::Utc::now().timestamp_micros(),
            item_type: item_type.into(),
            extra,
        });
        self.stick_to_bottom.set(true);
        self.render();

        let sending = send(main.client.clone(), recipient.clone(), reply.map(|(id, _)| id));
        let client = main.client.clone();
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let res = rt::bg(async move {
                let thread_id = sending.await?;
                // Fetch the conversation so the sent message shows up for real.
                let thread = match (thread_id, &recipient) {
                    (Some(id), _) => client.thread(&id, None).await.ok(),
                    (None, Recipient::Thread(id)) => client.thread(id, None).await.ok(),
                    (None, Recipient::User(pk)) => client.thread_with_user(pk).await.ok().flatten(),
                };
                anyhow::Ok(thread)
            })
            .await;
            let Some(c) = weak.upgrade() else { return };
            c.sending.set(false);
            c.send_button.set_sensitive(true);
            match res {
                Ok(Some(thread)) => {
                    let first = c.thread_id.borrow().is_none();
                    c.adopt_thread(thread, first);
                    if first
                        && let (Some(page), Some(id)) = (c.page.upgrade(), c.thread_id.borrow().clone())
                    {
                        page.set_tag(Some(&format!("thread:{id}")));
                    }
                }
                Ok(None) => {
                    // Sent, but we couldn't refetch; keep the message shown.
                }
                Err(e) => {
                    c.pending.borrow_mut().clear();
                    c.render();
                    if !text.is_empty() && c.entry.text().is_empty() {
                        c.entry.set_text(&text);
                    }
                    if let Some(main) = c.main.upgrade() {
                        main.handle_error(&e);
                    }
                }
            }
        });
        true
    }

    /// React with `emoji`, or take your reaction back (None).
    fn react(self: &Rc<Self>, item_id: &str, emoji: Option<&'static str>) {
        let before = {
            let mut items = self.items.borrow_mut();
            let Some(item) = items.iter_mut().find(|i| i.item_id == item_id) else { return };
            let before = item.reaction_by(&self.viewer_id);
            item.set_reaction(&self.viewer_id, emoji);
            before
        };
        // Keep focus out of the list, or redrawing it scrolls to the focus.
        self.entry.grab_focus();
        self.render();
        let Some(main) = self.main.upgrade() else { return };
        let thread_id = self.thread.borrow().thread_id.clone();
        let client = main.client.clone();
        let item_id = item_id.to_string();
        let weak = Rc::downgrade(self);
        // Taking a reaction back names the emoji it was.
        let (sent, on) = match (emoji, before.clone()) {
            (Some(e), _) => (e.to_string(), true),
            (None, Some(b)) => (b, false),
            (None, None) => return,
        };
        glib::spawn_future_local(async move {
            let id = item_id.clone();
            let res = rt::bg(async move { client.react(&thread_id, &id, &sent, on).await }).await;
            let Some(chat) = weak.upgrade() else { return };
            if let Err(e) = res {
                eprintln!("finsta: reaction failed: {e}");
                if let Some(item) = chat.items.borrow_mut().iter_mut().find(|i| i.item_id == item_id) {
                    item.set_reaction(&chat.viewer_id, before.as_deref());
                }
                chat.render();
                if let Some(main) = chat.main.upgrade() {
                    if api_login_required(&e) {
                        main.handle_error(&e);
                    } else {
                        main.toast("Couldn't send your reaction");
                    }
                }
            }
        });
    }

    fn render(self: &Rc<Self>) {
        // Rebuilding the list must not move the view if you've scrolled up.
        if !self.stick_to_bottom.get() && self.keep_distance_from_bottom.get().is_none() {
            let adj = self.scroller.vadjustment();
            self.keep_distance_from_bottom
                .set(Some(adj.upper() - adj.value() - adj.page_size()));
        }
        util::clear_box(&self.list);
        let Some(main) = self.main.upgrade() else { return };
        let thread = self.thread.borrow().clone();
        let group = thread.is_group_chat();
        let items = self.items.borrow();
        let pending = self.pending.borrow();
        let all: Vec<(&DmItem, bool)> = items
            .iter()
            .map(|i| (i, false))
            .chain(pending.iter().map(|i| (i, true)))
            .collect();

        let mut prev_ts = 0i64;
        let mut prev_user = String::new();
        for (item, is_pending) in all {
            let ts = item.timestamp_secs();
            let new_block = ts - prev_ts > 3600;
            if new_block {
                self.list.append(
                    &gtk::Label::builder()
                        .label(util::chat_time(ts))
                        .css_classes(["day-separator"])
                        .build(),
                );
            }
            let is_me = item.user_id == self.viewer_id;
            let sender = if group && !is_me && (new_block || item.user_id != prev_user) {
                Some(
                    thread
                        .user(&item.user_id)
                        .cloned()
                        .unwrap_or_else(|| User {
                            pk: item.user_id.clone(),
                            username: "someone".into(),
                            ..Default::default()
                        }),
                )
            } else {
                None
            };
            self.list
                .append(&message_widget(self, &main, item, is_me, group, sender.as_ref(), is_pending));
            prev_ts = ts;
            prev_user = item.user_id.clone();
        }
        let nothing = items.is_empty() && pending.is_empty();
        self.empty.set_visible(nothing && !self.empty.label().is_empty() && !self.loading.get());
        self.older_button
            .set_visible(self.has_older.get() && self.oldest_cursor.borrow().is_some());
    }
}

const REACTIONS: [&str; 6] = ["❤️", "😂", "😮", "😢", "😡", "👍"];

/// Right-click or long-press a message: react to it, and the usual
/// actions for what it holds.
fn message_menu(chat: &Rc<Chat>, bubble: &gtk::Widget, item: &DmItem) {
    let item_id = item.item_id.clone();
    if item_id.is_empty() || item_id.starts_with("snippet:") {
        return;
    }
    bubble.set_tooltip_text(Some(&format!(
        "{} · right-click for reactions and more",
        bubble.tooltip_text().map(|t| t.to_string()).unwrap_or_else(|| util::full_time(item.timestamp_secs()))
    )));
    let weak = Rc::downgrade(chat);
    let item = item.clone();
    // Before the text inside gets the click (it has its own menu).
    menu::attach_with(bubble, true, move || {
        let chat = weak.upgrade()?;
        let main = chat.main.upgrade()?;
        let mut m = Menu::new();
        let mine = item.reaction_by(&chat.viewer_id);
        let row = gtk::Box::builder().spacing(2).margin_start(6).margin_end(6).build();
        for emoji in REACTIONS {
            let b = gtk::Button::builder().label(emoji).css_classes(["flat", "reaction-choice"]).build();
            let remove = mine.as_deref() == Some(emoji);
            if remove {
                b.add_css_class("mine");
                b.set_tooltip_text(Some("Remove your reaction"));
            }
            let weak = Rc::downgrade(&chat);
            let item_id = item.item_id.clone();
            b.connect_clicked(move |b| {
                menu::close_from(b);
                // Once the menu is gone (redrawing the list removes the
                // message it points at).
                let weak = weak.clone();
                let item_id = item_id.clone();
                glib::idle_add_local_once(move || {
                    if let Some(chat) = weak.upgrade() {
                        chat.react(&item_id, if remove { None } else { Some(emoji) });
                    }
                });
            });
            row.append(&b);
        }
        m.top(&row);
        match item.content() {
            DmContent::Text(t) | DmContent::Link { text: t } if !t.is_empty() => {
                let w = Rc::downgrade(&main);
                m.item("Copy Text", move || {
                    if let Some(main) = w.upgrade() {
                        main.copy(&t, "Copied");
                    }
                });
            }
            DmContent::Post(media) => {
                let (w, media2) = (Rc::downgrade(&main), media.clone());
                m.item(if media.product_type == "clips" { "Open Reel" } else { "Open Post" }, move || {
                    if let Some(main) = w.upgrade() {
                        main.open_post((*media2).clone());
                    }
                });
                if let Some(url) = media.web_url() {
                    menu::add_link(&mut m, &main, &url);
                }
            }
            DmContent::Shared { url: Some(url), .. } => {
                let (w, u) = (Rc::downgrade(&main), url.clone());
                m.item("Open", move || {
                    if let Some(main) = w.upgrade() {
                        main.open_link(&u);
                    }
                });
                let (w, u) = (Rc::downgrade(&main), url);
                m.item("Copy Link", move || {
                    if let Some(main) = w.upgrade() {
                        main.copy(&u, "Link copied");
                    }
                });
            }
            _ => {}
        }
        let thread = chat.thread.borrow().clone();
        if item.user_id != chat.viewer_id
            && let Some(sender) = thread.user(&item.user_id)
        {
            m.section();
            menu::add_person(&mut m, &main, sender, true, thread.is_group_chat());
        }
        Some(m)
    });
}

/// The GIF button: search GIPHY (with your key from Preferences) and send one.
fn gif_button(chat: &Rc<Chat>) -> gtk::MenuButton {
    let search = gtk::SearchEntry::builder().placeholder_text("Search GIPHY").build();
    let status = gtk::Label::builder()
        .wrap(true)
        .max_width_chars(36)
        .margin_top(24)
        .margin_bottom(24)
        .css_classes(["dim-label"])
        .build();
    let grid = gtk::FlowBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .min_children_per_line(3)
        .max_children_per_line(3)
        .column_spacing(4)
        .row_spacing(4)
        .valign(gtk::Align::Start)
        .build();
    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .min_content_height(340)
        .min_content_width(360)
        .child(&grid)
        .build();
    let column = gtk::Box::new(gtk::Orientation::Vertical, 8);
    column.append(&search);
    column.append(&status);
    column.append(&scroller);
    let popover = gtk::Popover::builder().child(&column).build();
    let button = gtk::MenuButton::builder()
        .label("GIF")
        .tooltip_text("Send a GIF")
        .popover(&popover)
        .valign(gtk::Align::Center)
        .css_classes(["flat"])
        .build();

    // Results of an older search that arrive late are dropped.
    let generation = Rc::new(Cell::new(0u32));
    let (weak, grid, status, popover_ref) = (Rc::downgrade(chat), grid.downgrade(), status.downgrade(), popover.downgrade());
    let load = Rc::new(move |query: String| {
        let (Some(c), Some(grid), Some(status)) = (weak.upgrade(), grid.upgrade(), status.upgrade()) else { return };
        let Some(main) = c.main.upgrade() else { return };
        while let Some(child) = grid.first_child() {
            grid.remove(&child);
        }
        status.set_visible(true);
        let key = crate::ui::prefs::get().giphy_key;
        if key.is_empty() {
            status.set_label("To send GIFs, add your GIPHY API key in Preferences.");
            return;
        }
        status.set_label("Loading…");
        let n = generation.get() + 1;
        generation.set(n);
        let (generation, weak, popover) = (generation.clone(), weak.clone(), popover_ref.clone());
        let client = main.client.clone();
        glib::spawn_future_local(async move {
            let res = rt::bg(async move { client.search_gifs(&key, &query).await }).await;
            if generation.get() != n {
                return;
            }
            match res {
                Ok(gifs) if gifs.is_empty() => status.set_label("No GIFs found"),
                Ok(gifs) => {
                    status.set_visible(false);
                    for gif in gifs {
                        let pick = gtk::Button::builder()
                            .child(&media_thumb(Some(gif.still.clone()), gif.aspect, 110, false))
                            .tooltip_text("Send")
                            .css_classes(["flat"])
                            .build();
                        let (weak, popover) = (weak.clone(), popover.clone());
                        pick.connect_clicked(move |_| {
                            if let Some(p) = popover.upgrade() {
                                p.popdown();
                            }
                            if let Some(c) = weak.upgrade() {
                                c.send_gif(gif.clone());
                            }
                        });
                        grid.append(&pick);
                    }
                }
                Err(e) => {
                    eprintln!("finsta: GIF search failed: {e}");
                    status.set_label(if e.to_string().contains("API key") {
                        "GIPHY didn't accept your API key. Check it in Preferences."
                    } else {
                        "Couldn't load GIFs. Try again in a moment."
                    });
                }
            }
        });
    });
    let l = load.clone();
    let s = search.clone();
    popover.connect_show(move |_| l(s.text().to_string()));
    search.connect_search_changed(move |s| load(s.text().to_string()));
    // The search box takes Escape; it closes the picker.
    let p = popover.downgrade();
    search.connect_stop_search(move |_| {
        if let Some(p) = p.upgrade() {
            p.popdown();
        }
    });
    button
}

/// Click to open; waits out a double-click, which replies to the message instead.
fn clickable(widget: &impl IsA<gtk::Widget>, f: impl Fn() + 'static) {
    let click = gtk::GestureClick::new();
    let f = Rc::new(f);
    let waiting: Rc<RefCell<Option<glib::SourceId>>> = Rc::default();
    let w = waiting.clone();
    click.connect_pressed(move |_, presses, _, _| {
        if presses > 1
            && let Some(open) = w.take()
        {
            open.remove();
        }
    });
    click.connect_released(move |gesture, presses, _, _| {
        gesture.set_state(gtk::EventSequenceState::Claimed);
        if presses != 1 {
            return;
        }
        let wait = gesture.widget().map_or(400, |w| w.settings().gtk_double_click_time());
        let (f, w) = (f.clone(), waiting.clone());
        let open = glib::timeout_add_local_once(std::time::Duration::from_millis(wait.max(0) as u64), move || {
            w.take();
            f();
        });
        if let Some(old) = waiting.replace(Some(open)) {
            old.remove();
        }
    });
    widget.add_controller(click);
    widget.set_cursor_from_name(Some("pointer"));
}

fn text_label(markup: &str) -> gtk::Label {
    gtk::Label::builder()
        .use_markup(true)
        .label(markup)
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::WordChar)
        .xalign(0.0)
        .selectable(true)
        .max_width_chars(52)
        .build()
}

fn media_thumb(url: Option<String>, aspect: f64, width: i32, video: bool) -> gtk::Widget {
    let picture = gtk::Picture::builder()
        .content_fit(gtk::ContentFit::Cover)
        .can_shrink(true)
        .build();
    images::load_picture(&picture, url);
    let overlay = gtk::Overlay::builder().child(&picture).build();
    if video {
        overlay.add_overlay(
            &gtk::Image::builder()
                .icon_name("media-playback-start-symbolic")
                .pixel_size(32)
                .halign(gtk::Align::Center)
                .valign(gtk::Align::Center)
                .css_classes(["osd", "circular"])
                .build(),
        );
    }
    RatioBin::fixed(aspect, width, &overlay).upcast()
}

fn shared_card(
    main: &Rc<Main>,
    title: &str,
    subtitle: &str,
    preview: Option<String>,
    url: Option<String>,
    aspect: f64,
) -> gtk::Widget {
    let in_app = url
        .as_deref()
        .is_some_and(|u| crate::api::web::link_code(u).is_some() || crate::api::web::story_link_pk(u).is_some());
    let is_reel = url.as_deref().is_some_and(|u| u.contains("/reel"));
    let width = if aspect > 1.1 { 200 } else { 240 };
    let card = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .css_classes(["bubble-card"])
        .overflow(gtk::Overflow::Hidden)
        .width_request(width)
        .build();
    if preview.is_some() {
        card.append(&media_thumb(preview, aspect, width, is_reel));
    }
    let text = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(2)
        .margin_top(8)
        .margin_bottom(8)
        .margin_start(10)
        .margin_end(10)
        .build();
    text.append(
        &gtk::Label::builder()
            .label(title)
            .xalign(0.0)
            .wrap(true)
            .css_classes(["heading"])
            .build(),
    );
    if !subtitle.trim().is_empty() {
        text.append(
            &gtk::Label::builder()
                .label(subtitle)
                .xalign(0.0)
                .wrap(true)
                .lines(3)
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .max_width_chars(36)
                .css_classes(["caption"])
                .build(),
        );
    }
    if let Some(url) = url {
        // Posts and reels open as a single post, never as a reel feed.
        if !in_app {
            text.append(
                &gtk::Label::builder()
                    .label("Opens in your browser")
                    .xalign(0.0)
                    .css_classes(["caption", "dim-label"])
                    .build(),
            );
        }
        let weak = Rc::downgrade(main);
        clickable(&card, move || {
            if let Some(main) = weak.upgrade() {
                main.open_link(&url);
            }
        });
    }
    card.append(&text);
    card.upcast()
}

fn post_card(main: &Rc<Main>, media: &Media) -> gtk::Widget {
    let card = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .css_classes(["bubble-card"])
        .overflow(gtk::Overflow::Hidden)
        .width_request(260)
        .build();
    if let Some(user) = &media.user {
        let head = gtk::Box::builder()
            .spacing(8)
            .margin_top(8)
            .margin_bottom(8)
            .margin_start(10)
            .margin_end(10)
            .build();
        head.append(&util::avatar(user, 24));
        head.append(
            &gtk::Label::builder()
                .label(&user.username)
                .css_classes(["username"])
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .build(),
        );
        card.append(&head);
    }
    card.append(&media_thumb(media.image_url(480), media.aspect(), 260, media.is_video() || media.is_carousel()));
    let caption = media.caption_text();
    if !caption.trim().is_empty() {
        card.append(
            &gtk::Label::builder()
                .label(&caption)
                .xalign(0.0)
                .wrap(true)
                .lines(2)
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .max_width_chars(36)
                .margin_top(6)
                .margin_bottom(8)
                .margin_start(10)
                .margin_end(10)
                .css_classes(["caption"])
                .build(),
        );
    }
    let weak = Rc::downgrade(main);
    let media = media.clone();
    clickable(&card, move || {
        if let Some(main) = weak.upgrade() {
            main.open_post(media.clone());
        }
    });
    card.upcast()
}

fn disappearing_widget(chat: &Rc<Chat>, item: &DmItem, d: &Disappearing, is_me: bool) -> gtk::Widget {
    let icon = if d.is_video { "camera-video-symbolic" } else { "camera-photo-symbolic" };
    let title = if d.is_video { "Video" } else { "Photo" };
    let mode = if d.view_mode == "replayable" { "Replay allowed" } else { "View once" };
    let (subtitle, openable) = if is_me {
        let status = match d.action.as_str() {
            "raven_opened" => "Opened",
            "raven_replayed" => "Replayed",
            "raven_screenshot" | "raven_screenshotted" => "Screenshot taken",
            "raven_delivered" => "Delivered",
            "raven_sending" => "Sending",
            "raven_cannot_deliver" | "raven_failed" => "Not delivered",
            _ if !d.seen_user_ids.is_empty() => "Opened",
            _ => "Sent",
        };
        (format!("{mode} · {status}"), false)
    } else {
        match chat.visual_state(item, d) {
            VisualState::View => (format!("{mode} · Tap to view"), true),
            VisualState::Replay => ("Tap to replay".to_string(), true),
            VisualState::Opened => ("Opened".to_string(), false),
            VisualState::Gone => ("No longer available".to_string(), false),
        }
    };

    let content = gtk::Box::builder().spacing(10).build();
    content.append(&gtk::Image::from_icon_name(icon));
    let text = gtk::Box::new(gtk::Orientation::Vertical, 0);
    text.append(
        &gtk::Label::builder()
            .label(title)
            .xalign(0.0)
            .css_classes(["heading"])
            .build(),
    );
    text.append(
        &gtk::Label::builder()
            .label(&subtitle)
            .xalign(0.0)
            .css_classes(["caption"])
            .build(),
    );
    content.append(&text);

    if !openable {
        let b = gtk::Box::builder()
            .css_classes(["disappearing", "used"])
            .halign(if is_me { gtk::Align::End } else { gtk::Align::Start })
            .build();
        b.append(&content);
        return b.upcast();
    }
    let button = gtk::Button::builder()
        .child(&content)
        .css_classes(["disappearing", "openable"])
        .halign(gtk::Align::Start)
        .tooltip_text(format!(
            "Disappearing {}. The sender will see when you open it.",
            d.kind()
        ))
        .build();
    let weak = Rc::downgrade(chat);
    let id = item.item_id.clone();
    button.connect_clicked(move |_| {
        if let Some(c) = weak.upgrade() {
            c.open_disappearing(id.clone());
        }
    });
    button.upcast()
}

fn message_widget(
    chat: &Rc<Chat>,
    main: &Rc<Main>,
    item: &DmItem,
    is_me: bool,
    group: bool,
    sender: Option<&User>,
    pending: bool,
) -> gtk::Widget {
    let content = item.content();
    if let DmContent::Info(text) = &content {
        return gtk::Label::builder()
            .label(text)
            .wrap(true)
            .justify(gtk::Justification::Center)
            .css_classes(["info-message"])
            .margin_top(4)
            .margin_bottom(4)
            .build()
            .upcast();
    }

    let align = if is_me { gtk::Align::End } else { gtk::Align::Start };
    // The row fills the width and an expanding spacer pushes the bubble to
    // its side, so GTK measures the bubble at its real (natural) width.
    let row = gtk::Box::builder().spacing(8).build();
    let spacer = gtk::Box::builder().hexpand(true).build();
    if is_me {
        row.append(&spacer);
    }
    if group && !is_me {
        let slot = gtk::Box::builder().width_request(28).valign(gtk::Align::End).build();
        if let Some(user) = sender {
            slot.append(&util::avatar(user, 28));
        }
        row.append(&slot);
    }
    let column = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(2)
        .halign(align)
        .build();
    if let Some(user) = sender {
        column.append(
            &gtk::Label::builder()
                .label(&user.username)
                .xalign(0.0)
                .margin_start(12)
                .margin_top(6)
                .css_classes(["caption", "dim-label"])
                .build(),
        );
    }
    if let Some(quote) = item.replied_to_text() {
        column.append(
            &gtk::Label::builder()
                .label(format!("Replying to: {quote}"))
                .xalign(0.0)
                .wrap(true)
                .max_width_chars(48)
                .halign(align)
                .css_classes(["reply-quote"])
                .build(),
        );
    }

    let bubble_class = if is_me { "outgoing" } else { "incoming" };
    let is_text = matches!(content, DmContent::Text(_) | DmContent::Link { .. });
    let bubble: gtk::Widget = match content {
        DmContent::Text(text) | DmContent::Link { text } => {
            let b = gtk::Box::builder()
                .css_classes(["bubble", bubble_class])
                .halign(align)
                .build();
            b.append(&text_label(&util::linkify(&text)));
            b.upcast()
        }
        DmContent::Like(emoji) => gtk::Label::builder()
            .label(emoji)
            .css_classes(["emoji-big"])
            .halign(align)
            .build()
            .upcast(),
        DmContent::Media { image, video, aspect } => {
            let thumb = media_thumb(image.clone(), aspect, 260, video.is_some());
            thumb.add_css_class("rounded");
            thumb.set_overflow(gtk::Overflow::Hidden);
            thumb.set_halign(align);
            let weak = Rc::downgrade(main);
            clickable(&thumb, move || {
                if let Some(main) = weak.upgrade() {
                    media_viewer::open(&main, image.clone(), video.clone());
                }
            });
            thumb
        }
        DmContent::Post(media) => post_card(main, &media),
        DmContent::Shared { title, subtitle, preview, url, aspect } => {
            shared_card(main, &title, &subtitle, preview, url, aspect)
        }
        DmContent::Voice { url, duration_ms } => {
            let b = gtk::Box::builder()
                .spacing(8)
                .css_classes(["bubble", bubble_class])
                .halign(align)
                .build();
            let secs = duration_ms / 1000;
            b.append(&gtk::Label::new(Some(&format!(
                "Voice message · {}:{:02}",
                secs / 60,
                secs % 60
            ))));
            if let Some(url) = url {
                b.prepend(&video::voice_button(url));
            }
            b.upcast()
        }
        DmContent::Gif(url) => {
            let thumb = media_thumb(url, 1.0, 180, false);
            thumb.add_css_class("rounded");
            thumb.set_overflow(gtk::Overflow::Hidden);
            thumb.set_halign(align);
            thumb
        }
        DmContent::Disappearing(d) => disappearing_widget(chat, item, &d, is_me),
        DmContent::Info(_) => unreachable!(),
    };
    if bubble.tooltip_text().is_none() {
        bubble.set_tooltip_text(Some(&util::full_time(item.timestamp_secs())));
    }
    if pending {
        bubble.add_css_class("pending");
        bubble.set_tooltip_text(Some("Sending…"));
    }
    column.append(&bubble);
    if !pending {
        message_menu(chat, &bubble, item);
        if !item.item_id.is_empty() && !item.item_id.starts_with("snippet:") {
            // Double-click: reply to it. Seen before what's inside: text would select a word, and
            // a post (or photo) must see it too, to not open.
            let double = gtk::GestureClick::builder().propagation_phase(gtk::PropagationPhase::Capture).build();
            let (weak, item) = (Rc::downgrade(chat), item.clone());
            double.connect_pressed(move |gesture, presses, _, _| {
                if presses == 2
                    && let Some(c) = weak.upgrade()
                {
                    if is_text {
                        gesture.set_state(gtk::EventSequenceState::Claimed);
                    }
                    c.set_reply(Some(&item));
                }
            });
            bubble.add_controller(double);
        }
    }

    let reactions = item.reactions();
    if !reactions.is_empty() {
        let mut counts: Vec<(String, usize)> = Vec::new();
        for r in reactions {
            match counts.iter_mut().find(|(e, _)| *e == r) {
                Some((_, n)) => *n += 1,
                None => counts.push((r, 1)),
            }
        }
        let text = counts
            .iter()
            .map(|(e, n)| if *n > 1 { format!("{e} {n}") } else { e.clone() })
            .collect::<Vec<_>>()
            .join(" ");
        column.append(
            &gtk::Label::builder()
                .label(text)
                .halign(align)
                .margin_start(8)
                .margin_end(8)
                .css_classes(["reaction"])
                .build(),
        );
    }
    row.append(&column);
    if !is_me {
        row.append(&spacer);
    }
    row.upcast()
}
