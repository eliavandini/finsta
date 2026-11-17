//! What you saved: your folders across the top (All Posts, the folders you
//! made on Instagram, and the songs you saved in Finsta), and the chosen
//! folder's posts below, a page at a time with a "Show More" button.

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

use adw::prelude::*;
use gtk::glib;

use crate::api::Media;
use crate::api::web::Collection;
use crate::rt;
use crate::ui::profile::thumbnail;
use crate::ui::saving::{self, SavedSong};
use crate::ui::util;
use crate::ui::window::Main;

#[derive(Clone)]
enum Folder {
    All,
    Collection(Collection),
    Songs,
}

impl Folder {
    fn name(&self) -> String {
        match self {
            Folder::All => "All Posts".into(),
            Folder::Collection(c) => c.name.clone(),
            Folder::Songs => "Songs".into(),
        }
    }
}

/// Which folder the page opens on.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Start {
    All,
    Songs,
}

struct Saved {
    main: Weak<Main>,
    strip: gtk::Box,
    folders: RefCell<Vec<Folder>>,
    cards: RefCell<Vec<gtk::Button>>,
    selected: Cell<usize>,
    all_cover: RefCell<Option<String>>,
    heading: gtk::Label,
    grid: gtk::FlowBox,
    songs: gtk::ListBox,
    status: gtk::Box,
    more: gtk::Button,
    posts: RefCell<Vec<Media>>,
    next: RefCell<Option<String>>,
    generation: Cell<u64>,
}

const THUMB: i32 = 200;

pub fn page(main: &Rc<Main>, start: Start) -> adw::NavigationPage {
    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .margin_top(12)
        .margin_bottom(24)
        .build();
    content.append(
        &gtk::Label::builder()
            .label("Saved")
            .xalign(0.0)
            .margin_start(18)
            .margin_end(18)
            .css_classes(["title-2"])
            .build(),
    );
    let strip = gtk::Box::builder().spacing(10).margin_start(12).margin_end(12).build();
    content.append(
        &gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Automatic)
            .vscrollbar_policy(gtk::PolicyType::Never)
            .child(&strip)
            .build(),
    );
    let heading = gtk::Label::builder()
        .xalign(0.0)
        .margin_start(18)
        .margin_end(18)
        .margin_top(6)
        .css_classes(["title-4"])
        .build();
    content.append(&heading);
    let status = gtk::Box::new(gtk::Orientation::Vertical, 0);
    content.append(&status);
    let grid = gtk::FlowBox::builder()
        .min_children_per_line(3)
        .max_children_per_line(3)
        .homogeneous(true)
        .column_spacing(4)
        .row_spacing(4)
        .margin_start(12)
        .margin_end(12)
        .selection_mode(gtk::SelectionMode::None)
        .build();
    content.append(&grid);
    let songs = gtk::ListBox::builder()
        .css_classes(["boxed-list"])
        .selection_mode(gtk::SelectionMode::None)
        .margin_start(18)
        .margin_end(18)
        .visible(false)
        .build();
    content.append(&songs);
    let more = gtk::Button::builder()
        .label("Show More Saved Posts")
        .css_classes(["pill"])
        .halign(gtk::Align::Center)
        .visible(false)
        .build();
    content.append(&more);
    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&content)
        .build();
    let page = adw::NavigationPage::builder().title("Saved").tag("saved").child(&scroller).build();

    let saved = Rc::new(Saved {
        main: Rc::downgrade(main),
        strip,
        folders: RefCell::new(vec![Folder::All, Folder::Songs]),
        cards: RefCell::new(Vec::new()),
        selected: Cell::new(if start == Start::Songs { 1 } else { 0 }),
        all_cover: RefCell::new(None),
        heading,
        grid,
        songs,
        status,
        more,
        posts: RefCell::new(Vec::new()),
        next: RefCell::new(None),
        generation: Cell::new(0),
    });

    // As many columns of thumbnails as fit.
    let grid = saved.grid.downgrade();
    scroller.hadjustment().connect_page_size_notify(move |adj| {
        let Some(grid) = grid.upgrade() else { return };
        let n = ((adj.page_size() as i32 - 24) / THUMB).clamp(3, 8) as u32;
        if n != grid.max_children_per_line() {
            glib::idle_add_local_once(move || {
                grid.set_min_children_per_line(n);
                grid.set_max_children_per_line(n);
            });
        }
    });
    let weak = Rc::downgrade(&saved);
    saved.grid.connect_child_activated(move |_, child| {
        let Some(s) = weak.upgrade() else { return };
        let media = s.posts.borrow().get(child.index() as usize).cloned();
        if let (Some(media), Some(main)) = (media, s.main.upgrade()) {
            main.open_post(media);
        }
    });
    let weak = Rc::downgrade(&saved);
    saved.more.connect_clicked(move |_| {
        if let Some(s) = weak.upgrade() {
            s.load();
        }
    });
    saved.render_strip();
    saved.select(saved.selected.get());
    saved.load_folders();
    util::attach(&page, saved);
    page
}

