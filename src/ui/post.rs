//! A post card (header, media, actions, caption) and a standalone post page.

use std::cell::Cell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};

use crate::api::Media;
use crate::rt;
use crate::ui::images;
use crate::ui::ratio_bin::RatioBin;
use crate::ui::util;
use crate::ui::window::Main;

const MEDIA_WIDTH: i32 = 600;

pub fn page(main: &Rc<Main>, media: &Media) -> adw::NavigationPage {
    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .margin_top(12)
        .margin_bottom(24)
        .margin_start(12)
        .margin_end(12)
        .build();
    content.append(&card(main, media));
    let clamp = adw::Clamp::builder().maximum_size(620).child(&content).build();
    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&clamp)
        .build();
    let owner = media.user.as_ref().map(|u| u.username.clone()).unwrap_or_default();
    adw::NavigationPage::builder()
        .title(format!("Post by @{owner}"))
        .child(&scroller)
        .build()
}

pub fn card(main: &Rc<Main>, media: &Media) -> gtk::Widget {
    let card = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .css_classes(["card", "post"])
        .overflow(gtk::Overflow::Hidden)
        .build();
    let user = media.user.clone().unwrap_or_default();

    // Header
    let header = gtk::Box::builder().spacing(10).css_classes(["post-header"]).build();
    let avatar_button = gtk::Button::builder()
        .child(&util::avatar(&user, 36))
        .css_classes(["flat", "circular"])
        .tooltip_text(format!("View @{}'s profile", user.username))
        .build();
    let names = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .valign(gtk::Align::Center)
        .hexpand(true)
        .build();
    let name_button = gtk::Button::builder()
        .css_classes(["flat"])
        .halign(gtk::Align::Start)
        .build();
    let name_label = gtk::Label::builder()
        .label(&user.username)
        .css_classes(["username"])
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .build();
    if user.is_verified {
        let b = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        b.append(&name_label);
        b.append(&gtk::Image::from_icon_name("emblem-ok-symbolic"));
        name_button.set_child(Some(&b));
    } else {
        name_button.set_child(Some(&name_label));
    }
    names.append(&name_button);
    if let Some(loc) = media.location.as_ref().filter(|l| !l.name.is_empty()) {
        let l = gtk::Label::builder()
            .label(&loc.name)
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .css_classes(["caption", "dim-label"])
            .build();
        names.append(&l);
    }
    let time = gtk::Label::builder()
        .label(util::relative_time(media.taken_at))
        .tooltip_text(util::full_time(media.taken_at))
        .css_classes(["dim-label", "caption"])
        .build();
    header.append(&avatar_button);
    header.append(&names);
    header.append(&time);
    if let Some(url) = media.web_url() {
        let open = gtk::Button::builder()
            .icon_name("web-browser-symbolic")
            .tooltip_text("Open on instagram.com")
            .css_classes(["flat", "circular"])
            .build();
        open.connect_clicked(move |b| util::open_uri(b, &url));
        header.append(&open);
    }
    for button in [&avatar_button, &name_button] {
        let weak = Rc::downgrade(main);
        let user = user.clone();
        button.connect_clicked(move |_| {
            if let Some(main) = weak.upgrade() {
                main.open_profile(user.clone());
            }
        });
    }
    card.append(&header);

    // Media
    card.append(&media_view(media, MEDIA_WIDTH));

    // Actions
    let actions = gtk::Box::builder()
        .spacing(2)
        .margin_start(6)
        .margin_end(6)
        .margin_top(4)
        .build();
    let liked = Rc::new(Cell::new(media.has_liked));
    let likes = Rc::new(Cell::new(media.like_count));
    let like_button = gtk::Button::builder()
        .css_classes(["flat", "circular", "like-button"])
        .build();
    let likes_label = gtk::Label::builder()
        .xalign(0.0)
        .margin_start(12)
        .use_markup(true)
        .visible(!media.like_and_view_counts_disabled)
        .build();
    let update_like = {
        let like_button = like_button.clone();
        let likes_label = likes_label.clone();
        let liked = liked.clone();
        let likes = likes.clone();
        Rc::new(move || {
            let on = liked.get();
            like_button.set_icon_name(if on { "heart-filled-symbolic" } else { "heart-outline-symbolic" });
            like_button.set_tooltip_text(Some(if on { "Unlike" } else { "Like" }));
            if on {
                like_button.add_css_class("liked");
            } else {
                like_button.remove_css_class("liked");
            }
            let n = likes.get();
            likes_label.set_markup(&format!("<b>{}</b>", util::plural(n, "like", "likes")));
        })
    };
    update_like();
    {
        let weak = Rc::downgrade(main);
        let media_id = media.id.clone();
        let update_like = update_like.clone();
        like_button.connect_clicked(move |button| {
            let Some(main) = weak.upgrade() else { return };
            let now = !liked.get();
            liked.set(now);
            likes.set((likes.get() + if now { 1 } else { -1 }).max(0));
            update_like();
            button.set_sensitive(false);
            let client = main.client.clone();
            let media_id = media_id.clone();
            let weak = weak.clone();
            let button = button.downgrade();
            let liked = liked.clone();
            let likes = likes.clone();
            let update_like = update_like.clone();
            glib::spawn_future_local(async move {
                let res = rt::bg(async move { client.like(&media_id, now).await }).await;
                if let Some(b) = button.upgrade() {
                    b.set_sensitive(true);
                }
                if let Err(e) = res {
                    liked.set(!now);
                    likes.set((likes.get() + if now { -1 } else { 1 }).max(0));
                    update_like();
                    if let Some(main) = weak.upgrade() {
                        main.handle_error(&e);
                    }
                }
            });
        });
    }
    actions.append(&like_button);
    if !media.comments_disabled {
        let comment_button = gtk::Button::builder()
            .icon_name("comment-symbolic")
            .tooltip_text("Comments")
            .css_classes(["flat", "circular"])
            .build();
        let weak = Rc::downgrade(main);
        let m = media.clone();
        comment_button.connect_clicked(move |_| {
            if let Some(main) = weak.upgrade() {
                main.show_comments(&m);
            }
        });
        actions.append(&comment_button);
    }
    card.append(&actions);
    card.append(&likes_label);

    // Caption
    let caption = media.caption_text();
    if !caption.trim().is_empty() {
        let label = gtk::Label::builder()
            .use_markup(true)
            .label(format!(
                "<b>{}</b> {}",
                glib::markup_escape_text(&user.username),
                util::linkify(&caption)
            ))
            .wrap(true)
            .wrap_mode(gtk::pango::WrapMode::WordChar)
            .xalign(0.0)
            .lines(3)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .margin_start(12)
            .margin_end(12)
            .margin_top(6)
            .build();
        card.append(&label);
        if caption.chars().count() > 140 || caption.lines().count() > 3 {
            let more = gtk::Button::builder()
                .label("more")
                .css_classes(["flat", "dim-label"])
                .halign(gtk::Align::Start)
                .margin_start(4)
                .build();
            more.connect_clicked(glib::clone!(
                #[weak]
                label,
                move |b| {
                    let expanded = label.lines() < 0;
                    label.set_lines(if expanded { 3 } else { -1 });
                    label.set_ellipsize(if expanded {
                        gtk::pango::EllipsizeMode::End
                    } else {
                        gtk::pango::EllipsizeMode::None
                    });
                    b.set_label(if expanded { "more" } else { "less" });
                }
            ));
            card.append(&more);
        }
    }

    if media.comment_count > 0 && !media.comments_disabled {
        let view = gtk::Button::builder()
            .label(if media.comment_count == 1 {
                "View 1 comment".to_string()
            } else {
                format!("View all {} comments", util::count(media.comment_count))
            })
            .css_classes(["flat", "dim-label"])
            .halign(gtk::Align::Start)
            .margin_start(4)
            .build();
        let weak = Rc::downgrade(main);
        let m = media.clone();
        view.connect_clicked(move |_| {
            if let Some(main) = weak.upgrade() {
                main.show_comments(&m);
            }
        });
        card.append(&view);
    }

    card.upcast()
}

