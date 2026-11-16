//! Search results for the top bar: matching conversations and people.
//! A short, fixed list of results — no explore page, no endless results.

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

use adw::prelude::*;
use gtk::glib;

use crate::api::{Thread, User};
use crate::rt;
use crate::ui::util;
use crate::ui::window::Main;

pub struct SearchPage {
    main: Weak<Main>,
    pub page: adw::NavigationPage,
    chats_title: gtk::Label,
    chats_list: gtk::ListBox,
    people_list: gtk::ListBox,
    people_status: gtk::Stack,
    chats: RefCell<Vec<Thread>>,
    people: RefCell<Vec<User>>,
    generation: Cell<u64>,
}

fn section_title(text: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(text)
        .xalign(0.0)
        .css_classes(["heading"])
        .margin_top(6)
        .build()
}

impl SearchPage {
    pub fn new(main: &Rc<Main>) -> Rc<Self> {
        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(10)
            .margin_top(18)
            .margin_bottom(18)
            .margin_start(12)
            .margin_end(12)
            .build();
        let chats_title = section_title("Conversations");
        let chats_list = gtk::ListBox::builder()
            .css_classes(["boxed-list"])
            .selection_mode(gtk::SelectionMode::None)
            .build();
        content.append(&chats_title);
        content.append(&chats_list);

        content.append(&section_title("People"));
        let people_list = gtk::ListBox::builder()
            .css_classes(["boxed-list"])
            .selection_mode(gtk::SelectionMode::None)
            .build();
        let people_status = gtk::Stack::new();
        people_status.add_named(&people_list, Some("results"));
        people_status.add_named(&util::spinner_box(), Some("loading"));
        let empty = gtk::Label::builder()
            .label("No people found")
            .css_classes(["dim-label"])
            .margin_top(12)
            .build();
        people_status.add_named(&empty, Some("empty"));
        content.append(&people_status);

        let clamp = adw::Clamp::builder().maximum_size(620).child(&content).build();
        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .child(&clamp)
            .build();
        let page = adw::NavigationPage::builder()
            .title("Search")
            .tag("search")
            .child(&scroller)
            .build();

        let search = Rc::new(SearchPage {
            main: Rc::downgrade(main),
            page,
            chats_title,
            chats_list,
            people_list,
            people_status,
            chats: RefCell::new(Vec::new()),
            people: RefCell::new(Vec::new()),
            generation: Cell::new(0),
        });

        let weak = Rc::downgrade(&search);
        search.chats_list.connect_row_activated(glib::clone!(
            #[strong]
            weak,
            move |_, row| {
                let Some(s) = weak.upgrade() else { return };
                let thread = s.chats.borrow().get(row.index() as usize).cloned();
                if let (Some(thread), Some(main)) = (thread, s.main.upgrade()) {
                    main.open_thread(thread, false);
                }
            }
        ));
        search.people_list.connect_row_activated(glib::clone!(
            #[strong]
            weak,
            move |_, row| {
                let Some(s) = weak.upgrade() else { return };
                let user = s.people.borrow().get(row.index() as usize).cloned();
                if let (Some(user), Some(main)) = (user, s.main.upgrade()) {
                    main.open_profile(user);
                }
            }
        ));
        search
    }

    /// Enter in the search field opens the first result.
    pub fn activate_first(&self) {
        let first = self
            .chats_list
            .row_at_index(0)
            .or_else(|| self.people_list.row_at_index(0));
        if let Some(row) = first {
            row.emit_activate();
        }
    }

    pub fn set_query(self: &Rc<Self>, query: &str) {
        self.generation.set(self.generation.get() + 1);
        let Some(main) = self.main.upgrade() else { return };
        let q = query.trim().to_lowercase();

        // Conversations: filter what's already loaded in the sidebar.
        self.chats_list.remove_all();
        let matches: Vec<Thread> = if q.is_empty() {
            Vec::new()
        } else {
            main.sidebar
                .get()
                .map(|s| s.threads())
                .unwrap_or_default()
                .into_iter()
                .filter(|t| {
                    t.title().to_lowercase().contains(&q)
                        || t.users.iter().any(|u| {
                            u.username.to_lowercase().contains(&q) || u.full_name.to_lowercase().contains(&q)
                        })
                })
                .take(6)
                .collect()
        };
        for t in &matches {
            let row = adw::ActionRow::builder()
                .title(t.title())
                .title_lines(1)
                .use_markup(false)
                .activatable(true)
                .build();
            if t.is_group_chat() {
                row.set_subtitle(&format!("Group · {} members", t.users.len() + 1));
                let a = adw::Avatar::new(36, Some(&t.title()), false);
                a.set_icon_name(Some("system-users-symbolic"));
                row.add_prefix(&a);
            } else if let Some(u) = t.users.first() {
                row.set_subtitle(&format!("@{}", u.username));
                row.add_prefix(&util::avatar(u, 36));
            }
            row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
            self.chats_list.append(&row);
        }
        self.chats_title.set_visible(!matches.is_empty());
        self.chats_list.set_visible(!matches.is_empty());
        self.chats.replace(matches);

        // People: ask Instagram.
        self.people_list.remove_all();
        self.people.borrow_mut().clear();
        if q.is_empty() {
            return;
        }
        self.people_status.set_visible_child_name("loading");
        let generation = self.generation.get();
        let client = main.client.clone();
        let query = query.trim().to_string();
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let res = rt::bg(async move { client.search_users(&query).await }).await;
            let Some(s) = weak.upgrade() else { return };
            if s.generation.get() != generation {
                return;
            }
            let Some(main) = s.main.upgrade() else { return };
            match res {
                Ok(users) => {
                    let users: Vec<User> = users.into_iter().take(20).collect();
                    for u in &users {
                        let mut subtitle = format!("@{}", u.username);
                        if main.is_followed(u) && u.pk != main.client.user_id() {
                            subtitle.push_str(" · Following");
                        }
                        let row = adw::ActionRow::builder()
                            .title(u.display_name())
                            .subtitle(subtitle)
                            .title_lines(1)
                            .use_markup(false)
                            .activatable(true)
                            .build();
                        row.add_prefix(&util::avatar(u, 36));
                        if u.is_verified {
                            row.add_suffix(&gtk::Image::from_icon_name("object-select-symbolic"));
                        }
                        row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
                        s.people_list.append(&row);
                    }
                    s.people_status
                        .set_visible_child_name(if users.is_empty() { "empty" } else { "results" });
                    s.people.replace(users);
                }
                Err(e) => {
                    s.people_status.set_visible_child_name("empty");
                    main.handle_error(&e);
                }
            }
        });
    }
}
