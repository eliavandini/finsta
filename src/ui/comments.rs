//! A dialog with the first page of a post's comments and a field to add one.

use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

use crate::api::{Comment, Media};
use crate::rt;
use crate::ui::util;
use crate::ui::window::Main;

fn comment_row(c: &Comment) -> gtk::Widget {
    let row = gtk::Box::builder().spacing(10).build();
    let avatar = util::avatar(&c.user, 32);
    avatar.set_valign(gtk::Align::Start);
    row.append(&avatar);
    let col = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(2)
        .hexpand(true)
        .build();
    let text = gtk::Label::builder()
        .use_markup(true)
        .label(format!(
            "<b>{}</b> {}",
            glib::markup_escape_text(&c.user.username),
            util::linkify(&c.text)
        ))
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::WordChar)
        .xalign(0.0)
        .build();
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

pub fn open(main: &Rc<Main>, media: &Media) {
    let list = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(14)
        .margin_top(12)
        .margin_bottom(12)
        .margin_start(12)
        .margin_end(12)
        .build();
    list.append(&util::spinner_box());
    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&list)
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

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&scroller));
    toolbar.add_bottom_bar(&composer);

    let dialog = adw::Dialog::builder()
        .title("Comments")
        .content_width(480)
        .content_height(640)
        .child(&toolbar)
        .build();

    let weak = Rc::downgrade(main);
    let media_id = media.id.clone();
    {
        let client = main.client.clone();
        let media_id = media_id.clone();
        let weak = weak.clone();
        let list = list.downgrade();
        glib::spawn_future_local(async move {
            let res = rt::bg(async move { client.comments(&media_id).await }).await;
            let Some(list) = list.upgrade() else { return };
            util::clear_box(&list);
            match res {
                Ok(comments) if comments.is_empty() => {
                    list.append(
                        &gtk::Label::builder()
                            .label("No comments yet")
                            .css_classes(["dim-label"])
                            .margin_top(24)
                            .build(),
                    );
                }
                Ok(comments) => {
                    for c in &comments {
                        list.append(&comment_row(c));
                    }
                }
                Err(e) => {
                    list.append(
                        &gtk::Label::builder()
                            .label(e.to_string())
                            .wrap(true)
                            .css_classes(["dim-label"])
                            .margin_top(24)
                            .build(),
                    );
                    if let Some(main) = weak.upgrade() {
                        main.handle_error(&e);
                    }
                }
            }
        });
    }

    let send = {
        let entry = entry.clone();
        let post = post.clone();
        let list = list.downgrade();
        Rc::new(move || {
            let text = entry.text().trim().to_string();
            if text.is_empty() || !post.is_sensitive() {
                return;
            }
            let Some(main) = weak.upgrade() else { return };
            post.set_sensitive(false);
            let client = main.client.clone();
            let media_id = media_id.clone();
            let entry = entry.clone();
            let post = post.clone();
            let list = list.clone();
            let weak = weak.clone();
            glib::spawn_future_local(async move {
                let t = text.clone();
                let res = rt::bg(async move { client.add_comment(&media_id, &t).await }).await;
                post.set_sensitive(true);
                match res {
                    Ok(comment) => {
                        entry.set_text("");
                        if let Some(list) = list.upgrade() {
                            list.append(&comment_row(&comment));
                        }
                    }
                    Err(e) => {
                        if let Some(main) = weak.upgrade() {
                            main.handle_error(&e);
                        }
                    }
                }
            });
        })
    };
    {
        let send = send.clone();
        entry.connect_activate(move |_| send());
    }
    post.connect_clicked(move |_| send());

    dialog.present(Some(&main.window));
    entry.grab_focus();
}