/// The photo, video or carousel of a post, sized to its aspect ratio.
pub fn media_view(media: &Media, width: i32) -> gtk::Widget {
    let items = media.carousel_media.clone().unwrap_or_default();
    if items.is_empty() {
        return single_media(media, media.aspect(), width).upcast();
    }

    let aspect = media.aspect();
    let items = Rc::new(items);
    let index = Rc::new(Cell::new(0usize));
    let stack = gtk::Stack::builder()
        .transition_type(gtk::StackTransitionType::SlideLeftRight)
        .build();
    let overlay = gtk::Overlay::builder().child(&stack).build();
    let prev = gtk::Button::builder()
        .icon_name("go-previous-symbolic")
        .css_classes(["osd", "circular"])
        .halign(gtk::Align::Start)
        .valign(gtk::Align::Center)
        .margin_start(8)
        .tooltip_text("Previous")
        .build();
    let next = gtk::Button::builder()
        .icon_name("go-next-symbolic")
        .css_classes(["osd", "circular"])
        .halign(gtk::Align::End)
        .valign(gtk::Align::Center)
        .margin_end(8)
        .tooltip_text("Next")
        .build();
    let counter = gtk::Label::builder()
        .css_classes(["media-counter"])
        .halign(gtk::Align::End)
        .valign(gtk::Align::Start)
        .margin_top(10)
        .margin_end(10)
        .build();
    overlay.add_overlay(&prev);
    overlay.add_overlay(&next);
    overlay.add_overlay(&counter);

    // Slides are built lazily, the first time they're shown.
    let show = {
        let stack = stack.clone();
        let items = items.clone();
        let index = index.clone();
        let prev = prev.clone();
        let next = next.clone();
        let counter = counter.clone();
        Rc::new(move |i: usize| {
            let name = i.to_string();
            if stack.child_by_name(&name).is_none() {
                let w = single_media(&items[i], aspect, width);
                stack.add_named(&w, Some(&name));
            }
            stack.set_visible_child_name(&name);
            index.set(i);
            prev.set_visible(i > 0);
            next.set_visible(i + 1 < items.len());
            counter.set_label(&format!("{}/{}", i + 1, items.len()));
        })
    };
    show(0);
    {
        let show = show.clone();
        let index = index.clone();
        prev.connect_clicked(move |_| show(index.get().saturating_sub(1)));
    }
    {
        let len = items.len();
        next.connect_clicked(move |_| show((index.get() + 1).min(len - 1)));
    }

    RatioBin::new(aspect, width, &overlay).upcast()
}

fn single_media(media: &Media, aspect: f64, width: i32) -> RatioBin {
    let picture = gtk::Picture::builder()
        .content_fit(gtk::ContentFit::Cover)
        .can_shrink(true)
        .css_classes(["post-media"])
        .build();
    images::load_picture(&picture, media.image_url(750));

    let Some(video_url) = media.video_url().filter(|_| media.is_video()) else {
        return RatioBin::new(aspect, width, &picture);
    };

    let overlay = gtk::Overlay::builder().child(&picture).build();
    let play = gtk::Button::builder()
        .icon_name("media-playback-start-symbolic")
        .css_classes(["osd", "circular"])
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .tooltip_text("Play Video")
        .build();
    play.set_size_request(64, 64);
    overlay.add_overlay(&play);
    let bin = RatioBin::new(aspect, width, &overlay);
    let weak_bin = bin.downgrade();
    play.connect_clicked(move |_| {
        let Some(bin) = weak_bin.upgrade() else { return };
        let stream = gtk::MediaFile::for_file(&gio::File::for_uri(&video_url));
        let video = gtk::Video::builder().autoplay(true).css_classes(["post-media"]).build();
        video.set_media_stream(Some(&stream));
        bin.set_child(&video);
    });
    bin
}
