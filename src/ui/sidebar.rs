//! Sidebar: Home, the list of conversations (people and groups), Profile.

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

use adw::prelude::*;
use gtk::glib;
use serde_json::json;

use crate::api::{Thread, User};
use crate::rt;
use crate::ui::util;
use crate::ui::window::Main;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Filter {
    All,
    People,
    Groups,
}

pub struct Sidebar {
    main: Weak<Main>,
    pub widget: gtk::Box,
    home_list: gtk::ListBox,
    threads_list: gtk::ListBox,
    profile_list: gtk::ListBox,
    profile_avatar_slot: gtk::Box,
    profile_label: gtk::Label,
    status: gtk::Stack,
    older_button: gtk::Button,
    threads: RefCell<Vec<Thread>>,
    row_ids: RefCell<Vec<String>>,
    oldest_cursor: RefCell<Option<String>>,
    has_older: Cell<bool>,
    filter: Cell<Filter>,
    loading: Cell<bool>,
    loaded: Cell<bool>,
    selected_tag: RefCell<String>,
    viewer_id: String,
}

fn nav_row(icon: &gtk::Widget, label: &gtk::Label) -> gtk::ListBoxRow {
    let b = gtk::Box::builder()
        .spacing(12)
        .margin_top(6)
        .margin_bottom(6)
        .margin_start(4)
        .build();
    b.append(icon);
    b.append(label);
    gtk::ListBoxRow::builder().child(&b).build()
}

impl Sidebar {
    pub fn new(main: &Rc<Main>) -> Rc<Self> {
        let widget = gtk::Box::new(gtk::Orientation::Vertical, 0);

        // Home
        let home_list = gtk::ListBox::new();
        home_list.add_css_class("navigation-sidebar");
        let home_icon = gtk::Image::from_icon_name("go-home-symbolic");
        home_list.append(&nav_row(home_icon.upcast_ref(), &gtk::Label::new(Some("Home"))));
        widget.append(&home_list);

        // Messages heading with refresh + filter
        let heading = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        let title = gtk::Label::builder()
            .label("Messages")
            .xalign(0.0)
            .hexpand(true)
            .css_classes(["sidebar-section"])
            .build();
        let refresh = gtk::Button::builder()
            .icon_name("view-refresh-symbolic")
            .tooltip_text("Refresh Conversations")
            .css_classes(["flat", "circular"])
            .margin_end(8)
            .valign(gtk::Align::Center)
            .build();
        heading.append(&title);
        heading.append(&refresh);
        widget.append(&heading);

        let filters = gtk::Box::builder()
            .css_classes(["linked"])
            .homogeneous(true)
            .margin_start(12)
            .margin_end(12)
            .margin_top(4)
            .margin_bottom(4)
            .build();
        let all = gtk::ToggleButton::builder().label("All").active(true).build();
        let people = gtk::ToggleButton::builder().label("People").group(&all).build();
        let groups = gtk::ToggleButton::builder().label("Groups").group(&all).build();
        filters.append(&all);
        filters.append(&people);
        filters.append(&groups);
        widget.append(&filters);

        // Conversations
        let threads_list = gtk::ListBox::new();
        threads_list.add_css_class("navigation-sidebar");
        let older_button = gtk::Button::builder()
            .label("Load Older Conversations")
            .css_classes(["flat"])
            .margin_start(12)
            .margin_end(12)
            .margin_bottom(8)
            .visible(false)
            .build();
        let status = gtk::Stack::new();
        status.add_named(&gtk::Box::new(gtk::Orientation::Vertical, 0), Some("none"));
        status.add_named(&util::spinner_box(), Some("loading"));
        let empty = gtk::Label::builder()
            .label("No conversations")
            .css_classes(["dim-label"])
            .margin_top(24)
            .margin_bottom(24)
            .build();
        status.add_named(&empty, Some("empty"));
        status.set_visible_child_name("loading");
        let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
        column.append(&threads_list);
        column.append(&status);
        column.append(&older_button);
        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&column)
            .build();
        widget.append(&scroller);

