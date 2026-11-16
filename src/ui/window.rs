//! The main window: a top bar with the search field, a sidebar (Home,
//! conversations, Profile) and a navigation view for the content.

use std::cell::{Cell, OnceCell, RefCell};
use std::collections::HashSet;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

use crate::api::{self, Client, Media, Reel, Session, Thread, User};
use crate::rt;
use crate::ui::{chat, comments, feed, login, post, profile, search, sidebar, stories};

pub struct Main {
    pub client: Client,
    pub window: adw::ApplicationWindow,
    pub toasts: adw::ToastOverlay,
    pub nav: adw::NavigationView,
    pub split: adw::OverlaySplitView,
    pub search_entry: gtk::SearchEntry,
    back_button: gtk::Button,
    root: adw::BreakpointBin,
    pub me: RefCell<Option<User>>,
    /// Accounts the user follows. `None` until loaded (or if loading failed).
    pub following: RefCell<Option<HashSet<String>>>,
    pub sidebar: OnceCell<Rc<sidebar::Sidebar>>,
    pub home: OnceCell<Rc<feed::HomePage>>,
    pub search: OnceCell<Rc<search::SearchPage>>,
    suppress_search: Cell<bool>,
    logged_out: Cell<bool>,
    /// When the "Instagram is limiting requests" toast was last shown.
    limit_toast: Cell<Option<std::time::Instant>>,
}

pub fn build(app: &adw::Application) {
    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("Finsta")
        .default_width(1180)
        .default_height(820)
        .width_request(360)
        .height_request(480)
        .build();
    let toasts = adw::ToastOverlay::new();
    window.set_content(Some(&toasts));
    match Session::load() {
        Some(session) => show_main(&window, &toasts, session),
        None => show_login(&window, &toasts),
    }
    window.present();
}

fn show_login(window: &adw::ApplicationWindow, toasts: &adw::ToastOverlay) {
    // Drop the previous session's UI state (stops its timers).
    unsafe {
        let _ = window.steal_data::<Rc<Main>>("finsta-main");
    }
    let page = login::page(
        window,
        Rc::new(glib::clone!(
            #[weak]
            window,
            #[weak]
            toasts,
            move |session| show_main(&window, &toasts, session)
        )),
    );
    toasts.set_child(Some(&page));
}

fn show_main(window: &adw::ApplicationWindow, toasts: &adw::ToastOverlay, session: Session) {
    let main = Main::new(window, toasts, Client::new(session));
    toasts.set_child(Some(&main.root));
    unsafe {
        window.set_data::<Rc<Main>>("finsta-main", main.clone());
    }
    main.start();
}

impl Main {
    fn new(window: &adw::ApplicationWindow, toasts: &adw::ToastOverlay, client: Client) -> Rc<Self> {
        let nav = adw::NavigationView::new();
        let split = adw::OverlaySplitView::builder()
            .min_sidebar_width(260.0)
            .max_sidebar_width(330.0)
            .sidebar_width_fraction(0.27)
            .content(&nav)
            .build();

        let header = adw::HeaderBar::new();
        let sidebar_button = gtk::ToggleButton::builder()
            .icon_name("sidebar-show-symbolic")
            .tooltip_text("Toggle Sidebar")
            .build();
        split
            .bind_property("show-sidebar", &sidebar_button, "active")
            .bidirectional()
            .sync_create()
            .build();
        let back_button = gtk::Button::builder()
            .icon_name("go-previous-symbolic")
            .tooltip_text("Back")
            .visible(false)
            .build();
        header.pack_start(&sidebar_button);
        header.pack_start(&back_button);

        let search_entry = gtk::SearchEntry::builder()
            .placeholder_text("Search people and conversations")
            .search_delay(350)
            .hexpand(true)
            .build();
        let clamp = adw::Clamp::builder()
            .maximum_size(520)
            .tightening_threshold(320)
            .hexpand(true)
            .child(&search_entry)
            .build();
        header.set_title_widget(Some(&clamp));

        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(&split));

