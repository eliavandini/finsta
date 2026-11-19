//! Posts: a card for the home feed (stacked, or with comments beside it) and
//! a post page that puts the details and comments to the right of the photo
//! or video when the window is wide.

use std::cell::Cell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

use crate::api::{Media, Song};
use crate::rt;
use crate::ui::layout::SIDE_WIDTH;
use crate::ui::menu::{self, Menu};
use crate::ui::ratio_bin::RatioBin;
use crate::ui::util;
use crate::ui::window::Main;
use crate::ui::{comments, download, images, saving, share, video};

/// How a feed card is drawn.
#[derive(Debug, Clone, Copy)]
pub struct Style {
    /// The width of the photo or video.
    pub width: i32,
    /// Comments in a panel to the right of the post.
    pub side: bool,
}

impl Default for Style {
    fn default() -> Self {
        Style { width: 600, side: false }
    }
}

/// The pieces of a post, placed differently by each layout.
struct Parts {
    header: gtk::Widget,
    /// The photo, video or carousel, without a size of its own.
    media: gtk::Widget,
    aspect: f64,
    /// The buttons and the like count.
    actions: gtk::Widget,
    buttons: Buttons,
    caption: Option<gtk::Widget>,
}

/// A post's buttons, which its menu presses too (so both stay in step).
#[derive(Clone)]
pub struct Buttons {
    like: gtk::Button,
    save: gtk::Button,
}

type Action = Rc<dyn Fn()>;

fn parts(main: &Rc<Main>, media: &Media, on_comment: Action) -> Parts {
    let (actions, buttons) = actions(main, media, on_comment);
    Parts {
        header: header(main, media),
        media: with_song(main, media, media_content(media)),
        aspect: media.aspect(),
        actions,
        buttons,
        caption: caption(media),
    }
}

/// The right-click menu of a post. `buttons`: the post's own buttons, when
/// it's shown with them; `open`: offer to open it on its own page.
pub fn menu(main: &Rc<Main>, media: &Media, buttons: Option<&Buttons>, on_comment: Option<Action>, open: bool) -> Menu {
    let mut m = Menu::new();
    let what = if media.product_type == "clips" { share::Kind::Reel } else { share::Kind::Post };
    if open {
        let (weak, media) = (Rc::downgrade(main), media.clone());
        m.item(if what == share::Kind::Reel { "Open Reel" } else { "Open Post" }, move || {
            if let Some(main) = weak.upgrade() {
                main.open_post(media.clone());
            }
        });
    }
    if let Some(b) = buttons {
        let like = b.like.clone();
        let label = like.tooltip_text().map(|t| t.to_string()).unwrap_or_else(|| "Like".into());
        m.item(label, move || like.emit_clicked());
    }
    if let Some(comment) = on_comment.filter(|_| !media.comments_disabled) {
        m.item("Comment", move || comment());
    }
    m.section();
    if let Some(url) = media.web_url() {
        let weak = Rc::downgrade(main);
        m.item("Share…", move || {
            if let Some(main) = weak.upgrade() {
                share::open(&main, share::Shared { url: url.clone(), kind: what }, None, None);
            }
        });
    }
    match buttons {
        Some(b) => {
            let save = b.save.clone();
            let label = save.tooltip_text().map(|t| t.to_string()).unwrap_or_else(|| "Save".into());
            m.item(label, move || save.emit_clicked());
        }
        None => {
            let (weak, media, saved) = (Rc::downgrade(main), media.clone(), media.has_viewer_saved);
            m.item(if saved { "Remove from Saved" } else { "Save" }, move || {
                if let Some(main) = weak.upgrade() {
                    saving::set_saved(&main, &media, !saved, |_| {});
                }
            });
        }
    }
    let (weak, m2) = (Rc::downgrade(main), media.clone());
    m.item("Add to Folder…", move || {
        if let Some(main) = weak.upgrade() {
            saving::choose_folder(&main, &m2);
        }
    });
    let (weak, m2) = (Rc::downgrade(main), media.clone());
    m.item("Download", move || {
        if let Some(main) = weak.upgrade() {
            let owner = m2.user.as_ref().map(|u| u.username.clone()).unwrap_or_default();
            download::save(&main, &m2, &owner, None);
        }
    });
    m.section();
    if let Some(url) = media.web_url() {
        menu::add_link(&mut m, main, &url);
    }
    m.section();
    if let Some(user) = &media.user {
        menu::add_person(&mut m, main, user, true, true);
    }
    m
}