        // Profile
        widget.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        let profile_list = gtk::ListBox::new();
        profile_list.add_css_class("navigation-sidebar");
        let profile_avatar_slot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        profile_avatar_slot.append(&adw::Avatar::new(28, None, false));
        let profile_label = gtk::Label::builder()
            .label("Profile")
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .build();
        profile_list.append(&nav_row(profile_avatar_slot.upcast_ref(), &profile_label));
        profile_list.set_hexpand(true);
        let saved_button = gtk::Button::builder()
            .icon_name("user-bookmarks-symbolic")
            .tooltip_text("Saved Posts")
            .css_classes(["flat", "circular"])
            .valign(gtk::Align::Center)
            .margin_end(8)
            .build();
        let footer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        footer.append(&profile_list);
        footer.append(&saved_button);
        widget.append(&footer);

        let sidebar = Rc::new(Sidebar {
            main: Rc::downgrade(main),
            widget,
            home_list,
            threads_list,
            profile_list,
            profile_avatar_slot,
            profile_label,
            status,
            older_button,
            threads: RefCell::new(Vec::new()),
            row_ids: RefCell::new(Vec::new()),
            oldest_cursor: RefCell::new(None),
            has_older: Cell::new(false),
            filter: Cell::new(Filter::All),
            loading: Cell::new(false),
            loaded: Cell::new(false),
            selected_tag: RefCell::new("home".into()),
            viewer_id: main.client.user_id(),
        });

