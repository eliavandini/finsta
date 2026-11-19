//! Saving posts (on Instagram, optionally into one of your folders) and
//! songs. instagram.com has no way to save a song, so Finsta keeps saved
//! songs itself, in ~/.config/finsta/saved_songs.json.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use serde::{Deserialize, Serialize};

use crate::api::{self, Media, Song};
use crate::rt;
use crate::ui::images;
use crate::ui::share::Notify;
use crate::ui::window::Main;

fn set_bookmark(button: &gtk::Button, on: bool, save: &str, unsave: &str) {
    button.set_icon_name(if on { "bookmark-filled-symbolic" } else { "bookmark-outline-symbolic" });
    button.set_tooltip_text(Some(if on { unsave } else { save }));
}

/// Save a post to (or remove it from) your saved posts on Instagram, with
/// a toast saying how it went (after saving, it offers a folder). `done`
/// gets whether it worked.
pub fn set_saved(main: &Rc<Main>, media: &Media, save: bool, done: impl FnOnce(bool) + 'static) {
    let weak = Rc::downgrade(main);
    let client = main.client.clone();
    let media = media.clone();
    glib::spawn_future_local(async move {
        let m = media.clone();
        let res = rt::bg(async move { client.save_post(&m, save, None).await }).await;
        let Some(main) = weak.upgrade() else { return done(res.is_ok()) };
        match &res {
            Ok(()) if save => {
                let toast = adw::Toast::builder().title("Saved").button_label("Add to Folder").build();
                let weak = Rc::downgrade(&main);
                toast.connect_button_clicked(move |_| {
                    if let Some(main) = weak.upgrade() {
                        choose_folder(&main, &media);
                    }
                });
                main.toasts.add_toast(toast);
            }
            Ok(()) => main.toast("Removed from Saved"),
            Err(e) if api::is_login_required(e) => main.handle_error(e),
            Err(e) => {
                eprintln!("finsta: save failed: {e}");
                main.toast(if save { "Couldn't save this post" } else { "Couldn't remove this post from Saved" });
            }
        }
        done(res.is_ok());
    });
}

/// The bookmark button on a post: saves it to (or removes it from) your
/// saved posts on Instagram, then offers to put it in a folder.
pub fn post_button(main: &Rc<Main>, media: &Media) -> gtk::Button {
    let saved = Rc::new(Cell::new(media.has_viewer_saved));
    let button = gtk::Button::builder().css_classes(["flat", "circular"]).build();
    set_bookmark(&button, saved.get(), "Save", "Remove from Saved");
    let weak = Rc::downgrade(main);
    let media = media.clone();
    button.connect_clicked(move |b| {
        let Some(main) = weak.upgrade() else { return };
        let now = !saved.get();
        saved.set(now);
        set_bookmark(b, now, "Save", "Remove from Saved");
        b.set_sensitive(false);
        let saved = saved.clone();
        let b = b.downgrade();
        set_saved(&main, &media, now, move |ok| {
            let Some(b) = b.upgrade() else { return };
            b.set_sensitive(true);
            if !ok {
                saved.set(!now);
                set_bookmark(&b, !now, "Save", "Remove from Saved");
            }
        });
    });
    button
}

fn note(text: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(text)
        .wrap(true)
        .justify(gtk::Justification::Center)
        .css_classes(["dim-label"])
        .margin_top(24)
        .margin_bottom(24)
        .margin_start(12)
        .margin_end(12)
        .build()
}

/// A square cover picture for a folder or a song.
pub fn cover(url: Option<String>, size: i32, fallback_icon: &str) -> gtk::Widget {
    let frame = gtk::Overlay::builder()
        .width_request(size)
        .height_request(size)
        .css_classes(["folder-cover"])
        .overflow(gtk::Overflow::Hidden)
        .valign(gtk::Align::Center)
        .build();
    frame.set_child(Some(
        &gtk::Image::builder().icon_name(fallback_icon).pixel_size(size / 3).build(),
    ));
    if url.is_some() {
        let picture = gtk::Picture::builder()
            .content_fit(gtk::ContentFit::Cover)
            .can_shrink(true)
            .build();
        images::load_picture(&picture, url);
        frame.add_overlay(&picture);
    }
    frame.upcast()
}