/// A post's menu on a thumbnail (profile, saved and search grids).
pub fn attach_menu(main: &Rc<Main>, widget: &impl IsA<gtk::Widget>, media: &Media) {
    let weak = Rc::downgrade(main);
    let media = media.clone();
    menu::attach(widget, move || Some(menu(&weak.upgrade()?, &media, None, None, true)));
}

fn header(main: &Rc<Main>, media: &Media) -> gtk::Widget {
    let user = media.user.clone().unwrap_or_default();
    let header = gtk::Box::builder().spacing(10).css_classes(["post-header"]).build();
    let avatar_button = gtk::Button::builder()
        .child(&util::avatar(&user, 36))
        .css_classes(["flat", "circular"])
        .tooltip_text(format!("View @{}'s profile", user.username))
        .valign(gtk::Align::Center)
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
        names.append(
            &gtk::Label::builder()
                .label(&loc.name)
                .xalign(0.0)
                .margin_start(6)
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .css_classes(["caption", "dim-label"])
                .build(),
        );
    }
    if let Some(song) = media.song() {
        names.append(&music_label(main, &song, media.web_url(), None));
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
            .valign(gtk::Align::Center)
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
    header.upcast()
}

/// "♫ Song · Artist" and a button to save the song, for posts and stories
/// with music. `from` is the post or story it's on.
pub fn music_label(main: &Rc<Main>, song: &Song, from: Option<String>, notify: Option<share::Notify>) -> gtk::Widget {
    let music = song.label();
    let b = gtk::Box::builder().spacing(4).margin_start(6).css_classes(["music"]).build();
    b.append(&gtk::Image::builder().icon_name("audio-x-generic-symbolic").pixel_size(12).build());
    let label = gtk::Label::builder()
        .label(&music)
        .xalign(0.0)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .tooltip_text(&music)
        .css_classes(["caption"])
        .build();
    b.append(&label);
    b.append(&saving::song_button(main, song, from, notify));
    b.upcast()
}