impl Saved {
    fn render_strip(self: &Rc<Self>) {
        util::clear_box(&self.strip);
        let mut cards = Vec::new();
        let folders = self.folders.borrow().clone();
        for (index, folder) in folders.iter().enumerate() {
            let (cover, icon, count) = match folder {
                Folder::All => (self.all_cover.borrow().clone(), "bookmark-outline-symbolic", None),
                Folder::Collection(c) => (c.cover.clone(), "folder-symbolic", c.count.map(|n| util::plural(n, "post", "posts"))),
                Folder::Songs => {
                    let songs = saving::saved_songs();
                    let cover = songs.iter().find_map(|s| s.song.cover.clone());
                    (cover, "audio-x-generic-symbolic", Some(util::plural(songs.len() as i64, "song", "songs")))
                }
            };
            let column = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(4).build();
            column.append(&saving::cover(cover, 112, icon));
            column.append(
                &gtk::Label::builder()
                    .label(folder.name())
                    .max_width_chars(14)
                    .ellipsize(gtk::pango::EllipsizeMode::End)
                    .css_classes(["heading"])
                    .build(),
            );
            if let Some(count) = count {
                column.append(&gtk::Label::builder().label(count).css_classes(["caption", "dim-label"]).build());
            }
            let card = gtk::Button::builder()
                .child(&column)
                .css_classes(["flat", "folder-card"])
                .tooltip_text(folder.name())
                .build();
            if index == self.selected.get() {
                card.add_css_class("selected");
            }
            let weak = Rc::downgrade(self);
            card.connect_clicked(move |_| {
                if let Some(s) = weak.upgrade()
                    && s.selected.get() != index
                {
                    s.select(index);
                }
            });
            self.strip.append(&card);
            cards.push(card);
        }
        self.cards.replace(cards);
    }

    fn select(self: &Rc<Self>, index: usize) {
        self.selected.set(index);
        for (i, card) in self.cards.borrow().iter().enumerate() {
            if i == index {
                card.add_css_class("selected");
            } else {
                card.remove_css_class("selected");
            }
        }
        let Some(folder) = self.folders.borrow().get(index).cloned() else { return };
        self.heading.set_label(&folder.name());
        self.generation.set(self.generation.get() + 1);
        self.grid.remove_all();
        self.posts.borrow_mut().clear();
        self.next.replace(None);
        self.more.set_visible(false);
        util::clear_box(&self.status);
        let songs = matches!(folder, Folder::Songs);
        self.grid.set_visible(!songs);
        self.songs.set_visible(songs);
        if songs {
            self.render_songs();
        } else {
            self.status.append(&util::spinner_box());
            self.load();
        }
    }

    fn folder_id(&self) -> Option<String> {
        match self.folders.borrow().get(self.selected.get()) {
            Some(Folder::Collection(c)) => Some(c.id.clone()),
            _ => None,
        }
    }

