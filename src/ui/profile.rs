//! Profile page. Shows a fixed number of posts; more only load when you ask
//! for them with a button — no endless scrolling here.

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

use adw::prelude::*;
use gtk::glib;

use crate::api::{FriendshipStatus, Media, User};
use crate::rt;
use crate::ui::images;
use crate::ui::ratio_bin::RatioBin;
use crate::ui::util;
use crate::ui::window::Main;

struct Profile {
    main: Weak<Main>,
    user: RefCell<User>,
    is_me: bool,
    avatar_slot: gtk::Box,
    username: gtk::Label,
    name: gtk::Label,
    counts: gtk::Label,
    bio: gtk::Label,
    link: gtk::LinkButton,
    actions: gtk::Box,
    status: gtk::Box,
    grid: gtk::FlowBox,
    more_button: gtk::Button,
    posts: RefCell<Vec<Media>>,
    next_max_id: RefCell<Option<String>>,
    friendship: RefCell<Option<FriendshipStatus>>,
    loading_posts: Cell<bool>,
}

pub fn page(main: &Rc<Main>, user: User) -> adw::NavigationPage {
    let is_me = user.pk == main.client.user_id();
    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(18)
        .margin_top(24)
        .margin_bottom(24)
        .margin_start(16)
        .margin_end(16)
        .build();

    let header = gtk::Box::new(gtk::Orientation::Horizontal, 24);
    let avatar_slot = gtk::Box::builder().valign(gtk::Align::Start).build();
    avatar_slot.append(&util::avatar(&user, 96));
    header.append(&avatar_slot);
    let info = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(4)
        .hexpand(true)
        .valign(gtk::Align::Center)
        .build();
    let username = gtk::Label::builder()
        .label(&user.username)
        .xalign(0.0)
        .wrap(true)
        .css_classes(["title-2"])
        .build();
    let name = gtk::Label::builder()
        .label(&user.full_name)
        .xalign(0.0)
        .wrap(true)
        .css_classes(["heading"])
        .build();
    let counts = gtk::Label::builder()
        .xalign(0.0)
        .wrap(true)
        .css_classes(["profile-counts", "dim-label"])
        .build();
    let bio = gtk::Label::builder()
        .xalign(0.0)
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::WordChar)
        .use_markup(true)
        .visible(false)
        .build();
    let link = gtk::LinkButton::builder()
        .halign(gtk::Align::Start)
        .visible(false)
        .build();
    info.append(&username);
    info.append(&name);
    info.append(&counts);
    info.append(&bio);
    info.append(&link);
    header.append(&info);
    content.append(&header);

    let actions = gtk::Box::builder().spacing(8).homogeneous(true).build();
    content.append(&actions);
    content.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

    let status = gtk::Box::new(gtk::Orientation::Vertical, 0);
    status.append(&util::spinner_box());
    content.append(&status);

    let grid = gtk::FlowBox::builder()
        .min_children_per_line(3)
        .max_children_per_line(3)
        .homogeneous(true)
        .column_spacing(4)
        .row_spacing(4)
        .selection_mode(gtk::SelectionMode::None)
        .build();
    content.append(&grid);
    let more_button = gtk::Button::builder()
        .label("Show More Posts")
        .css_classes(["pill"])
        .halign(gtk::Align::Center)
        .visible(false)
        .build();
    content.append(&more_button);

    let clamp = adw::Clamp::builder().maximum_size(760).child(&content).build();
    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&clamp)
        .build();
    let page = adw::NavigationPage::builder()
        .title(if is_me { "Your Profile".to_string() } else { format!("@{}", user.username) })
        .child(&scroller)
        .build();

    let profile = Rc::new(Profile {
        main: Rc::downgrade(main),
        user: RefCell::new(user),
        is_me,
        avatar_slot,
        username,
        name,
        counts,
        bio,
        link,
        actions,
        status,
        grid,
        more_button,
        posts: RefCell::new(Vec::new()),
        next_max_id: RefCell::new(None),
        friendship: RefCell::new(None),
        loading_posts: Cell::new(false),
    });

    let weak = Rc::downgrade(&profile);
    profile.grid.connect_child_activated(glib::clone!(
        #[strong]
        weak,
        move |_, child| {
            let Some(p) = weak.upgrade() else { return };
            let media = p.posts.borrow().get(child.index() as usize).cloned();
            if let (Some(media), Some(main)) = (media, p.main.upgrade()) {
                main.open_post(media);
            }
        }
    ));
    profile.more_button.connect_clicked(glib::clone!(
        #[strong]
        weak,
        move |_| {
            if let Some(p) = weak.upgrade() {
                p.load_posts();
            }
        }
    ));

    profile.render_info();
    profile.render_actions();
    profile.load();
    util::attach(&page, profile);
    page
}