fn actions(main: &Rc<Main>, media: &Media, on_comment: Action) -> (gtk::Widget, Buttons) {
    let column = gtk::Box::builder().orientation(gtk::Orientation::Vertical).build();
    let row = gtk::Box::builder()
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
            likes_label.set_markup(&format!("<b>{}</b>", util::plural(likes.get(), "like", "likes")));
        })
    };
    update_like();
    {
        let weak = Rc::downgrade(main);
        let media_id = media.id.clone();
        let token = media.organic_tracking_token.clone();
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
            let token = token.clone();
            let weak = weak.clone();
            let button = button.downgrade();
            let liked = liked.clone();
            let likes = likes.clone();
            let update_like = update_like.clone();
            glib::spawn_future_local(async move {
                let res = rt::bg(async move { client.like(&media_id, token.as_deref(), now).await }).await;
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
    row.append(&like_button);
    if !media.comments_disabled {
        let comment_button = gtk::Button::builder()
            .icon_name("comment-symbolic")
            .tooltip_text("Comments")
            .css_classes(["flat", "circular"])
            .build();
        comment_button.connect_clicked(move |_| on_comment());
        row.append(&comment_button);
    }
    if let Some(url) = media.web_url() {
        let what = if media.product_type == "clips" { share::Kind::Reel } else { share::Kind::Post };
        row.append(&share::button(main, share::Shared { url, kind: what }));
    }
    row.append(&gtk::Box::builder().hexpand(true).build());
    let owner = media.user.as_ref().map(|u| u.username.clone()).unwrap_or_default();
    row.append(&download::button(main, media, &owner));
    let save_button = saving::post_button(main, media);
    row.append(&save_button);
    column.append(&row);
    column.append(&likes_label);
    (column.upcast(), Buttons { like: like_button, save: save_button })
}

fn caption(media: &Media) -> Option<gtk::Widget> {
    let caption = media.caption_text();
    if caption.trim().is_empty() {
        return None;
    }
    let user = media.user.clone().unwrap_or_default();
    let b = gtk::Box::builder().orientation(gtk::Orientation::Vertical).build();
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
        .selectable(true)
        .margin_start(12)
        .margin_end(12)
        .margin_top(6)
        .build();
    b.append(&label);
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
        b.append(&more);
    }
    Some(b.upcast())
}

fn open_comments(main: &Rc<Main>, media: &Media) -> Action {
    let weak = Rc::downgrade(main);
    let media = media.clone();
    Rc::new(move || {
        if let Some(main) = weak.upgrade() {
            main.show_comments(&media);
        }
    })
}

fn focus_entry(panel: &Rc<comments::Panel>) -> Action {
    let entry = panel.entry.downgrade();
    Rc::new(move || {
        if let Some(e) = entry.upgrade() {
            e.grab_focus();
        }
    })
}

fn attach_card_menu(main: &Rc<Main>, card: &impl IsA<gtk::Widget>, media: &Media, buttons: &Buttons, on_comment: Action, open: bool) {
    let weak = Rc::downgrade(main);
    let (media, buttons) = (media.clone(), buttons.clone());
    menu::attach(card, move || Some(menu(&weak.upgrade()?, &media, Some(&buttons), Some(on_comment.clone()), open)));
}

/// A post for the home feed.
pub fn card(main: &Rc<Main>, media: &Media, style: Style) -> gtk::Widget {
    if style.side {
        return side_card(main, media, style.width);
    }
    let card = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .css_classes(["card", "post"])
        .overflow(gtk::Overflow::Hidden)
        .build();
    let p = parts(main, media, open_comments(main, media));
    attach_card_menu(main, &card, media, &p.buttons, open_comments(main, media), true);
    card.append(&p.header);
    card.append(&RatioBin::new(p.aspect, style.width, &p.media));
    card.append(&p.actions);
    if let Some(c) = &p.caption {
        card.append(c);
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
        let open = open_comments(main, media);
        view.connect_clicked(move |_| open());
        card.append(&view);
    } else {
        card.append(&gtk::Box::builder().height_request(6).build());
    }
    card.upcast()
}

/// A feed card with the details and comments in a panel to the right.
fn side_card(main: &Rc<Main>, media: &Media, width: i32) -> gtk::Widget {
    let panel = comments::panel(main, media);
    let p = parts(main, media, focus_entry(&panel));
    // The card is as tall as the photo or video (or the panel's minimum):
    // the media column sets the size, keeping room on its right where the
    // panel is laid over it.
    let card = gtk::Overlay::builder()
        .css_classes(["card", "post", "side-post"])
        .overflow(gtk::Overflow::Hidden)
        .build();
    attach_card_menu(main, &card, media, &p.buttons, focus_entry(&panel), true);
    let ratio = RatioBin::new(p.aspect, width, &p.media);
    ratio.set_valign(gtk::Align::Center);
    ratio.set_vexpand(true);
    let pane = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .margin_end(SIDE_WIDTH)
        .height_request(290)
        .css_classes(["post-media-pane"])
        .build();
    pane.append(&ratio);
    card.set_child(Some(&pane));

    let side = gtk::Box::builder().orientation(gtk::Orientation::Vertical).build();
    side.append(&p.header);
    side.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    let body = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(10)
        .margin_bottom(10)
        .build();
    if let Some(c) = &p.caption {
        body.append(c);
    }
    panel.list.set_margin_start(12);
    panel.list.set_margin_end(12);
    panel.list.set_margin_top(6);
    body.append(&panel.list);
    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .min_content_height(100)
        .child(&body)
        .build();
    side.append(&scroller);
    side.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    side.append(&p.actions);
    panel.composer.add_css_class("side-composer");
    side.append(&panel.composer);
    let clamp = adw::Clamp::builder()
        .maximum_size(SIDE_WIDTH)
        .tightening_threshold(SIDE_WIDTH)
        .width_request(SIDE_WIDTH)
        .child(&side)
        .build();
    clamp.set_halign(gtk::Align::End);
    card.add_overlay(&clamp);

    // Comments load once the post scrolls into view.
    let weak = Rc::downgrade(&panel);
    util::when_visible(&card, move || {
        if let Some(p) = weak.upgrade() {
            p.load();
        }
    });
    card.upcast()
}

/// Remove `w` from the box or frame it's in.
fn detach(w: &gtk::Widget) {
    let Some(parent) = w.parent() else { return };
    if let Some(b) = parent.downcast_ref::<gtk::Box>() {
        b.remove(w);
    } else if let Some(f) = parent.downcast_ref::<gtk::AspectFrame>() {
        f.set_child(None::<&gtk::Widget>);
    }
}

/// A single post with its comments: stacked when the window is narrow, the
/// details and comments to the right of the media when it's wide.
pub fn page(main: &Rc<Main>, media: &Media) -> adw::NavigationPage {
    let panel = comments::panel(main, media);
    let p = parts(main, media, focus_entry(&panel));
    attach_card_menu(main, &p.media, media, &p.buttons, focus_entry(&panel), false);
    let comments_title = gtk::Label::builder()
        .label("Comments")
        .xalign(0.0)
        .margin_start(12)
        .margin_top(12)
        .css_classes(["heading"])
        .visible(!media.comments_disabled)
        .build();

    // Narrow: one scrolling column.
    let holder = gtk::Box::builder().orientation(gtk::Orientation::Vertical).build();
    let ratio = RatioBin::new(p.aspect, 600, &holder);
    let narrow_card = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(0)
        .css_classes(["card", "post"])
        .overflow(gtk::Overflow::Hidden)
        .build();
    let narrow_content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .margin_top(12)
        .margin_bottom(24)
        .margin_start(12)
        .margin_end(12)
        .build();
    narrow_content.append(&narrow_card);
    let narrow = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&adw::Clamp::builder().maximum_size(620).child(&narrow_content).build())
        .build();

    // Wide: the media on the left, everything else on the right.
    let frame = gtk::AspectFrame::new(0.5, 0.5, (1.0 / p.aspect) as f32, false);
    let pane = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .hexpand(true)
        .vexpand(true)
        .css_classes(["post-media-pane"])
        .build();
    frame.set_vexpand(true);
    pane.append(&frame);
    let side_top = gtk::Box::builder().orientation(gtk::Orientation::Vertical).build();
    let side_body = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(10)
        .margin_bottom(12)
        .build();
    let side_bottom = gtk::Box::builder().orientation(gtk::Orientation::Vertical).build();
    let side = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .width_request(400)
        .css_classes(["post-side"])
        .build();
    side.append(&side_top);
    side.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    side.append(
        &gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&side_body)
            .build(),
    );
    side.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    side.append(&side_bottom);
    let wide = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    wide.append(&pane);
    wide.append(&gtk::Separator::new(gtk::Orientation::Vertical));
    wide.append(&side);

    panel.list.set_margin_start(12);
    panel.list.set_margin_end(12);
    panel.list.set_margin_bottom(12);
    let bin = adw::BreakpointBin::builder().width_request(360).height_request(300).build();
    let arrange = {
        let bin = bin.downgrade();
        let pieces: Vec<gtk::Widget> = [
            Some(p.header.clone()),
            Some(p.media.clone()),
            Some(ratio.clone().upcast()),
            Some(p.actions.clone()),
            p.caption.clone(),
            Some(comments_title.clone().upcast()),
            Some(panel.list.clone().upcast()),
            Some(panel.composer.clone().upcast()),
        ]
        .into_iter()
        .flatten()
        .collect();
        let caption = p.caption.clone();
        let (header, media_w, actions) = (p.header.clone(), p.media.clone(), p.actions.clone());
        let list: gtk::Widget = panel.list.clone().upcast();
        let composer: gtk::Widget = panel.composer.clone().upcast();
        let title: gtk::Widget = comments_title.clone().upcast();
        let ratio: gtk::Widget = ratio.clone().upcast();
        Rc::new(move |is_wide: bool| {
            let Some(bin) = bin.upgrade() else { return };
            for w in &pieces {
                detach(w);
            }
            if is_wide {
                frame.set_child(Some(&media_w));
                side_top.append(&header);
                if let Some(c) = &caption {
                    side_body.append(c);
                }
                side_body.append(&title);
                side_body.append(&list);
                side_bottom.append(&actions);
                side_bottom.append(&composer);
                if bin.child().as_ref() != Some(wide.upcast_ref()) {
                    bin.set_child(Some(&wide));
                }
            } else {
                holder.append(&media_w);
                narrow_card.append(&header);
                narrow_card.append(&ratio);
                narrow_card.append(&actions);
                if let Some(c) = &caption {
                    narrow_card.append(c);
                }
                narrow_card.append(&title);
                narrow_card.append(&list);
                narrow_card.append(&composer);
                if bin.child().as_ref() != Some(narrow.upcast_ref()) {
                    bin.set_child(Some(&narrow));
                }
            }
        })
    };
    arrange(false);
    let breakpoint = adw::Breakpoint::new(adw::BreakpointCondition::new_length(
        adw::BreakpointConditionLengthType::MinWidth,
        760.0,
        adw::LengthUnit::Sp,
    ));
    let a = arrange.clone();
    breakpoint.connect_apply(move |_| a(true));
    breakpoint.connect_unapply(move |_| arrange(false));
    bin.add_breakpoint(breakpoint);
    panel.load();

    let owner = media.user.as_ref().map(|u| u.username.clone()).unwrap_or_default();
    adw::NavigationPage::builder()
        .title(format!("Post by @{owner}"))
        .child(&bin)
        .build()
}