    fn load(self: &Rc<Self>) {
        let Some(main) = self.main.upgrade() else { return };
        self.more.set_sensitive(false);
        let client = main.client.clone();
        let next = self.next.borrow().clone();
        let folder = self.folder_id();
        let generation = self.generation.get();
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let res = rt::bg(async move { client.saved_posts(folder.as_deref(), next).await }).await;
            let Some(s) = weak.upgrade() else { return };
            if s.generation.get() != generation {
                return;
            }
            s.more.set_sensitive(true);
            util::clear_box(&s.status);
            match res {
                Ok(page) => {
                    let first = s.posts.borrow().is_empty();
                    for m in &page.items {
                        s.grid.append(&thumbnail(m));
                    }
                    // "All Posts" gets the newest saved post as its cover.
                    if first
                        && s.selected.get() == 0
                        && s.all_cover.borrow().is_none()
                        && let Some(url) = page.items.first().and_then(|m| m.image_url(320))
                    {
                        s.all_cover.replace(Some(url));
                        s.render_strip();
                    }
                    s.posts.borrow_mut().extend(page.items);
                    let next = page.next_max_id.filter(|_| page.more_available);
                    s.more.set_visible(next.is_some());
                    s.next.replace(next);
                    if s.posts.borrow().is_empty() {
                        s.status.append(&message(if s.selected.get() == 0 {
                            "Nothing saved yet. Use the bookmark button on a post to save it."
                        } else {
                            "This folder is empty."
                        }));
                    }
                }
                Err(e) => {
                    s.status.append(&message(&format!("Couldn't load your saved posts: {e}")));
                    if let Some(main) = s.main.upgrade()
                        && crate::api::is_login_required(&e)
                    {
                        main.handle_error(&e);
                    }
                }
            }
        });
    }

    /// Your folders on Instagram go between "All Posts" and "Songs".
    fn load_folders(self: &Rc<Self>) {
        let Some(main) = self.main.upgrade() else { return };
        let client = main.client.clone();
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let res = rt::bg(async move { client.saved_folders().await }).await;
            let Some(s) = weak.upgrade() else { return };
            match res {
                Ok(list) if !list.is_empty() => {
                    let on_songs = matches!(s.folders.borrow().get(s.selected.get()), Some(Folder::Songs));
                    let mut folders = vec![Folder::All];
                    folders.extend(list.into_iter().map(Folder::Collection));
                    folders.push(Folder::Songs);
                    let songs_index = folders.len() - 1;
                    s.folders.replace(folders);
                    if on_songs {
                        s.selected.set(songs_index);
                    }
                    s.render_strip();
                }
                Ok(_) => {}
                Err(e) => eprintln!("finsta: saved folders failed: {e}"),
            }
        });
    }

    fn render_songs(self: &Rc<Self>) {
        self.songs.remove_all();
        let songs = saving::saved_songs();
        if songs.is_empty() {
            self.songs.set_visible(false);
            self.status.append(&message(
                "No saved songs yet. Use the bookmark next to a song on a post, reel or story to save it here.",
            ));
            return;
        }
        for saved in songs {
            self.songs.append(&self.song_row(saved));
        }
    }

    fn song_row(self: &Rc<Self>, saved: SavedSong) -> adw::ActionRow {
        let song = &saved.song;
        let row = adw::ActionRow::builder()
            .title(if song.title.is_empty() { &song.artist } else { &song.title })
            .use_markup(false)
            .title_lines(1)
            .build();
        if !song.title.is_empty() && !song.artist.is_empty() {
            row.set_subtitle(&song.artist);
        }
        row.add_prefix(&saving::cover(song.cover.clone(), 48, "audio-x-generic-symbolic"));
        if let Some(from) = saved.from.clone() {
            let what = if from.contains("/stories/") {
                "Story"
            } else if from.contains("/reel/") {
                "Reel"
            } else {
                "Post"
            };
            let open = gtk::Button::builder()
                .label(what)
                .tooltip_text(format!("Open the {} With This Song", what))
                .css_classes(["flat"])
                .valign(gtk::Align::Center)
                .build();
            let weak = Rc::downgrade(self);
            open.connect_clicked(move |_| {
                if let Some(main) = weak.upgrade().and_then(|s| s.main.upgrade()) {
                    main.open_link(&from);
                }
            });
            row.add_suffix(&open);
        }
        let remove = gtk::Button::builder()
            .icon_name("user-trash-symbolic")
            .tooltip_text("Remove from Saved Songs")
            .css_classes(["flat", "circular"])
            .valign(gtk::Align::Center)
            .build();
        let weak = Rc::downgrade(self);
        let song = song.clone();
        remove.connect_clicked(move |_| {
            saving::set_song_saved(&song, None, false);
            if let Some(s) = weak.upgrade() {
                util::clear_box(&s.status);
                s.songs.set_visible(true);
                s.render_songs();
                s.render_strip();
            }
        });
        row.add_suffix(&remove);
        row
    }
}

fn message(text: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(text)
        .wrap(true)
        .justify(gtk::Justification::Center)
        .css_classes(["dim-label"])
        .margin_top(24)
        .margin_start(18)
        .margin_end(18)
        .build()
}
