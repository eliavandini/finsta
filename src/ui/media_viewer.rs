//! Full-size view of a photo or video from a conversation.

use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, gio};

use crate::ui::images;
use crate::ui::window::Main;

pub fn open(main: &Rc<Main>, image: Option<String>, video: Option<String>) {
    let content: gtk::Widget = match video {
        Some(url) => {
            let stream = gtk::MediaFile::for_file(&gio::File::for_uri(&url));
            let video = gtk::Video::builder().autoplay(true).vexpand(true).build();
            video.set_media_stream(Some(&stream));
            video.upcast()
        }
        None => {
            let picture = gtk::Picture::builder()
                .content_fit(gtk::ContentFit::Contain)
                .can_shrink(true)
                .vexpand(true)
                .build();
            images::load_picture(&picture, image);
            picture.upcast()
        }
    };
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&content));
    let dialog = adw::Dialog::builder()
        .content_width(900)
        .content_height(900)
        .child(&toolbar)
        .build();
    dialog.present(Some(&main.window));
}

/// What to show for a disappearing message that has already been reported as opened.
pub enum Ephemeral {
    /// Already downloaded (into memory only).
    Photo(gdk::Texture),
    Video(String),
}

/// Shows a disappearing photo or video. Nothing is cached, and a video plays
/// once without controls, so it can't be scrubbed back and rewatched.
pub fn open_disappearing(main: &Rc<Main>, content: Ephemeral, note: &str) {
    let overlay = gtk::Overlay::builder().vexpand(true).build();
    let title = match &content {
        Ephemeral::Photo(_) => "Disappearing Photo",
        Ephemeral::Video(_) => "Disappearing Video",
    };
    let picture = gtk::Picture::builder()
        .content_fit(gtk::ContentFit::Contain)
        .can_shrink(true)
        .build();
    overlay.set_child(Some(&picture));
    let stream = match content {
        Ephemeral::Photo(texture) => {
            picture.set_paintable(Some(&texture));
            None
        }
        Ephemeral::Video(url) => {
            let stream = gtk::MediaFile::for_file(&gio::File::for_uri(&url));
            stream.set_loop(false);
            picture.set_paintable(Some(&stream));
            stream.play();
            let ended = gtk::Label::builder()
                .label("Finished")
                .css_classes(["osd", "media-counter"])
                .halign(gtk::Align::Center)
                .valign(gtk::Align::Center)
                .visible(false)
                .build();
            overlay.add_overlay(&ended);
            stream.connect_ended_notify(move |s| ended.set_visible(s.is_ended()));
            Some(stream)
        }
    };

    let header = adw::HeaderBar::builder()
        .title_widget(&adw::WindowTitle::new(title, note))
        .build();
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&overlay));
    let dialog = adw::Dialog::builder()
        .content_width(560)
        .content_height(900)
        .child(&toolbar)
        .build();
    dialog.connect_closed(move |_| {
        if let Some(stream) = &stream {
            stream.pause();
            stream.set_file(gio::File::NONE);
        }
        picture.set_paintable(gdk::Paintable::NONE);
    });
    dialog.present(Some(&main.window));
}
