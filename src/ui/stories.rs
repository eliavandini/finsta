//! Story viewer: plays the stories of the accounts in the home feed's tray.

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::time::Duration;

use adw::prelude::*;
use gtk::{gdk, glib};

use crate::api::Reel;
use crate::rt;
use crate::ui::story_player::StoryPlayer;
use crate::ui::window::Main;
use crate::ui::{images, util};

const PHOTO_SECONDS: f64 = 6.0;
const TICK_MS: u64 = 50;

struct Viewer {
    main: Weak<Main>,
    dialog: glib::WeakRef<adw::Dialog>,
    reels: RefCell<Vec<Reel>>,
    reel: Cell<usize>,
    item: Cell<usize>,
    progress: gtk::Box,
    bars: RefCell<Vec<gtk::ProgressBar>>,
    stack: gtk::Stack,
    picture: gtk::Picture,
    /// Shared story video player (None: videos show their cover image).
    player: Option<Rc<StoryPlayer>>,
    /// Whether the current item is a video playing in `player`.
    playing_video: Cell<bool>,
    avatar_slot: gtk::Box,
    name: gtk::Label,
    time: gtk::Label,
    pause_button: gtk::Button,
    paused: Cell<bool>,
    elapsed: Cell<f64>,
    ready: Cell<bool>,
    token: Cell<u64>,
}

pub fn open(main: &Rc<Main>, reels: Vec<Reel>, start: usize) {
    if reels.is_empty() {
        return;
    }
    let picture = gtk::Picture::builder()
        .content_fit(gtk::ContentFit::Contain)
        .can_shrink(true)
        .build();
    let player = StoryPlayer::get();
    let video = gtk::Picture::builder()
        .content_fit(gtk::ContentFit::Contain)
        .can_shrink(true)
        .build();
    if let Some(p) = &player {
        video.set_paintable(Some(p.paintable()));
    }
    let stack = gtk::Stack::builder()
        .transition_type(gtk::StackTransitionType::Crossfade)
        .vexpand(true)
        .hexpand(true)
        .build();
    stack.add_named(&picture, Some("picture"));
    stack.add_named(&video, Some("video"));
    let loading = util::spinner_box();
    loading.set_valign(gtk::Align::Center);
    stack.add_named(&loading, Some("loading"));
    let error = gtk::Label::builder()
        .label("This story couldn't be loaded")
        .css_classes(["dim-label"])
        .build();
    stack.add_named(&error, Some("error"));

    let progress = gtk::Box::builder().spacing(4).homogeneous(true).build();
    let top = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(8)
        .valign(gtk::Align::Start)
        .css_classes(["story-top"])
        .build();
    let info = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let avatar_slot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    let name = gtk::Label::builder()
        .css_classes(["heading"])
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .build();
    let time = gtk::Label::builder().css_classes(["caption"]).opacity(0.8).build();
    let pause_button = gtk::Button::builder()
        .icon_name("media-playback-pause-symbolic")
        .tooltip_text("Pause")
        .css_classes(["flat", "circular"])
        .build();
    let close = gtk::Button::builder()
        .icon_name("window-close-symbolic")
        .tooltip_text("Close")
        .css_classes(["flat", "circular"])
        .build();
    info.append(&avatar_slot);
    info.append(&name);
    info.append(&time);
    info.append(&gtk::Box::builder().hexpand(true).build());
    info.append(&pause_button);
    info.append(&close);
    top.append(&progress);
    top.append(&info);

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

    let overlay = gtk::Overlay::builder().child(&stack).css_classes(["story-viewer"]).build();
    overlay.add_overlay(&top);
    overlay.add_overlay(&prev);
    overlay.add_overlay(&next);

    let dialog = adw::Dialog::builder()
        .content_width(440)
        .content_height(800)
        .child(&overlay)
        .title("Stories")
        .build();

    let viewer = Rc::new(Viewer {
        main: Rc::downgrade(main),
        dialog: dialog.downgrade(),
        reels: RefCell::new(reels),
        reel: Cell::new(start),
        item: Cell::new(0),
        progress,
        bars: RefCell::new(Vec::new()),
        stack,
        picture,
        player,
        playing_video: Cell::new(false),
        avatar_slot,
        name,
        time,
        pause_button,
        paused: Cell::new(false),
        elapsed: Cell::new(0.0),
        ready: Cell::new(false),
        token: Cell::new(0),
    });

    let weak = Rc::downgrade(&viewer);
    close.connect_clicked(glib::clone!(
        #[weak]
        dialog,
        move |_| {
            dialog.close();
        }
    ));
    prev.connect_clicked(glib::clone!(
        #[strong]
        weak,
        move |_| {
            if let Some(v) = weak.upgrade() {
                v.previous();
            }
        }
    ));
    next.connect_clicked(glib::clone!(
        #[strong]
        weak,
        move |_| {
            if let Some(v) = weak.upgrade() {
                v.next();
            }
        }
    ));
    viewer.pause_button.connect_clicked(glib::clone!(
        #[strong]
        weak,
        move |_| {
            if let Some(v) = weak.upgrade() {
                v.toggle_pause();
            }
        }
    ));
    let keys = gtk::EventControllerKey::new();
    keys.connect_key_pressed(glib::clone!(
        #[strong]
        weak,
        move |_, key, _, _| {
            let Some(v) = weak.upgrade() else { return glib::Propagation::Proceed };
            match key {
                gdk::Key::Left => v.previous(),
                gdk::Key::Right => v.next(),
                gdk::Key::space => v.toggle_pause(),
                _ => return glib::Propagation::Proceed,
            }
            glib::Propagation::Stop
        }
    ));
    dialog.add_controller(keys);
    dialog.connect_closed(glib::clone!(
        #[strong]
        weak,
        move |_| {
            if let Some(v) = weak.upgrade() {
                v.token.set(v.token.get() + 1);
                v.stop_media();
            }
        }
    ));

    glib::timeout_add_local(Duration::from_millis(TICK_MS), glib::clone!(
        #[strong]
        weak,
        move || match weak.upgrade() {
            Some(v) if v.dialog.upgrade().is_some() => {
                v.tick();
                glib::ControlFlow::Continue
            }
            _ => glib::ControlFlow::Break,
        }
    ));

    viewer.show(start, 0);
    util::attach(&dialog, viewer);
    dialog.present(Some(&main.window));
}

