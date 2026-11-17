//! Search results for the top bar: conversations, people and posts.
//! A short, fixed list of results — no explore page, no endless results.

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

use adw::prelude::*;
use gtk::glib;

use crate::api::{self, Media, Thread, User};
use crate::rt;
use crate::ui::profile::thumbnail;
use crate::ui::util;
use crate::ui::window::Main;

/// A conversation result: one already loaded, or someone to message.
#[derive(Clone)]
enum Chat {
    Thread(Thread),
    Contact(User),
}

pub struct SearchPage {
    main: Weak<Main>,
    pub page: adw::NavigationPage,
    chats_title: gtk::Label,
    chats_list: gtk::ListBox,
    people_list: gtk::ListBox,
    people_status: gtk::Stack,
    posts_title: gtk::Label,
    posts_grid: gtk::FlowBox,
    posts_status: gtk::Stack,
    chats: RefCell<Vec<Chat>>,
    people: RefCell<Vec<User>>,
    posts: RefCell<Vec<Media>>,
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

fn status_stack(results: &impl IsA<gtk::Widget>, empty: &str) -> gtk::Stack {
    let stack = gtk::Stack::builder().vhomogeneous(false).build();
    stack.add_named(results, Some("results"));
    stack.add_named(&util::spinner_box(), Some("loading"));
    stack.add_named(
        &gtk::Label::builder()
            .label(empty)
            .css_classes(["dim-label"])
            .margin_top(12)
            .build(),
        Some("empty"),
    );
    stack
}

fn person_row(main: &Main, u: &User) -> adw::ActionRow {
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
    row
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
        let people_status = status_stack(&people_list, "No people found");
        content.append(&people_status);

        let posts_title = section_title("Posts");
        let posts_grid = gtk::FlowBox::builder()
            .min_children_per_line(3)
            .max_children_per_line(4)
            .homogeneous(true)
            .column_spacing(4)
            .row_spacing(4)
            .selection_mode(gtk::SelectionMode::None)
            .build();
        let posts_status = status_stack(&posts_grid, "No posts found");
        content.append(&posts_title);
        content.append(&posts_status);

        let clamp = adw::Clamp::builder().maximum_size(720).child(&content).build();
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
            posts_title,
            posts_grid,
            posts_status,
            chats: RefCell::new(Vec::new()),
            people: RefCell::new(Vec::new()),
            posts: RefCell::new(Vec::new()),
            generation: Cell::new(0),
        });

        let weak = Rc::downgrade(&search);
        search.chats_list.connect_row_activated(glib::clone!(
            #[strong]
            weak,
            move |_, row| {
                let Some(s) = weak.upgrade() else { return };
                let chat = s.chats.borrow().get(row.index() as usize).cloned();
                let Some(main) = s.main.upgrade() else { return };
                match chat {
                    Some(Chat::Thread(thread)) => main.open_thread(thread, false),
                    Some(Chat::Contact(user)) => main.open_chat_with_user(user),
                    None => {}
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
        search.posts_grid.connect_child_activated(move |_, child| {
            let Some(s) = weak.upgrade() else { return };
            let media = s.posts.borrow().get(child.index() as usize).cloned();
            if let (Some(media), Some(main)) = (media, s.main.upgrade()) {
                main.open_post(media);
            }
        });
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
        } else if let Some(child) = self.posts_grid.child_at_index(0) {
            child.emit_activate();
        }
    }

    fn render_chats(&self) {
        self.chats_list.remove_all();
        let chats = self.chats.borrow();
        for chat in chats.iter() {
            let row = adw::ActionRow::builder().title_lines(1).use_markup(false).activatable(true).build();
            match chat {
                Chat::Thread(t) => {
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
                Chat::Contact(u) => {
                    row.set_title(&u.display_name());
                    row.set_subtitle(&format!("@{} · Message", u.username));
                    row.add_prefix(&util::avatar(u, 36));
                }
            }
            row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
            self.chats_list.append(&row);
        }
        self.chats_title.set_visible(!chats.is_empty());
        self.chats_list.set_visible(!chats.is_empty());
    }

    pub fn set_query(self: &Rc<Self>, query: &str) {
        self.generation.set(self.generation.get() + 1);
        let Some(main) = self.main.upgrade() else { return };
        let q = query.trim().to_lowercase();

        // Conversations: what's already loaded in the sidebar first.
        let matches: Vec<Chat> = if q.is_empty() {
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
                .map(Chat::Thread)
                .collect()
        };
        self.chats.replace(matches);
        self.render_chats();

        self.people_list.remove_all();
        self.people.borrow_mut().clear();
        self.posts_grid.remove_all();
        self.posts.borrow_mut().clear();
        // Posts: the one a pasted link points to, or a hashtag's top posts.
        let link = api::web::link_code(query.trim());
        let posts_wanted = link.is_some() || api::web::hashtag(&q).is_some();
        self.posts_title.set_visible(posts_wanted);
        self.posts_status.set_visible(posts_wanted);
        self.posts_title.set_label(if link.is_some() { "Post" } else { "Top Posts for the Hashtag" });
        if q.is_empty() {
            return;
        }
        self.people_status.set_visible_child_name("loading");
        self.posts_status.set_visible_child_name("loading");
        let generation = self.generation.get();
        let query = query.trim().to_string();
        let username_like = !query.contains(' ') && !query.starts_with('#');

        // People.
        {
            let client = main.client.clone();
            let query = query.clone();
            let weak = Rc::downgrade(self);
            glib::spawn_future_local(async move {
                let res = rt::bg(async move { client.search_people(&query).await }).await;
                let Some(s) = weak.upgrade() else { return };
                if s.generation.get() != generation {
                    return;
                }
                let Some(main) = s.main.upgrade() else { return };
                match res {
                    Ok(users) => {
                        let users: Vec<User> = users.into_iter().take(20).collect();
                        for u in &users {
                            s.people_list.append(&person_row(&main, u));
                        }
                        s.people_status
                            .set_visible_child_name(if users.is_empty() { "empty" } else { "results" });
                        s.people.replace(users);
                    }
                    Err(e) => {
                        s.people_status.set_visible_child_name("empty");
                        if api::is_login_required(&e) {
                            main.handle_error(&e);
                        } else {
                            eprintln!("finsta: people search failed: {e}");
                        }
                    }
                }
            });
        }

        // Conversations not loaded yet: people you can message.
        if username_like {
            let client = main.client.clone();
            let query = query.clone();
            let weak = Rc::downgrade(self);
            glib::spawn_future_local(async move {
                let res = rt::bg(async move { client.search_contacts(&query).await }).await;
                let Some(s) = weak.upgrade() else { return };
                if s.generation.get() != generation {
                    return;
                }
                match res {
                    Ok(users) => {
                        let mut chats = s.chats.borrow_mut();
                        for u in users {
                            let known = chats.iter().any(|c| match c {
                                Chat::Thread(t) => !t.is_group_chat() && t.users.iter().any(|x| x.pk == u.pk),
                                Chat::Contact(x) => x.pk == u.pk,
                            });
                            if !known && chats.len() < 10 {
                                chats.push(Chat::Contact(u));
                            }
                        }
                        drop(chats);
                        s.render_chats();
                    }
                    Err(e) => eprintln!("finsta: conversation search failed: {e}"),
                }
            });
        }

        if posts_wanted {
            let client = main.client.clone();
            let weak = Rc::downgrade(self);
            glib::spawn_future_local(async move {
                let res = rt::bg(async move {
                    match link {
                        Some(code) => client.media_by_code(&code).await.map(|m| vec![m]),
                        None => client.search_posts(&query).await,
                    }
                })
                .await;
                let Some(s) = weak.upgrade() else { return };
                if s.generation.get() != generation {
                    return;
                }
                match res {
                    Ok(posts) => {
                        for m in &posts {
                            s.posts_grid.append(&thumbnail(m));
                        }
                        s.posts_status
                            .set_visible_child_name(if posts.is_empty() { "empty" } else { "results" });
                        s.posts.replace(posts);
                    }
                    Err(e) => {
                        // Say nothing rather than "No posts found".
                        s.posts_title.set_visible(false);
                        s.posts_status.set_visible(false);
                        if api::is_login_required(&e)
                            && let Some(main) = s.main.upgrade()
                        {
                            main.handle_error(&e);
                        } else {
                            eprintln!("finsta: post search failed: {e}");
                        }
                    }
                }
            });
        }
    }
}