/// Pick one of your folders to save a post into.
pub fn choose_folder(main: &Rc<Main>, media: &Media) {
    let list = gtk::ListBox::builder()
        .css_classes(["boxed-list"])
        .selection_mode(gtk::SelectionMode::None)
        .margin_top(12)
        .margin_bottom(12)
        .margin_start(12)
        .margin_end(12)
        .valign(gtk::Align::Start)
        .build();
    let stack = gtk::Stack::new();
    stack.add_named(&crate::ui::util::spinner_box(), Some("loading"));
    stack.add_named(
        &gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&list)
            .build(),
        Some("list"),
    );
    let message = gtk::Box::new(gtk::Orientation::Vertical, 0);
    stack.add_named(&message, Some("message"));
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&stack));
    let dialog = adw::Dialog::builder()
        .title("Save to Folder")
        .content_width(380)
        .content_height(460)
        .child(&toolbar)
        .build();
    dialog.present(Some(&main.window));

    let weak = Rc::downgrade(main);
    let client = main.client.clone();
    let media = media.clone();
    let dialog_weak = dialog.downgrade();
    glib::spawn_future_local(async move {
        let res = rt::bg(async move { client.saved_folders().await }).await;
        let Some(main) = weak.upgrade() else { return };
        match res {
            Ok(folders) if folders.is_empty() => {
                message.append(&note("You have no folders yet. Folders you make in the Instagram app show up here."));
                stack.set_visible_child_name("message");
            }
            Ok(folders) => {
                let folders = Rc::new(folders);
                for f in folders.iter() {
                    let row = adw::ActionRow::builder()
                        .title(&f.name)
                        .use_markup(false)
                        .activatable(true)
                        .build();
                    if let Some(n) = f.count {
                        row.set_subtitle(&crate::ui::util::plural(n, "post", "posts"));
                    }
                    row.add_prefix(&cover(f.cover.clone(), 40, "folder-symbolic"));
                    list.append(&row);
                }
                stack.set_visible_child_name("list");
                list.connect_row_activated(move |_, row| {
                    let Some(folder) = folders.get(row.index() as usize).cloned() else { return };
                    if let Some(d) = dialog_weak.upgrade() {
                        d.close();
                    }
                    let Some(main) = weak.upgrade() else { return };
                    let client = main.client.clone();
                    let media = media.clone();
                    let weak = weak.clone();
                    glib::spawn_future_local(async move {
                        let id = folder.id.clone();
                        let res = rt::bg(async move { client.save_post(&media, true, Some(&id)).await }).await;
                        let Some(main) = weak.upgrade() else { return };
                        match res {
                            Ok(()) => main.toast(&format!("Saved to {}", folder.name)),
                            Err(e) if api::is_login_required(&e) => main.handle_error(&e),
                            Err(e) => {
                                eprintln!("finsta: saving to a folder failed: {e}");
                                main.toast("Couldn't add this post to the folder");
                            }
                        }
                    });
                });
            }
            Err(e) => {
                message.append(&note(&format!("Couldn't load your folders: {e}")));
                stack.set_visible_child_name("message");
                if api::is_login_required(&e) {
                    main.handle_error(&e);
                }
            }
        }
    });
}

/// A song you saved, and where you heard it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedSong {
    pub song: Song,
    /// The post, reel or story it was on.
    #[serde(default)]
    pub from: Option<String>,
    #[serde(default)]
    pub saved_at: i64,
}

fn songs_path() -> Option<PathBuf> {
    Some(dirs::config_dir()?.join("finsta").join("saved_songs.json"))
}

thread_local! {
    static SONGS: RefCell<Option<Vec<SavedSong>>> = const { RefCell::new(None) };
}

fn song_key(song: &Song) -> String {
    if song.id.is_empty() { song.label().to_lowercase() } else { song.id.clone() }
}

/// Saved songs, newest first.
pub fn saved_songs() -> Vec<SavedSong> {
    SONGS.with_borrow_mut(|songs| {
        songs
            .get_or_insert_with(|| {
                songs_path()
                    .and_then(|p| std::fs::read_to_string(p).ok())
                    .and_then(|s| serde_json::from_str(&s).ok())
                    .unwrap_or_default()
            })
            .clone()
    })
}

pub fn is_song_saved(song: &Song) -> bool {
    let key = song_key(song);
    saved_songs().iter().any(|s| song_key(&s.song) == key)
}

pub fn set_song_saved(song: &Song, from: Option<String>, on: bool) {
    let key = song_key(song);
    let mut songs = saved_songs();
    songs.retain(|s| song_key(&s.song) != key);
    if on {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as i64);
        songs.insert(0, SavedSong { song: song.clone(), from, saved_at: now });
    }
    SONGS.with_borrow_mut(|s| *s = Some(songs.clone()));
    let Some(path) = songs_path() else { return };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Err(e) = std::fs::write(&path, serde_json::to_string_pretty(&songs).unwrap_or_default()) {
        eprintln!("finsta: couldn't save the saved songs ({})", e.kind());
    }
}

/// The small bookmark button next to a song. `notify` reports what
/// happened instead of a toast (the story viewer covers toasts).
pub fn song_button(main: &Rc<Main>, song: &Song, from: Option<String>, notify: Option<Notify>) -> gtk::Button {
    let button = gtk::Button::builder()
        .css_classes(["flat", "circular", "song-save"])
        .valign(gtk::Align::Center)
        .build();
    set_bookmark(&button, is_song_saved(song), "Save Song", "Remove from Saved Songs");
    let weak = Rc::downgrade(main);
    let song = song.clone();
    button.connect_clicked(move |b| {
        let on = !is_song_saved(&song);
        set_song_saved(&song, from.clone(), on);
        set_bookmark(b, on, "Save Song", "Remove from Saved Songs");
        if let Some(notify) = &notify {
            notify(if on { "Song saved" } else { "Song removed from Saved" });
        } else if let Some(main) = weak.upgrade() {
            if on {
                let toast = adw::Toast::builder().title("Song saved").button_label("Show").build();
                let weak = weak.clone();
                toast.connect_button_clicked(move |_| {
                    if let Some(main) = weak.upgrade() {
                        main.open_saved_songs();
                    }
                });
                main.toasts.add_toast(toast);
            } else {
                main.toast("Song removed from Saved");
            }
        }
    });
    button
}