        let root = adw::BreakpointBin::builder()
            .width_request(360)
            .height_request(480)
            .child(&toolbar)
            .build();
        let breakpoint = adw::Breakpoint::new(adw::BreakpointCondition::parse("max-width: 720sp").unwrap());
        breakpoint.add_setter(&split, "collapsed", Some(&true.to_value()));
        breakpoint.add_setter(&split, "show-sidebar", Some(&false.to_value()));
        root.add_breakpoint(breakpoint);

        let main = Rc::new(Main {
            client,
            window: window.clone(),
            toasts: toasts.clone(),
            nav,
            split,
            search_entry,
            back_button,
            root,
            me: RefCell::new(None),
            following: RefCell::new(None),
            sidebar: OnceCell::new(),
            home: OnceCell::new(),
            search: OnceCell::new(),
            suppress_search: Cell::new(false),
            logged_out: Cell::new(false),
            limit_toast: Cell::new(None),
        });

        let sidebar = sidebar::Sidebar::new(&main);
        main.split.set_sidebar(Some(&sidebar.widget));
        let _ = main.sidebar.set(sidebar);

        let home = feed::HomePage::new(&main);
        main.nav.add(&home.page);
        let _ = main.home.set(home);

        let search = search::SearchPage::new(&main);
        main.nav.add(&search.page);
        let _ = main.search.set(search);