        let weak = Rc::downgrade(&sidebar);
        sidebar.home_list.connect_row_activated(glib::clone!(
            #[strong]
            weak,
            move |_, _| {
                if let Some(s) = weak.upgrade()
                    && let Some(main) = s.main.upgrade()
                {
                    main.show_home();
                }
            }
        ));
        let w = weak.clone();
        saved_button.connect_clicked(move |_| {
            if let Some(s) = w.upgrade()
                && let Some(main) = s.main.upgrade()
            {
                main.open_saved();
            }
        });
        sidebar.profile_list.connect_row_activated(glib::clone!(
            #[strong]
            weak,
            move |_, _| {
                if let Some(s) = weak.upgrade()
                    && let Some(main) = s.main.upgrade()
                {
                    main.open_my_profile();
                }
            }
        ));
        sidebar.threads_list.connect_row_activated(glib::clone!(
            #[strong]
            weak,
            move |_, row| {
                let Some(s) = weak.upgrade() else { return };
                let id = s.row_ids.borrow().get(row.index() as usize).cloned();
                let thread = id.and_then(|id| s.thread(&id));
                if let (Some(thread), Some(main)) = (thread, s.main.upgrade()) {
                    s.mark_read_local(&thread.thread_id);
                    main.open_thread(thread, true);
                }
            }
        ));
        refresh.connect_clicked(glib::clone!(
            #[strong]
            weak,
            move |_| {
                if let Some(s) = weak.upgrade() {
                    s.refresh(false);
                }
            }
        ));
        sidebar.older_button.connect_clicked(glib::clone!(
            #[strong]
            weak,
            move |_| {
                if let Some(s) = weak.upgrade() {
                    s.load_older();
                }
            }
        ));
        for (button, filter) in [(all, Filter::All), (people, Filter::People), (groups, Filter::Groups)] {
            button.connect_toggled(glib::clone!(
                #[strong]
                weak,
                move |b| {
                    if b.is_active()
                        && let Some(s) = weak.upgrade()
                    {
                        s.filter.set(filter);
                        s.render();
                    }
                }
            ));
        }
        sidebar.sync_selection("home");
        sidebar
    }

    pub fn start(self: &Rc<Self>) {
        self.refresh(false);
        // Keep unread state fresh without a push connection.
        let weak = Rc::downgrade(self);
        glib::timeout_add_seconds_local(60, move || match weak.upgrade() {
            Some(s) => {
                s.refresh(true);
                glib::ControlFlow::Continue
            }
            None => glib::ControlFlow::Break,
        });
    }

    pub fn set_me(&self, user: &User) {
        util::clear_box(&self.profile_avatar_slot);
        self.profile_avatar_slot.append(&util::avatar(user, 28));
        self.profile_label.set_label(&format!("Profile · @{}", user.username));
    }

    pub fn threads(&self) -> Vec<Thread> {
        self.threads.borrow().clone()
    }

    pub fn thread(&self, id: &str) -> Option<Thread> {
        self.threads.borrow().iter().find(|t| t.thread_id == id).cloned()
    }

    pub fn direct_thread_with(&self, user_pk: &str) -> Option<Thread> {
        self.threads
            .borrow()
            .iter()
            .find(|t| !t.is_group_chat() && t.users.len() == 1 && t.users[0].pk == user_pk)
            .cloned()
    }

    pub fn refresh(self: &Rc<Self>, quiet: bool) {
        if self.loading.replace(true) {
            return;
        }
        if !self.loaded.get() {
            self.status.set_visible_child_name("loading");
        }
        let Some(main) = self.main.upgrade() else { return };
        let client = main.client.clone();
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let res = rt::bg(async move { client.inbox(None).await }).await;
            let Some(s) = weak.upgrade() else { return };
            s.loading.set(false);
            match res {
                Ok(page) => {
                    if !s.loaded.replace(true) {
                        s.oldest_cursor.replace(page.oldest_cursor);
                        s.has_older.set(page.has_older);
                    }
                    for thread in page.threads {
                        s.merge(thread);
                    }
                    s.render();
                }
                Err(e) => {
                    if !s.loaded.get() {
                        s.status.set_visible_child_name("empty");
                    }
                    if let Some(main) = s.main.upgrade()
                        && (!quiet || crate::api::is_login_required(&e))
                    {
                        main.handle_error(&e);
                    }
                }
            }
        });
    }

    fn load_older(self: &Rc<Self>) {
        let Some(cursor) = self.oldest_cursor.borrow().clone() else { return };
        if self.loading.replace(true) {
            return;
        }
        let Some(main) = self.main.upgrade() else { return };
        self.older_button.set_sensitive(false);
        let client = main.client.clone();
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let res = rt::bg(async move { client.inbox(Some(cursor)).await }).await;
            let Some(s) = weak.upgrade() else { return };
            s.loading.set(false);
            s.older_button.set_sensitive(true);
            match res {
                Ok(page) => {
                    s.oldest_cursor.replace(page.oldest_cursor);
                    s.has_older.set(page.has_older);
                    for thread in page.threads {
                        s.merge(thread);
                    }
                    s.render();
                }
                Err(e) => {
                    if let Some(main) = s.main.upgrade() {
                        main.handle_error(&e);
                    }
                }
            }
        });
    }

    fn merge(&self, mut thread: Thread) {
        let mut threads = self.threads.borrow_mut();
        if let Some(existing) = threads.iter_mut().find(|t| t.thread_id == thread.thread_id) {
            if thread.users.is_empty() {
                thread.users = std::mem::take(&mut existing.users);
            }
            if thread.items.is_empty() {
                thread.items = std::mem::take(&mut existing.items);
            }
            *existing = thread;
        } else {
            threads.push(thread);
        }
        threads.sort_by_key(|t| {
            std::cmp::Reverse(t.last_activity_at.max(t.last_item().map(|i| i.timestamp).unwrap_or(0)))
        });
    }

    /// Called by an open conversation when it learns about new messages.
    pub fn update_thread(&self, thread: Thread) {
        if thread.thread_id.is_empty() {
            return;
        }
        self.merge(thread);
        self.render();
    }

    pub fn mark_read_local(&self, thread_id: &str) {
        let mut changed = false;
        if let Some(t) = self.threads.borrow_mut().iter_mut().find(|t| t.thread_id == thread_id)
            && let Some(last) = t.last_item().cloned()
        {
            if !t.last_seen_at.is_object() {
                t.last_seen_at = json!({});
            }
            t.last_seen_at[self.viewer_id.as_str()] = json!({
                "item_id": last.item_id,
                "timestamp": last.timestamp.to_string(),
            });
            t.read_state = 0;
            changed = true;
        }
        if changed {
            self.render();
        }
    }

    fn render(&self) {
        self.threads_list.remove_all();
        let mut ids = Vec::new();
        let filter = self.filter.get();
        for thread in self.threads.borrow().iter() {
            let keep = match filter {
                Filter::All => true,
                Filter::People => !thread.is_group_chat(),
                Filter::Groups => thread.is_group_chat(),
            };
            if keep {
                self.threads_list.append(&self.thread_row(thread));
                ids.push(thread.thread_id.clone());
            }
        }
        let empty = ids.is_empty();
        self.row_ids.replace(ids);
        self.status.set_visible_child_name(if empty { "empty" } else { "none" });
        self.older_button
            .set_visible(self.has_older.get() && self.oldest_cursor.borrow().is_some());
        let tag = self.selected_tag.borrow().clone();
        self.sync_selection(&tag);
    }

    fn thread_row(&self, thread: &Thread) -> gtk::ListBoxRow {
        let unread = thread.is_unread(&self.viewer_id);
        let row_box = gtk::Box::builder()
            .spacing(10)
            .margin_top(4)
            .margin_bottom(4)
            .build();

        let avatar = if thread.is_group_chat() {
            let a = adw::Avatar::new(40, Some(&thread.title()), false);
            a.set_icon_name(Some("system-users-symbolic"));
            a
        } else if let Some(user) = thread.users.first() {
            util::avatar(user, 40)
        } else {
            adw::Avatar::new(40, Some(&thread.title()), true)
        };
        row_box.append(&avatar);

        let text = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .valign(gtk::Align::Center)
            .hexpand(true)
            .spacing(2)
            .build();
        let title = gtk::Label::builder()
            .label(thread.title())
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .css_classes(["thread-title"])
            .build();
        let preview_text = match thread.last_item() {
            Some(item) => {
                let summary = item.content().summary();
                if item.user_id == self.viewer_id {
                    format!("You: {summary}")
                } else if thread.is_group_chat() {
                    match thread.user(&item.user_id) {
                        Some(u) => format!("{}: {summary}", u.username),
                        None => summary,
                    }
                } else {
                    summary
                }
            }
            None if thread.is_group_chat() => format!("{} members", thread.users.len() + 1),
            None => String::new(),
        };
        let preview = gtk::Label::builder()
            .label(preview_text)
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .single_line_mode(true)
            .css_classes(["caption", "dim-label", "thread-preview"])
            .build();
        if unread {
            title.add_css_class("unread");
            preview.add_css_class("unread");
            preview.remove_css_class("dim-label");
        }
        text.append(&title);
        text.append(&preview);
        row_box.append(&text);

        let meta = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .valign(gtk::Align::Center)
            .spacing(6)
            .build();
        let ts = thread
            .last_item()
            .map(|i| i.timestamp_secs())
            .unwrap_or(thread.last_activity_at / 1_000_000);
        let time = gtk::Label::builder()
            .label(util::relative_time(ts))
            .css_classes(["caption", "dim-label"])
            .halign(gtk::Align::End)
            .build();
        meta.append(&time);
        let dot = gtk::Box::builder()
            .css_classes(["unread-dot"])
            .halign(gtk::Align::End)
            .visible(unread)
            .build();
        meta.append(&dot);
        row_box.append(&meta);

        let row = gtk::ListBoxRow::builder().child(&row_box).build();
        row.set_tooltip_text(Some(&thread.title()));
        row
    }

    /// Highlight the sidebar entry matching the visible page.
    pub fn sync_selection(&self, tag: &str) {
        self.selected_tag.replace(tag.to_string());
        self.home_list.unselect_all();
        self.profile_list.unselect_all();
        self.threads_list.unselect_all();
        if tag == "home" {
            self.home_list.select_row(self.home_list.row_at_index(0).as_ref());
        } else if tag == "me" {
            self.profile_list.select_row(self.profile_list.row_at_index(0).as_ref());
        } else if let Some(id) = tag.strip_prefix("thread:")
            && let Some(index) = self.row_ids.borrow().iter().position(|r| r == id)
        {
            self.threads_list
                .select_row(self.threads_list.row_at_index(index as i32).as_ref());
        }
    }
}
