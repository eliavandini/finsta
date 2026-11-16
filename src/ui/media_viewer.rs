//! Full-size view of a photo or video from a conversation.

use std::rc::Rc;

use adw::prelude::*;
use gtk::gio;

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
