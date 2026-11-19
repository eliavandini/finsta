//! A post's comments (the first page) with a field to add one: in a dialog,
//! or as a panel beside or below a post.

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

use adw::prelude::*;
use gtk::glib;

use crate::api::{Comment, Media};
use crate::rt;
use crate::ui::util;
use crate::ui::window::Main;

/// Start a reply in a thread: its top comment's pk, who's answered, and the
/// box its replies go in.
type ReplyTo = Rc<dyn Fn(String, String, gtk::Box)>;

/// A comment; its avatar and username open the commenter's profile, and
/// `reply` (if any) adds a Reply button.
fn comment_row(main: &Weak<Main>, c: &Comment, avatar_size: i32, reply: Option<Rc<dyn Fn()>>) -> gtk::Widget {
    let row = gtk::Box::builder().spacing(10).build();
    let open = {
        let (main, user) = (main.clone(), c.user.clone());
        move || {
            let Some(main) = main.upgrade() else { return };
            // The comments dialog would cover the profile.
            if let Some(dialog) = main.window.visible_dialog() {
                dialog.force_close();
            }
            main.open_profile(user.clone());
        }
    };
    let avatar = gtk::Button::builder()
        .child(&util::avatar(&c.user, avatar_size))
        .css_classes(["flat", "circular"])
        .tooltip_text(format!("@{}", c.user.username))
        .valign(gtk::Align::Start)
        .build();
    let o = open.clone();
    avatar.connect_clicked(move |_| o());
    row.append(&avatar);
    let col = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(2)
        .hexpand(true)
        .build();
    // The name is a link but looks like plain bold text.
    // shortcut: its color is fixed when the row is made, so a theme switch shows it until the comments reload.
    let text = gtk::Label::builder()
        .use_markup(true)
        .label(format!(
            "<a href=\"finsta:profile\"><span underline=\"none\" foreground=\"{}\"><b>{}</b></span></a> {}",
            if adw::StyleManager::default().is_dark() { "#ffffff" } else { "#000000" },
            glib::markup_escape_text(&c.user.username),
            util::linkify(&c.text)
        ))
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::WordChar)
        .xalign(0.0)
        .selectable(true)
        .build();
    text.connect_activate_link(move |_, uri| {
        if uri != "finsta:profile" {
            return glib::Propagation::Proceed;
        }
        open();
        glib::Propagation::Stop
    });
    let mut meta = util::relative_time(c.created_at);
    if c.comment_like_count > 0 {
        meta.push_str(&format!(" · {}", util::plural(c.comment_like_count, "like", "likes")));
    }
    let meta = gtk::Label::builder()
        .label(meta)
        .xalign(0.0)
        .css_classes(["caption", "dim-label"])
        .build();
    col.append(&text);
    match c.media() {
        Some(m) => {
            let size = 160;
            let thumb = if m.animated {
                crate::ui::chat::animated_thumb(Some(m.url), m.aspect, size)
            } else {
                crate::ui::chat::media_thumb(Some(m.url), m.aspect, (size as f64 / m.aspect.max(1.0)) as i32, false)
            };
            thumb.set_halign(gtk::Align::Start);
            thumb.set_margin_top(4);
            col.append(&thumb);
        }
        None if c.media_placeholder().is_some() => {
            let note = gtk::Label::builder()
                .label(c.media_placeholder().unwrap_or_default())
                .xalign(0.0)
                .wrap(true)
                .css_classes(["dim-label"])
                .build();
            note.add_css_class("caption");
            col.append(&note);
        }
        // Field names only, to learn the shape of what isn't shown.
        None if c.text.trim().is_empty() => {
            eprintln!("finsta: a comment with nothing to show; fields {:?}", c.extra.keys().collect::<Vec<_>>());
            if let Some(shape) = c.media_shape() {
                eprintln!("finsta: its media_comment_info: {shape}");
            }
        }
        None => {}
    }
    match reply {
        Some(reply) => {
            let line = gtk::Box::builder().spacing(4).build();
            line.append(&meta);
            let button = gtk::Button::builder()
                .label("Reply")
                .css_classes(["flat", "caption", "dim-label", "comment-reply"])
                .build();
            button.connect_clicked(move |_| reply());
            line.append(&button);
            col.append(&line);
        }
        None => col.append(&meta),
    }
    row.append(&col);
    row.upcast()
}

/// The Reply action for a row in the thread of `top` (`pk`), whose replies go in `replies`.
fn reply_action(reply: &ReplyTo, top_pk: &str, c: &Comment, replies: &gtk::Box) -> Option<Rc<dyn Fn()>> {
    if top_pk.is_empty() {
        return None;
    }
    let (reply, pk, user, replies) = (reply.clone(), top_pk.to_string(), c.user.username.clone(), replies.clone());
    Some(Rc::new(move || reply(pk.clone(), user.clone(), replies.clone())))
}

