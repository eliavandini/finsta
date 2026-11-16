//! The home feed: stories and posts from accounts you follow, in the order
//! Instagram serves them, ending at "You're all caught up". This is the only
//! place in Finsta that loads more content as you scroll, and it stops for
//! good once the posts from people you follow run out.

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::rc::{Rc, Weak};

use adw::prelude::*;
use gtk::glib;

use crate::api::Reel;
use crate::rt;
use crate::ui::{post, util};
use crate::ui::window::Main;

pub struct HomePage {
    main: Weak<Main>,
    pub page: adw::NavigationPage,
    scroller: gtk::ScrolledWindow,
    stories_scroller: gtk::ScrolledWindow,
    stories_box: gtk::Box,
    posts_box: gtk::Box,
    footer: gtk::Box,
    reels: RefCell<Vec<Reel>>,
    next_max_id: RefCell<Option<String>>,
    more: Cell<bool>,
    caught_up: Cell<bool>,
    loading: Cell<bool>,
    first_page: Cell<bool>,
    seen: RefCell<HashSet<String>>,
    generation: Cell<u64>,
    empty_pages: Cell<u32>,
    shown: Cell<usize>,
}

impl HomePage {
    pub fn new(main: &Rc<Main>) -> Rc<Self> {
        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(16)
            .margin_top(12)
            .margin_bottom(12)
            .margin_start(12)
            .margin_end(12)
            .build();

        let top = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let title = gtk::Label::builder()
            .label("Home")
            .xalign(0.0)
            .hexpand(true)
            .css_classes(["title-2"])
            .build();
        let refresh = gtk::Button::builder()
            .icon_name("view-refresh-symbolic")
            .tooltip_text("Refresh Feed")
            .css_classes(["flat", "circular"])
            .build();
        top.append(&title);
        top.append(&refresh);
        content.append(&top);

        let stories_box = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let stories_scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Automatic)
            .vscrollbar_policy(gtk::PolicyType::Never)
            .child(&stories_box)
            .visible(false)
            .build();
        content.append(&stories_scroller);

        let posts_box = gtk::Box::new(gtk::Orientation::Vertical, 18);
        content.append(&posts_box);
        let footer = gtk::Box::new(gtk::Orientation::Vertical, 0);
        content.append(&footer);

        let clamp = adw::Clamp::builder()
            .maximum_size(620)
            .tightening_threshold(480)
            .child(&content)
            .build();
        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .child(&clamp)
            .build();

        let page = adw::NavigationPage::builder()
            .title("Home")
            .tag("home")
            .child(&scroller)
            .build();

        let home = Rc::new(HomePage {
            main: Rc::downgrade(main),
            page,
            scroller,
            stories_scroller,
            stories_box,
            posts_box,
            footer,
            reels: RefCell::new(Vec::new()),
            next_max_id: RefCell::new(None),
            more: Cell::new(true),
            caught_up: Cell::new(false),
            loading: Cell::new(false),
            first_page: Cell::new(true),
            seen: RefCell::new(HashSet::new()),
            generation: Cell::new(0),
            empty_pages: Cell::new(0),
            shown: Cell::new(0),
        });