/// A photo post with music plays its song (while the sound is on and it's
/// on screen), with the sound button videos have.
fn with_song(main: &Rc<Main>, media: &Media, content: gtk::Widget) -> gtk::Widget {
    if media.is_video() || media.song().is_none() {
        return content;
    }
    let overlay = gtk::Overlay::builder().child(&content).build();
    let mute = video::mute_button();
    mute.set_valign(gtk::Align::End);
    mute.set_halign(gtk::Align::Start);
    mute.set_margin_start(10);
    overlay.add_overlay(&mute);
    let weak = Rc::downgrade(main);
    let media = media.clone();
    video::autoplay_song(&overlay, move |ready| {
        let Some(main) = weak.upgrade() else { return ready(None) };
        let client = main.client.clone();
        let media = media.clone();
        glib::spawn_future_local(async move {
            let res = rt::bg(async move { client.song_clip(&media).await }).await;
            ready(match res {
                Ok(Some(clip)) => Some((clip.url, segment(clip.start_ms, clip.duration_ms))),
                Ok(None) => None,
                Err(e) => {
                    eprintln!("finsta: music: couldn't load a post's song: {e}");
                    None
                }
            });
        });
    });
    overlay.upcast()
}

/// The part of a song a post or story plays.
pub fn segment(start_ms: u64, duration_ms: Option<u64>) -> video::Segment {
    video::Segment {
        start: std::time::Duration::from_millis(start_ms),
        length: duration_ms.map(std::time::Duration::from_millis),
    }
}