/// A comment and, below it, its replies: those that came with it, and a
/// button that loads all of them, a page at a time.
fn comment_thread(main: &Weak<Main>, media_id: &str, c: &Comment, reply: &ReplyTo) -> gtk::Widget {
    let column = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(10).build();
    // Lined up with the comment's text; hidden until it has replies.
    let replies = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(10)
        .margin_start(42)
        .build();
    column.append(&comment_row(main, c, 32, reply_action(reply, &c.pk, c, &replies)));
    column.append(&replies);
    let shown = c.preview_child_comments.len() as i64;
    replies.set_visible(c.child_comment_count.max(shown) > 0);
    for r in &c.preview_child_comments {
        replies.append(&comment_row(main, r, 24, reply_action(reply, &c.pk, r, &replies)));
    }
    let hidden = c.child_comment_count - shown;
    if hidden <= 0 || c.pk.is_empty() {
        return column.upcast();
    }
    let label = match (shown, hidden) {
        (0, 1) => "View 1 reply".to_string(),
        (0, n) => format!("View {n} replies"),
        _ => format!("View all {} replies", c.child_comment_count),
    };
    let more = gtk::Button::builder()
        .label(label)
        .halign(gtk::Align::Start)
        .css_classes(["flat", "caption"])
        .build();
    replies.append(&more);
    // None: nothing loaded yet (the first page replaces the replies shown).
    let cursor: Rc<std::cell::RefCell<Option<String>>> = Rc::default();
    let (main, media_id, pk, reply) = (main.clone(), media_id.to_string(), c.pk.clone(), reply.clone());
    let list = replies.downgrade();
    more.connect_clicked(move |button| {
        let Some(client) = main.upgrade().map(|m| m.client.clone()) else { return };
        button.set_sensitive(false);
        let (media_id, pk, after) = (media_id.clone(), pk.clone(), cursor.borrow().clone());
        let (main, cursor, list, button, reply, top) = (main.clone(), cursor.clone(), list.clone(), button.downgrade(), reply.clone(), pk.clone());
        glib::spawn_future_local(async move {
            let first = after.is_none();
            let res = rt::bg(async move { client.comment_replies(&media_id, &pk, after.as_deref()).await }).await;
            let (Some(list), Some(button)) = (list.upgrade(), button.upgrade()) else { return };
            button.set_sensitive(true);
            match res {
                Ok((page, _)) if page.is_empty() => button.set_label("Couldn't load the replies. Try again"),
                Ok((page, next)) => {
                    // The button goes back after them, if there's more.
                    if first {
                        util::clear_box(&list);
                    } else {
                        list.remove(&button);
                    }
                    for r in &page {
                        list.append(&comment_row(&main, r, 24, reply_action(&reply, &top, r, &list)));
                    }
                    if let Some(next) = next {
                        cursor.replace(Some(next));
                        button.set_label("View more replies");
                        list.append(&button);
                    }
                }
                Err(e) => {
                    button.set_label("Couldn't load the replies. Try again");
                    if let Some(main) = main.upgrade()
                        && crate::api::is_login_required(&e)
                    {
                        main.handle_error(&e);
                    }
                }
            }
        });
    });
    column.upcast()
}

fn note(text: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(text)
        .wrap(true)
        .justify(gtk::Justification::Center)
        .css_classes(["dim-label"])
        .margin_top(12)
        .margin_bottom(12)
        .build()
}

/// The comment list and the field to add one, kept as two widgets so they
/// can be placed apart (the list scrolls; the field stays at the bottom).
pub struct Panel {
    main: Weak<Main>,
    media_id: String,
    comments_disabled: bool,
    pub list: gtk::Box,
    pub composer: gtk::Box,
    pub entry: gtk::Entry,
    post: gtk::Button,
    loaded: Cell<bool>,
    /// Replying in a thread: its top comment's pk and its replies box.
    replying: RefCell<Option<(String, glib::WeakRef<gtk::Box>)>>,
    /// Stops replying.
    cancel: gtk::Button,
}

pub fn panel(main: &Rc<Main>, media: &Media) -> Rc<Panel> {
    let list = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(14)
        .css_classes(["comments-list"])
        .build();
    let entry = gtk::Entry::builder()
        .placeholder_text("Add a comment…")
        .hexpand(true)
        .build();
    let post = gtk::Button::builder()
        .label("Post")
        .css_classes(["suggested-action"])
        .build();
    let composer = gtk::Box::builder().spacing(8).css_classes(["composer"]).build();
    let cancel = gtk::Button::builder()
        .icon_name("window-close-symbolic")
        .tooltip_text("Cancel Reply")
        .css_classes(["flat", "circular"])
        .valign(gtk::Align::Center)
        .visible(false)
        .build();
    composer.append(&cancel);
    composer.append(&entry);
    composer.append(&post);
    composer.set_visible(!media.comments_disabled);

    let panel = Rc::new(Panel {
        main: Rc::downgrade(main),
        media_id: media.id.clone(),
        comments_disabled: media.comments_disabled,
        list,
        composer,
        entry,
        post,
        loaded: Cell::new(false),
        replying: RefCell::new(None),
        cancel,
    });
    let weak = Rc::downgrade(&panel);
    let w = weak.clone();
    panel.cancel.connect_clicked(move |_| {
        if let Some(p) = w.upgrade() {
            p.stop_reply();
            p.entry.set_text("");
        }
    });
    panel.entry.connect_activate(glib::clone!(
        #[strong]
        weak,
        move |_| {
            if let Some(p) = weak.upgrade() {
                p.send();
            }
        }
    ));
    panel.post.connect_clicked(move |_| {
        if let Some(p) = weak.upgrade() {
            p.send();
        }
    });
    // The panel lives as long as its list.
    util::attach(&panel.list, panel.clone());
    panel
}