        let weak = Rc::downgrade(&home);
        refresh.connect_clicked(glib::clone!(
            #[strong]
            weak,
            move |_| {
                if let Some(h) = weak.upgrade() {
                    h.refresh();
                }
            }
        ));
        // Prefetch the next page shortly before the bottom is reached.
        home.scroller.vadjustment().connect_value_changed(glib::clone!(
            #[strong]
            weak,
            move |adj| {
                if adj.value() + adj.page_size() > adj.upper() - 1500.0
                    && let Some(h) = weak.upgrade()
                {
                    h.load_more();
                }
            }
        ));
        home
    }

    pub fn refresh(self: &Rc<Self>) {
        self.generation.set(self.generation.get() + 1);
        util::clear_box(&self.posts_box);
        self.seen.borrow_mut().clear();
        self.next_max_id.replace(None);
        self.more.set(true);
        self.caught_up.set(false);
        self.loading.set(false);
        self.first_page.set(true);
        self.empty_pages.set(0);
        self.shown.set(0);
        self.scroller.vadjustment().set_value(0.0);
        self.load_more();
    }

    fn set_footer(&self, widget: Option<&gtk::Widget>) {
        util::clear_box(&self.footer);
        if let Some(w) = widget {
            self.footer.append(w);
        }
    }

    fn caught_up_widget(&self) -> gtk::Widget {
        let b = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .css_classes(["caught-up"])
            .build();
        let icon = gtk::Image::builder()
            .icon_name("object-select-symbolic")
            .pixel_size(48)
            .css_classes(["ok-icon"])
            .build();
        let (title, subtitle) = if self.shown.get() == 0 {
            (
                "No new posts",
                "When people you follow share something, it will show up here.",
            )
        } else {
            (
                "You're all caught up",
                "You've seen all new posts from people you follow.",
            )
        };
        let title = gtk::Label::builder().label(title).css_classes(["title-3"]).build();
        let subtitle = gtk::Label::builder()
            .label(subtitle)
            .wrap(true)
            .justify(gtk::Justification::Center)
            .css_classes(["dim-label"])
            .build();
        b.append(&icon);
        b.append(&title);
        b.append(&subtitle);
        b.upcast()
    }

    fn error_widget(self: &Rc<Self>, message: &str) -> gtk::Widget {
        let b = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .margin_top(24)
            .margin_bottom(24)
            .halign(gtk::Align::Center)
            .build();
        let label = gtk::Label::builder()
            .label(message)
            .wrap(true)
            .justify(gtk::Justification::Center)
            .css_classes(["dim-label"])
            .build();
        let retry = gtk::Button::builder()
            .label("Try Again")
            .css_classes(["pill"])
            .halign(gtk::Align::Center)
            .build();
        let weak = Rc::downgrade(self);
        retry.connect_clicked(move |_| {
            if let Some(h) = weak.upgrade() {
                h.load_more();
            }
        });
        b.append(&label);
        b.append(&retry);
        b.upcast()
    }

    pub fn load_more(self: &Rc<Self>) {
        if self.loading.get() || self.caught_up.get() || !self.more.get() {
            return;
        }
        let Some(main) = self.main.upgrade() else { return };
        self.loading.set(true);
        self.set_footer(Some(util::spinner_box().upcast_ref()));

        let generation = self.generation.get();
        let max_id = self.next_max_id.borrow().clone();
        let is_first = self.first_page.get();
        let need_following = main.following.borrow().is_none();
        let client = main.client.clone();
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let (following, page) = rt::bg(async move {
                let following = async {
                    if need_following {
                        client.following_ids().await.ok()
                    } else {
                        None
                    }
                };
                tokio::join!(following, client.timeline(max_id, false))
            })
            .await;
            let Some(home) = weak.upgrade() else { return };
            let Some(main) = home.main.upgrade() else { return };
            if home.generation.get() != generation {
                return;
            }
            home.loading.set(false);
            if let Some(set) = following {
                main.following.replace(Some(set));
            }
            match page {
                Ok(page) => {
                    home.first_page.set(false);
                    let mut added = 0;
                    for media in page.posts {
                        if !main.accept_post(&media) || !home.seen.borrow_mut().insert(media.id.clone()) {
                            continue;
                        }
                        home.posts_box.append(&post::card(&main, &media));
                        added += 1;
                    }
                    home.shown.set(home.shown.get() + added);
                    home.next_max_id.replace(page.next_max_id.clone());
                    home.more.set(page.more_available && page.next_max_id.is_some());
                    home.caught_up.set(page.caught_up);

                    if home.caught_up.get() || !home.more.get() {
                        let w = home.caught_up_widget();
                        home.set_footer(Some(&w));
                    } else {
                        home.set_footer(None);
                        if added == 0 && home.empty_pages.get() < 3 {
                            // A page with nothing from people you follow; try the next one.
                            home.empty_pages.set(home.empty_pages.get() + 1);
                            home.load_more();
                        } else {
                            home.empty_pages.set(0);
                            home.fill_viewport();
                        }
                    }
                    if is_first {
                        home.load_stories();
                    }
                }
                Err(e) => {
                    let w = home.error_widget(&e.to_string());
                    home.set_footer(Some(&w));
                    main.handle_error(&e);
                }
            }
        });
    }

    /// If the posts don't fill the window yet, there's nothing to scroll, so
    /// fetch the next page right away.
    fn fill_viewport(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        glib::timeout_add_local_once(std::time::Duration::from_millis(300), move || {
            if let Some(h) = weak.upgrade() {
                let adj = h.scroller.vadjustment();
                if adj.upper() <= adj.page_size() + 200.0 {
                    h.load_more();
                }
            }
        });
    }

    fn load_stories(self: &Rc<Self>) {
        let Some(main) = self.main.upgrade() else { return };
        let client = main.client.clone();
        let weak = Rc::downgrade(self);
        let generation = self.generation.get();
        glib::spawn_future_local(async move {
            let res = rt::bg(async move { client.reels_tray().await }).await;
            let Some(home) = weak.upgrade() else { return };
            let Some(main) = home.main.upgrade() else { return };
            if home.generation.get() != generation {
                return;
            }
            match res {
                Ok(tray) => {
                    let me = main.client.user_id();
                    let mut reels: Vec<Reel> = tray
                        .into_iter()
                        .filter(|r| !r.user.pk.is_empty() && main.is_followed(&r.user))
                        .collect();
                    // Your own story first, then unseen ones, then the rest.
                    reels.sort_by_key(|r| (r.user.pk != me, !r.has_unseen()));
                    home.reels.replace(reels);
                    home.render_stories();
                }
                Err(e) => {
                    if crate::api::is_login_required(&e) {
                        main.handle_error(&e);
                    } else {
                        eprintln!("finsta: could not load stories: {e:#}");
                    }
                }
            }
        });
    }

    fn render_stories(self: &Rc<Self>) {
        util::clear_box(&self.stories_box);
        let reels = self.reels.borrow().clone();
        self.stories_scroller.set_visible(!reels.is_empty());
        let Some(main) = self.main.upgrade() else { return };
        let me = main.client.user_id();
        for (index, reel) in reels.iter().enumerate() {
            let ring = gtk::Box::builder()
                .css_classes(["story-ring"])
                .halign(gtk::Align::Center)
                .build();
            if reel.has_unseen() {
                ring.add_css_class("unseen");
            }
            let gap = gtk::Box::builder().css_classes(["story-gap"]).build();
            gap.append(&util::avatar(&reel.user, 58));
            ring.append(&gap);
            let name = if reel.user.pk == me {
                "Your story".to_string()
            } else {
                reel.user.username.clone()
            };
            let label = gtk::Label::builder()
                .label(&name)
                .max_width_chars(10)
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .css_classes(["caption"])
                .build();
            let column = gtk::Box::new(gtk::Orientation::Vertical, 4);
            column.append(&ring);
            column.append(&label);
            let button = gtk::Button::builder()
                .child(&column)
                .css_classes(["flat", "story-button"])
                .tooltip_text(reel.user.display_name())
                .build();
            let weak = Rc::downgrade(self);
            button.connect_clicked(move |_| {
                let Some(home) = weak.upgrade() else { return };
                let Some(main) = home.main.upgrade() else { return };
                let reels = home.reels.borrow().clone();
                main.open_stories(reels, index);
            });
            self.stories_box.append(&button);
        }
    }

    /// Stories viewer reports which reels were watched (and their items, so
    /// reopening doesn't refetch).
    pub fn reel_viewed(self: &Rc<Self>, reel: &Reel) {
        let mut changed = false;
        if let Some(r) = self.reels.borrow_mut().iter_mut().find(|r| r.id == reel.id) {
            if r.items.is_empty() {
                r.items = reel.items.clone();
            }
            if r.has_unseen() {
                r.seen = r.latest_reel_media;
                changed = true;
            }
        }
        if changed {
            self.render_stories();
        }
    }
}