/// The photo, video or carousel of a post, filling whatever size it's given.
fn media_content(media: &Media) -> gtk::Widget {
    let items = media.carousel_media.clone().unwrap_or_default();
    if items.is_empty() {
        return single_media(media);
    }

    let items = Rc::new(items);
    let index = Rc::new(Cell::new(0usize));
    let stack = gtk::Stack::builder()
        .transition_type(gtk::StackTransitionType::SlideLeftRight)
        .hhomogeneous(true)
        .vhomogeneous(true)
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
                stack.add_named(&single_media(&items[i]), Some(&name));
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
    overlay.upcast()
}

fn single_media(media: &Media) -> gtk::Widget {
    let picture = gtk::Picture::builder()
        .content_fit(gtk::ContentFit::Contain)
        .can_shrink(true)
        .hexpand(true)
        .vexpand(true)
        .css_classes(["post-media"])
        .build();
    images::load_picture(&picture, media.image_url(750));

    let Some(video_url) = media.video_url().filter(|_| media.is_video()) else {
        return picture.upcast();
    };

    // Plays (muted, looping) while it's the most visible video on screen.
    let overlay = gtk::Overlay::builder().child(&picture).build();
    overlay.add_overlay(&video::mute_button());
    video::autoplay(&picture, video_url);
    video::controls(&overlay, &picture);
    overlay.upcast()
}