        main.nav.replace_with_tags(&["home"]);
        main.connect_signals();
        main
    }

    fn connect_signals(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.back_button.connect_clicked(glib::clone!(
            #[strong]
            weak,
            move |_| {
                if let Some(main) = weak.upgrade() {
                    main.nav.pop();
                }
            }
        ));

        self.nav.connect_visible_page_notify(glib::clone!(
            #[strong]
            weak,
            move |_| {
                if let Some(main) = weak.upgrade() {
                    main.on_visible_page_changed();
                }
            }
        ));

        self.search_entry.connect_search_changed(glib::clone!(
            #[strong]
            weak,
            move |_| {
                if let Some(main) = weak.upgrade() {
                    main.on_search_changed();
                }
            }
        ));
        self.search_entry.connect_stop_search(|entry| entry.set_text(""));
        self.search_entry.connect_activate(glib::clone!(
            #[strong]
            weak,
            move |_| {
                if let Some(main) = weak.upgrade()
                    && let Some(search) = main.search.get()
                {
                    search.activate_first();
                }
            }
        ));

        let shortcuts = gtk::ShortcutController::new();
        shortcuts.set_scope(gtk::ShortcutScope::Managed);
        let focus_search = gtk::CallbackAction::new(glib::clone!(
            #[strong]
            weak,
            move |_, _| {
                if let Some(main) = weak.upgrade() {
                    main.search_entry.grab_focus();
                }
                glib::Propagation::Stop
            }
        ));
        shortcuts.add_shortcut(gtk::Shortcut::new(
            gtk::ShortcutTrigger::parse_string("<Control>f"),
            Some(focus_search),
        ));
        self.root.add_controller(shortcuts);
    }

    fn start(self: &Rc<Self>) {
        if api::cooldown_remaining().is_some() {
            self.limit_toast.set(Some(std::time::Instant::now()));
            self.toast("Signed in. Instagram is limiting requests right now, so things will load in a few minutes.");
        }
        self.load_me();
        if let Some(home) = self.home.get() {
            home.refresh();
        }
        if let Some(sidebar) = self.sidebar.get() {
            sidebar.start();
        }
    }

    /// Load the signed-in account's profile, waiting out a rate limit first.
    fn load_me(self: &Rc<Self>) {
        if let Some(wait) = api::cooldown_remaining() {
            let weak = Rc::downgrade(self);
            glib::timeout_add_local_once(wait + std::time::Duration::from_secs(2), move || {
                if let Some(main) = weak.upgrade()
                    && !main.logged_out.get()
                {
                    main.load_me();
                }
            });
            return;
        }
        let weak = Rc::downgrade(self);
        let client = self.client.clone();
        let uid = client.user_id();
        glib::spawn_future_local(async move {
            let res = rt::bg(async move { client.user_info(&uid).await }).await;
            let Some(main) = weak.upgrade() else { return };
            match res {
                Ok(user) => {
                    if let Some(sidebar) = main.sidebar.get() {
                        sidebar.set_me(&user);
                    }
                    main.me.replace(Some(user));
                }
                Err(e) if api::is_rate_limited(&e) => main.load_me(),
                Err(e) => main.handle_error(&e),
            }
        });
    }

    // ----- Navigation ----------------------------------------------------

    fn in_stack(&self, tag: &str) -> bool {
        let stack = self.nav.navigation_stack();
        (0..stack.n_items()).any(|i| {
            stack
                .item(i)
                .and_downcast::<adw::NavigationPage>()
                .and_then(|p| p.tag())
                .is_some_and(|t| t == tag)
        })
    }

    fn visible_tag(&self) -> String {
        self.nav
            .visible_page()
            .and_then(|p| p.tag())
            .map(|t| t.to_string())
            .unwrap_or_default()
    }

    fn on_visible_page_changed(self: &Rc<Self>) {
        let page = self.nav.visible_page();
        let can_go_back = page.as_ref().is_some_and(|p| self.nav.previous_page(p).is_some());
        self.back_button.set_visible(can_go_back);
        if !self.in_stack("search") && !self.search_entry.text().is_empty() {
            self.clear_search();
        }
        if let Some(sidebar) = self.sidebar.get() {
            sidebar.sync_selection(&self.visible_tag());
        }
    }

    fn clear_search(&self) {
        self.suppress_search.set(true);
        self.search_entry.set_text("");
        self.suppress_search.set(false);
    }

    fn on_search_changed(self: &Rc<Self>) {
        if self.suppress_search.get() {
            return;
        }
        let text = self.search_entry.text().trim().to_string();
        let Some(search) = self.search.get() else { return };
        if text.is_empty() {
            if self.visible_tag() == "search" {
                self.nav.pop();
            }
            search.set_query("");
            return;
        }
        if self.visible_tag() != "search" {
            if self.in_stack("search") {
                self.nav.pop_to_tag("search");
            } else {
                self.nav.push_by_tag("search");
            }
        }
        search.set_query(&text);
    }

    /// On narrow windows the sidebar overlays the content; hide it after a choice.
    fn after_sidebar_choice(&self) {
        if self.split.is_collapsed() {
            self.split.set_show_sidebar(false);
        }
    }

    pub fn show_home(self: &Rc<Self>) {
        self.clear_search();
        self.nav.replace_with_tags(&["home"]);
        self.after_sidebar_choice();
    }

    /// Open a conversation. From the sidebar it replaces the current content;
    /// from elsewhere (e.g. a profile) it is pushed so Back returns there.
    pub fn open_thread(self: &Rc<Self>, thread: Thread, from_sidebar: bool) {
        let tag = format!("thread:{}", thread.thread_id);
        if !from_sidebar && self.in_stack(&tag) {
            self.nav.pop_to_tag(&tag);
            return;
        }
        let page = chat::page(self, chat::ChatTarget::Thread(thread));
        page.set_tag(Some(&tag));
        if from_sidebar {
            self.clear_search();
            self.nav.replace(&[page]);
            self.after_sidebar_choice();
        } else {
            self.nav.push(&page);
        }
    }

    pub fn open_chat_with_user(self: &Rc<Self>, user: User) {
        if let Some(thread) = self.sidebar.get().and_then(|s| s.direct_thread_with(&user.pk)) {
            self.open_thread(thread, false);
            return;
        }
        let tag = format!("dm:{}", user.pk);
        if self.in_stack(&tag) {
            self.nav.pop_to_tag(&tag);
            return;
        }
        let page = chat::page(self, chat::ChatTarget::User(user));
        page.set_tag(Some(&tag));
        self.nav.push(&page);
    }

    pub fn open_profile(self: &Rc<Self>, user: User) {
        if user.pk == self.client.user_id() {
            if self.in_stack("me") {
                self.nav.pop_to_tag("me");
                return;
            }
            let page = profile::page(self, user);
            page.set_tag(Some("me"));
            self.nav.push(&page);
            return;
        }
        let tag = format!("profile:{}", user.pk);
        if self.in_stack(&tag) {
            self.nav.pop_to_tag(&tag);
            return;
        }
        let page = profile::page(self, user);
        page.set_tag(Some(&tag));
        self.nav.push(&page);
    }

    pub fn open_my_profile(self: &Rc<Self>) {
        let user = self.me.borrow().clone().unwrap_or_else(|| User {
            pk: self.client.user_id(),
            username: self.client.session().username,
            ..Default::default()
        });
        let page = profile::page(self, user);
        page.set_tag(Some("me"));
        self.clear_search();
        self.nav.replace(&[page]);
        self.after_sidebar_choice();
    }

    pub fn open_post(self: &Rc<Self>, media: Media) {
        let tag = format!("post:{}", media.id);
        if self.in_stack(&tag) {
            self.nav.pop_to_tag(&tag);
            return;
        }
        let page = post::page(self, &media);
        page.set_tag(Some(&tag));
        self.nav.push(&page);
    }

    pub fn open_stories(self: &Rc<Self>, reels: Vec<Reel>, start: usize) {
        stories::open(self, reels, start);
    }

    pub fn show_comments(self: &Rc<Self>, media: &Media) {
        comments::open(self, media);
    }

    // ----- Feed rules ----------------------------------------------------

    /// Whether a post belongs in the home feed: only accounts you follow (and
    /// your own posts), never ads or suggestions.
    pub fn is_followed(&self, user: &User) -> bool {
        if user.pk == self.client.user_id() {
            return true;
        }
        let flagged = user.friendship_status.as_ref().map(|f| f.following);
        match self.following.borrow().as_ref() {
            Some(set) => set.contains(&user.pk) || flagged == Some(true),
            None => flagged != Some(false),
        }
    }

    pub fn accept_post(&self, media: &Media) -> bool {
        !media.is_ad() && media.user.as_ref().is_some_and(|u| self.is_followed(u))
    }

    pub fn set_following(&self, pk: &str, following: bool) {
        if let Some(set) = self.following.borrow_mut().as_mut() {
            if following {
                set.insert(pk.to_string());
            } else {
                set.remove(pk);
            }
        }
    }

    // ----- Feedback ------------------------------------------------------

    pub fn toast(&self, message: &str) {
        let toast = adw::Toast::new(message);
        toast.set_timeout(4);
        self.toasts.add_toast(toast);
    }

    pub fn handle_error(self: &Rc<Self>, err: &anyhow::Error) {
        if api::is_login_required(err) {
            if !self.logged_out.replace(true) {
                self.toast(&err.to_string());
                Session::delete();
                show_login(&self.window, &self.toasts);
            }
            return;
        }
        eprintln!("finsta: {err:#}");
        if api::is_rate_limited(err) {
            // Several pages fail at once when this happens; say it once.
            let recent = self.limit_toast.get().is_some_and(|t| t.elapsed().as_secs() < 60);
            if recent {
                return;
            }
            self.limit_toast.set(Some(std::time::Instant::now()));
        }
        self.toast(&format!("{err}"));
    }

    pub fn logout(self: &Rc<Self>) {
        if self.logged_out.replace(true) {
            return;
        }
        let client = self.client.clone();
        rt::runtime().spawn(async move {
            let _ = client.logout().await;
        });
        Session::delete();
        show_login(&self.window, &self.toasts);
    }
}