impl Panel {
    fn reply_to(self: &Rc<Self>) -> ReplyTo {
        let weak = Rc::downgrade(self);
        Rc::new(move |pk, user, replies: gtk::Box| {
            let Some(p) = weak.upgrade() else { return };
            p.replying.replace(Some((pk, replies.downgrade())));
            p.entry.set_placeholder_text(Some(&format!("Reply to @{user}…")));
            p.entry.set_text(&format!("@{user} "));
            p.cancel.set_visible(true);
            p.entry.grab_focus();
            p.entry.set_position(-1);
        })
    }

    fn stop_reply(&self) {
        self.replying.replace(None);
        self.entry.set_placeholder_text(Some("Add a comment…"));
        self.cancel.set_visible(false);
    }

    /// Load the comments (once).
    pub fn load(self: &Rc<Self>) {
        if self.loaded.replace(true) {
            return;
        }
        if self.comments_disabled {
            self.list.append(&note("Comments are turned off for this post."));
            return;
        }
        let Some(main) = self.main.upgrade() else { return };
        self.list.append(&util::spinner_box());
        let client = main.client.clone();
        let media_id = self.media_id.clone();
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let res = rt::bg(async move { client.comments(&media_id).await }).await;
            let Some(p) = weak.upgrade() else { return };
            util::clear_box(&p.list);
            match res {
                Ok(comments) if comments.is_empty() => p.list.append(&note("No comments yet")),
                Ok(comments) => {
                    let reply = p.reply_to();
                    for c in &comments {
                        p.list.append(&comment_thread(&p.main, &p.media_id, c, &reply));
                    }
                }
                Err(e) => {
                    p.list.append(&note(&format!("Couldn't load the comments: {e}")));
                    if let Some(main) = p.main.upgrade()
                        && crate::api::is_login_required(&e)
                    {
                        main.handle_error(&e);
                    }
                }
            }
        });
    }

    fn send(self: &Rc<Self>) {
        let text = self.entry.text().trim().to_string();
        if text.is_empty() || !self.post.is_sensitive() {
            return;
        }
        let Some(main) = self.main.upgrade() else { return };
        self.post.set_sensitive(false);
        let client = main.client.clone();
        let media_id = self.media_id.clone();
        let replying = self.replying.borrow().clone();
        let reply_pk = replying.as_ref().map(|r| r.0.clone());
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let res = rt::bg(async move { client.add_comment(&media_id, &text, reply_pk.as_deref()).await }).await;
            let Some(p) = weak.upgrade() else { return };
            p.post.set_sensitive(true);
            match res {
                Ok(comment) => {
                    p.entry.set_text("");
                    // Drop a "No comments yet" note.
                    if let Some(first) = p.list.first_child()
                        && first.is::<gtk::Label>()
                    {
                        p.list.remove(&first);
                    }
                    let reply = p.reply_to();
                    match replying.and_then(|(pk, b)| Some((pk, b.upgrade()?))) {
                        Some((pk, replies)) => {
                            let row = comment_row(&p.main, &comment, 24, reply_action(&reply, &pk, &comment, &replies));
                            // Above a "View replies" button, if there is one.
                            match replies.last_child().filter(|w| w.is::<gtk::Button>()) {
                                Some(more) => replies.insert_child_after(&row, more.prev_sibling().as_ref()),
                                None => replies.append(&row),
                            }
                            replies.set_visible(true);
                        }
                        None => p.list.append(&comment_thread(&p.main, &p.media_id, &comment, &reply)),
                    }
                    p.stop_reply();
                }
                Err(e) => {
                    if let Some(main) = p.main.upgrade() {
                        main.handle_error(&e);
                    }
                }
            }
        });
    }
}

pub fn open(main: &Rc<Main>, media: &Media) {
    let panel = panel(main, media);
    panel.list.set_margin_top(12);
    panel.list.set_margin_bottom(12);
    panel.list.set_margin_start(12);
    panel.list.set_margin_end(12);
    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&panel.list)
        .build();

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&scroller));
    toolbar.add_bottom_bar(&panel.composer);

    let dialog = adw::Dialog::builder()
        .title("Comments")
        .content_width(480)
        .content_height(640)
        .child(&toolbar)
        .build();
    panel.load();
    dialog.present(Some(&main.window));
    panel.entry.grab_focus();
}
