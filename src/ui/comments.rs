//! A post's comments (the first page) with a field to add one: in a dialog,
//! or as a panel beside or below a post.

use std::cell::Cell;
use std::rc::{Rc, Weak};

use adw::prelude::*;
use gtk::glib;

use crate::api::{Comment, Media};
use crate::rt;
use crate::ui::util;
use crate::ui::window::Main;

/// A comment; its avatar and username open the commenter's profile.
fn comment_row(main: &Weak<Main>, c: &Comment, avatar_size: i32) -> gtk::Widget {
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
    col.append(&meta);
    row.append(&col);
    row.upcast()
}

/// A comment and, below it, its replies: those that came with it, and a
/// button that loads all of them, a page at a time.
fn comment_thread(main: &Weak<Main>, media_id: &str, c: &Comment) -> gtk::Widget {
    let column = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(10).build();
    column.append(&comment_row(main, c, 32));
    let shown = c.preview_child_comments.len() as i64;
    if c.child_comment_count.max(shown) == 0 {
        return column.upcast();
    }
    // Lined up with the comment's text.
    let replies = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(10)
        .margin_start(42)
        .build();
    for r in &c.preview_child_comments {
        replies.append(&comment_row(main, r, 24));
    }
    column.append(&replies);
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
    let (main, media_id, pk) = (main.clone(), media_id.to_string(), c.pk.clone());
    let list = replies.downgrade();
    more.connect_clicked(move |button| {
        let Some(client) = main.upgrade().map(|m| m.client.clone()) else { return };
        button.set_sensitive(false);
        let (media_id, pk, after) = (media_id.clone(), pk.clone(), cursor.borrow().clone());
        let (main, cursor, list, button) = (main.clone(), cursor.clone(), list.clone(), button.downgrade());
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
                        list.append(&comment_row(&main, r, 24));
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
    });
    let weak = Rc::downgrade(&panel);
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
                    for c in &comments {
                        p.list.append(&comment_thread(&p.main, &p.media_id, c));
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
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let res = rt::bg(async move { client.add_comment(&media_id, &text).await }).await;
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
                    p.list.append(&comment_row(&p.main, &comment, 32));
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