impl Profile {
    fn render_info(&self) {
        let user = self.user.borrow();
        self.username.set_label(&user.username);
        self.name.set_label(&user.full_name);
        self.name.set_visible(!user.full_name.is_empty());
        if user.media_count > 0 || user.follower_count > 0 {
            self.counts.set_label(&format!(
                "{}  ·  {}  ·  {} following",
                util::plural(user.media_count, "post", "posts"),
                util::plural(user.follower_count, "follower", "followers"),
                util::count(user.following_count),
            ));
        }
        self.bio.set_markup(&util::linkify(&user.biography));
        self.bio.set_visible(!user.biography.trim().is_empty());
        if !user.external_url.is_empty() {
            let shown = user
                .external_url
                .trim_start_matches("https://")
                .trim_start_matches("http://")
                .trim_end_matches('/');
            self.link.set_uri(&user.external_url);
            self.link.set_label(shown);
            self.link.set_visible(true);
        }
    }

    fn render_actions(self: &Rc<Self>) {
        util::clear_box(&self.actions);
        let user = self.user.borrow().clone();
        let weak = Rc::downgrade(self);
        if self.is_me {
            let open = gtk::Button::builder().label("Open on Instagram").build();
            let url = format!("https://www.instagram.com/{}/", user.username);
            open.connect_clicked(move |b| util::open_uri(b, &url));
            let logout = gtk::Button::builder()
                .label("Log Out")
                .css_classes(["destructive-action"])
                .build();
            logout.connect_clicked(glib::clone!(
                #[strong]
                weak,
                move |_| {
                    if let Some(p) = weak.upgrade() {
                        p.confirm_logout();
                    }
                }
            ));
            self.actions.append(&open);
            self.actions.append(&logout);
            return;
        }

        let friendship = self.friendship.borrow().clone();
        let follow = gtk::Button::new();
        match &friendship {
            Some(f) if f.following => follow.set_label("Following"),
            Some(f) if f.outgoing_request => follow.set_label("Requested"),
            Some(_) => {
                follow.set_label("Follow");
                follow.add_css_class("suggested-action");
            }
            None => {
                follow.set_label("Follow");
                follow.set_sensitive(false);
            }
        }
        follow.connect_clicked(glib::clone!(
            #[strong]
            weak,
            move |_| {
                if let Some(p) = weak.upgrade() {
                    p.toggle_follow();
                }
            }
        ));
        let message = gtk::Button::builder().label("Message").build();
        message.connect_clicked(glib::clone!(
            #[strong]
            weak,
            move |_| {
                if let Some(p) = weak.upgrade()
                    && let Some(main) = p.main.upgrade()
                {
                    main.open_chat_with_user(p.user.borrow().clone());
                }
            }
        ));
        let open = gtk::Button::builder()
            .icon_name("web-browser-symbolic")
            .tooltip_text("Open on instagram.com")
            .build();
        let url = format!("https://www.instagram.com/{}/", user.username);
        open.connect_clicked(move |b| util::open_uri(b, &url));
        if friendship.as_ref().is_some_and(|f| f.followed_by) && !self.counts.label().ends_with("Follows you") {
            self.counts.set_label(&format!("{}  ·  Follows you", self.counts.label()));
        }
        self.actions.set_homogeneous(false);
        follow.set_hexpand(true);
        message.set_hexpand(true);
        self.actions.append(&follow);
        self.actions.append(&message);
        self.actions.append(&open);
    }

    fn load(self: &Rc<Self>) {
        let Some(main) = self.main.upgrade() else { return };
        let client = main.client.clone();
        let pk = self.user.borrow().pk.clone();
        let known = self.user.borrow().clone();
        let is_me = self.is_me;
        let weak = Rc::downgrade(self);
        self.loading_posts.set(true);
        glib::spawn_future_local(async move {
            let (info, friendship, feed) = rt::bg(async move {
                let friendship = async {
                    if is_me { None } else { Some(client.friendship(&pk).await) }
                };
                tokio::join!(client.profile(&known), friendship, client.user_feed(&known, None))
            })
            .await;
            let Some(p) = weak.upgrade() else { return };
            let Some(main) = p.main.upgrade() else { return };
            p.loading_posts.set(false);
            match info {
                Ok(user) => {
                    util::clear_box(&p.avatar_slot);
                    let avatar = adw::Avatar::new(96, Some(&user.display_name()), true);
                    images::load_avatar(&avatar, Some(user.best_pic()));
                    p.avatar_slot.append(&avatar);
                    p.user.replace(user);
                    p.render_info();
                }
                Err(e) => main.handle_error(&e),
            }
            if let Some(Ok(f)) = friendship {
                p.friendship.replace(Some(f));
            }
            p.render_actions();
            util::clear_box(&p.status);
            match feed {
                Ok(page) => {
                    if page.items.is_empty() {
                        p.status.append(&p.message("No posts yet"));
                    }
                    p.add_posts(page.items);
                    p.next_max_id.replace(page.next_max_id.filter(|_| page.more_available));
                    p.more_button.set_visible(p.next_max_id.borrow().is_some());
                }
                Err(e) if crate::api::is_login_required(&e) => main.handle_error(&e),
                Err(e) => {
                    let user = p.user.borrow();
                    let following = p.friendship.borrow().as_ref().is_some_and(|f| f.following);
                    let text = if user.is_private && !following {
                        "This account is private".to_string()
                    } else {
                        e.to_string()
                    };
                    p.status.append(&p.message(&text));
                }
            }
        });
    }