/// Describe a person's stories once per run (counts only), to check that
/// what Instagram sends is what the viewer can show.
fn log_items(items: &[crate::api::Media], can_play: bool) {
    static LOGGED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if LOGGED.swap(true, std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    let videos = items.iter().filter(|i| i.is_video()).count();
    let playable = items.iter().filter(|i| i.is_video() && i.video_url().is_some()).count();
    let images = items.iter().filter(|i| i.image_url(1080).is_some()).count();
    eprintln!(
        "finsta: stories: {} items, {videos} videos ({playable} with an address), {images} with an image; videos {}",
        items.len(),
        if can_play { "play" } else { "show their cover image" }
    );
}

impl Viewer {
    fn stop_media(&self) {
        if self.playing_video.replace(false)
            && let Some(player) = &self.player
        {
            player.stop();
        }
    }

    fn show(self: &Rc<Self>, reel_index: usize, item_index: usize) {
        let token = self.token.get() + 1;
        self.token.set(token);
        self.stop_media();
        self.ready.set(false);
        self.elapsed.set(0.0);
        self.reel.set(reel_index);
        self.item.set(item_index);

        let Some(reel) = self.reels.borrow().get(reel_index).cloned() else {
            self.close();
            return;
        };
        util::clear_box(&self.avatar_slot);
        self.avatar_slot.append(&util::avatar(&reel.user, 32));
        self.name.set_label(&reel.user.username);

        if reel.items.is_empty() {
            self.stack.set_visible_child_name("loading");
            self.set_bars(1, 0);
            let Some(main) = self.main.upgrade() else { return };
            let client = main.client.clone();
            let id = reel.id.clone();
            let weak = Rc::downgrade(self);
            glib::spawn_future_local(async move {
                let ids = vec![id.clone()];
                let res = rt::bg(async move { client.reels_media(&ids).await }).await;
                let Some(v) = weak.upgrade() else { return };
                if v.token.get() != token {
                    return;
                }
                match res {
                    Ok(mut map) => {
                        let items = map.remove(&id).map(|r| r.items).unwrap_or_default();
                        log_items(&items, v.player.is_some());
                        if items.is_empty() {
                            // Expired or hidden; move on to the next person.
                            v.show_next_reel();
                            return;
                        }
                        if let Some(r) = v.reels.borrow_mut().get_mut(reel_index) {
                            r.items = items;
                        }
                        v.show(reel_index, item_index);
                    }
                    Err(e) => {
                        v.stack.set_visible_child_name("error");
                        if let Some(main) = v.main.upgrade() {
                            main.handle_error(&e);
                        }
                    }
                }
            });
            return;
        }

        let item_index = item_index.min(reel.items.len() - 1);
        self.item.set(item_index);
        self.set_bars(reel.items.len(), item_index);
        let item = &reel.items[item_index];
        self.time.set_label(&util::relative_time(item.taken_at));

        if let Some(main) = self.main.upgrade()
            && let Some(home) = main.home.get()
        {
            home.reel_viewed(&reel);
        }

        let video = item.video_url().filter(|_| item.is_video());
        if let (Some(url), Some(player)) = (&video, &self.player) {
            player.load(url, !self.paused.get());
            self.playing_video.set(true);
            self.stack.set_visible_child_name("video");
            self.ready.set(true);
        } else {
            // A photo, or a video's cover image when videos can't play.
            self.picture.set_paintable(gdk::Paintable::NONE);
            self.stack.set_visible_child_name("loading");
            let url = item.image_url(1080);
            let weak = Rc::downgrade(self);
            glib::spawn_future_local(async move {
                let tex = match url {
                    Some(u) => images::texture(&u).await,
                    None => None,
                };
                let Some(v) = weak.upgrade() else { return };
                if v.token.get() != token {
                    return;
                }
                match tex {
                    Some(tex) => {
                        v.picture.set_paintable(Some(&tex));
                        v.stack.set_visible_child_name("picture");
                        v.ready.set(true);
                    }
                    None => {
                        v.stack.set_visible_child_name("error");
                        v.ready.set(true);
                    }
                }
            });
        }
    }

    fn set_bars(&self, count: usize, current: usize) {
        util::clear_box(&self.progress);
        let mut bars = Vec::new();
        for i in 0..count {
            let bar = gtk::ProgressBar::builder()
                .fraction(if i < current { 1.0 } else { 0.0 })
                .hexpand(true)
                .build();
            self.progress.append(&bar);
            bars.push(bar);
        }
        self.bars.replace(bars);
    }

    fn tick(self: &Rc<Self>) {
        if !self.ready.get() || self.paused.get() {
            return;
        }
        let fraction = if let Some(player) = self.player.as_ref().filter(|_| self.playing_video.get()) {
            if player.failed() {
                // Unplayable (expired, or a format that isn't supported): move on.
                1.0
            } else {
                player.progress().unwrap_or(0.0)
            }
        } else {
            self.elapsed.set(self.elapsed.get() + TICK_MS as f64 / 1000.0);
            self.elapsed.get() / PHOTO_SECONDS
        };
        if let Some(bar) = self.bars.borrow().get(self.item.get()) {
            bar.set_fraction(fraction.clamp(0.0, 1.0));
        }
        if fraction >= 1.0 {
            self.next();
        }
    }

    fn toggle_pause(&self) {
        let paused = !self.paused.get();
        self.paused.set(paused);
        if let Some(player) = self.player.as_ref().filter(|_| self.playing_video.get()) {
            player.set_playing(!paused);
        }
        self.pause_button.set_icon_name(if paused {
            "media-playback-start-symbolic"
        } else {
            "media-playback-pause-symbolic"
        });
        self.pause_button
            .set_tooltip_text(Some(if paused { "Play" } else { "Pause" }));
    }

    fn next(self: &Rc<Self>) {
        let count = self
            .reels
            .borrow()
            .get(self.reel.get())
            .map(|r| r.items.len())
            .unwrap_or(0);
        if self.item.get() + 1 < count {
            self.show(self.reel.get(), self.item.get() + 1);
        } else {
            self.show_next_reel();
        }
    }

    fn show_next_reel(self: &Rc<Self>) {
        if self.reel.get() + 1 < self.reels.borrow().len() {
            self.show(self.reel.get() + 1, 0);
        } else {
            self.close();
        }
    }

    fn previous(self: &Rc<Self>) {
        if self.item.get() > 0 {
            self.show(self.reel.get(), self.item.get() - 1);
        } else if self.reel.get() > 0 {
            self.show(self.reel.get() - 1, 0);
        } else {
            self.show(0, 0);
        }
    }

    fn close(&self) {
        self.token.set(self.token.get() + 1);
        self.stop_media();
        if let Some(d) = self.dialog.upgrade() {
            d.close();
        }
    }
}