    fn message(&self, text: &str) -> gtk::Widget {
        gtk::Label::builder()
            .label(text)
            .wrap(true)
            .css_classes(["dim-label", "title-4"])
            .margin_top(24)
            .margin_bottom(24)
            .build()
            .upcast()
    }

    fn add_posts(&self, items: Vec<Media>) {
        for media in items {
            self.grid.append(&thumbnail(&media));
            self.posts.borrow_mut().push(media);
        }
    }

    fn load_posts(self: &Rc<Self>) {
        let Some(max_id) = self.next_max_id.borrow().clone() else { return };
        if self.loading_posts.replace(true) {
            return;
        }
        let Some(main) = self.main.upgrade() else { return };
        self.more_button.set_sensitive(false);
        let client = main.client.clone();
        let user = self.user.borrow().clone();
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let res = rt::bg(async move { client.user_feed(&user, Some(max_id)).await }).await;
            let Some(p) = weak.upgrade() else { return };
            p.loading_posts.set(false);
            p.more_button.set_sensitive(true);
            match res {
                Ok(page) => {
                    p.add_posts(page.items);
                    p.next_max_id.replace(page.next_max_id.filter(|_| page.more_available));
                    p.more_button.set_visible(p.next_max_id.borrow().is_some());
                }
                Err(e) => {
                    if let Some(main) = p.main.upgrade() {
                        main.handle_error(&e);
                    }
                }
            }
        });
    }

    fn toggle_follow(self: &Rc<Self>) {
        let Some(main) = self.main.upgrade() else { return };
        let following = self
            .friendship
            .borrow()
            .as_ref()
            .is_some_and(|f| f.following || f.outgoing_request);
        if following {
            let username = self.user.borrow().username.clone();
            let dialog = adw::AlertDialog::new(
                Some(&format!("Unfollow @{username}?")),
                Some("Their posts and stories will no longer appear in your home feed."),
            );
            dialog.add_responses(&[("cancel", "Cancel"), ("unfollow", "Unfollow")]);
            dialog.set_response_appearance("unfollow", adw::ResponseAppearance::Destructive);
            dialog.set_default_response(Some("cancel"));
            dialog.set_close_response("cancel");
            let weak = Rc::downgrade(self);
            dialog.connect_response(None, move |_, response| {
                if response == "unfollow"
                    && let Some(p) = weak.upgrade()
                {
                    p.set_follow(false);
                }
            });
            dialog.present(Some(&main.window));
        } else {
            self.set_follow(true);
        }
    }

    fn set_follow(self: &Rc<Self>, follow: bool) {
        let Some(main) = self.main.upgrade() else { return };
        let client = main.client.clone();
        let pk = self.user.borrow().pk.clone();
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let pk2 = pk.clone();
            let res = rt::bg(async move { client.set_following(&pk2, follow).await }).await;
            let Some(p) = weak.upgrade() else { return };
            let Some(main) = p.main.upgrade() else { return };
            match res {
                Ok(status) => {
                    main.set_following(&pk, status.following);
                    p.friendship.replace(Some(status));
                    p.render_actions();
                }
                Err(e) => main.handle_error(&e),
            }
        });
    }

    fn confirm_logout(self: &Rc<Self>) {
        let Some(main) = self.main.upgrade() else { return };
        let dialog = adw::AlertDialog::new(
            Some("Log out?"),
            Some("Your saved session will be removed from this computer."),
        );
        dialog.add_responses(&[("cancel", "Cancel"), ("logout", "Log Out")]);
        dialog.set_response_appearance("logout", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        let weak = Rc::downgrade(&main);
        dialog.connect_response(None, move |_, response| {
            if response == "logout"
                && let Some(main) = weak.upgrade()
            {
                main.logout();
            }
        });
        dialog.present(Some(&main.window));
    }
}

fn thumbnail(media: &Media) -> gtk::FlowBoxChild {
    let picture = gtk::Picture::builder()
        .content_fit(gtk::ContentFit::Cover)
        .can_shrink(true)
        .css_classes(["post-media"])
        .build();
    images::load_picture(&picture, media.image_url(320));
    let overlay = gtk::Overlay::builder().child(&picture).build();
    let badge = if media.is_carousel() {
        Some("view-paged-symbolic")
    } else if media.is_video() {
        Some("media-playback-start-symbolic")
    } else {
        None
    };
    if let Some(icon) = badge {
        overlay.add_overlay(
            &gtk::Image::builder()
                .icon_name(icon)
                .halign(gtk::Align::End)
                .valign(gtk::Align::Start)
                .css_classes(["grid-badge"])
                .build(),
        );
    }
    let bin = RatioBin::new(1.0, 220, &overlay);
    let child = gtk::FlowBoxChild::builder().child(&bin).build();
    child.set_cursor_from_name(Some("pointer"));
    let caption: String = media.caption_text().chars().take(120).collect();
    if !caption.is_empty() {
        child.set_tooltip_text(Some(&caption));
    }
    child
}
