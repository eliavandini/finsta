//! Story viewer: plays the stories of the accounts in the home feed's tray.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::{Rc, Weak};
use std::time::Duration;

use adw::prelude::*;
use gtk::{gdk, glib};

use crate::api::{Media, Reel, StoryReply, User};
use crate::rt;
use crate::ui::menu::Menu;
use crate::ui::story_stage::StoryStage;
use crate::ui::video::{self, Opts, Player};
use crate::ui::window::Main;
use crate::ui::{download, images, menu, post, share, util};

const PHOTO_SECONDS: f64 = 6.0;
/// A photo story with music lasts as long as its part of the song, up to this.
const SONG_SECONDS: f64 = 15.0;
const TICK_MS: u64 = 50;
/// Item index meaning "the first story you haven't watched yet".
const FIRST_UNSEEN: usize = usize::MAX;

thread_local! {
    static MUTED: Cell<bool> = const { Cell::new(false) };
}

struct Viewer {
    main: Weak<Main>,
    me: String,
    dialog: glib::WeakRef<adw::Dialog>,
    reels: RefCell<Vec<Reel>>,
    reel: Cell<usize>,
    item: Cell<usize>,
    progress: gtk::Box,
    bars: RefCell<Vec<gtk::ProgressBar>>,
    stack: gtk::Stack,
    picture: gtk::Picture,
    video: gtk::Picture,
    /// The app's video player (None: videos show their cover image).
    player: Option<Rc<Player>>,
    /// Keeps feed videos from playing behind the viewer.
    exclusive: RefCell<Option<video::Exclusive>>,
    /// The current item's video, if one is playing (0: none).
    ticket: Cell<u64>,
    /// A photo story's song, if one is playing (0: none).
    song_ticket: Cell<u64>,
    /// How long the current photo shows.
    photo_seconds: Cell<f64>,
    /// The story in the middle, the people before and after on the sides.
    stage: StoryStage,
    /// Which person the side cards were drawn for.
    sides_for: Cell<Option<usize>>,
    /// People whose stories were asked for already.
    requested: RefCell<HashSet<String>>,
    avatar_slot: gtk::Box,
    name: gtk::Label,
    time: gtk::Label,
    pause_button: gtk::Button,
    mute_button: gtk::Button,
    bottom: gtk::Box,
    /// Music, "Reshared from", mentions (under the name).
    details: gtk::Box,
    /// Buttons for a shared post or a link (above the reply field).
    extras: gtk::Box,
    download_button: gtk::Button,
    share_button: gtk::Button,
    reply: gtk::Entry,
    like_button: gtk::Button,
    notice: gtk::Label,
    notice_token: Cell<u64>,
    typing: Cell<bool>,
    paused_for_typing: Cell<bool>,
    paused: Cell<bool>,
    elapsed: Cell<f64>,
    ready: Cell<bool>,
    token: Cell<u64>,
}

pub fn open(main: &Rc<Main>, reels: Vec<Reel>, start: usize) {
    if reels.is_empty() {
        return;
    }
    // Opened on someone with new stories: go on only through people with
    // new stories (each starts at their first one you haven't watched).
    let (reels, start) = if reels.get(start).is_some_and(Reel::has_unseen) {
        let id = reels[start].id.clone();
        let kept: Vec<Reel> = reels.into_iter().filter(Reel::has_unseen).collect();
        let start = kept.iter().position(|r| r.id == id).unwrap_or(0);
        (kept, start)
    } else {
        let last = reels.len() - 1;
        (reels, start.min(last))
    };
    let picture = gtk::Picture::builder()
        .content_fit(gtk::ContentFit::Contain)
        .can_shrink(true)
        .build();
    let player = Player::get();
    let video = gtk::Picture::builder()
        .content_fit(gtk::ContentFit::Contain)
        .can_shrink(true)
        .build();
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
    let mute_button = gtk::Button::builder().css_classes(["flat", "circular"]).visible(false).build();
    video::show_muted(&mute_button, MUTED.get());
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
    let download_button = gtk::Button::builder()
        .icon_name("folder-download-symbolic")
        .tooltip_text("Download")
        .css_classes(["flat", "circular"])
        .build();
    info.append(&avatar_slot);
    info.append(&name);
    info.append(&time);
    let share_button = gtk::Button::builder()
        .icon_name("share-symbolic")
        .tooltip_text("Share This Story")
        .css_classes(["flat", "circular"])
        .build();
    info.append(&gtk::Box::builder().hexpand(true).build());
    info.append(&share_button);
    info.append(&download_button);
    info.append(&mute_button);
    info.append(&pause_button);
    info.append(&close);
    let details = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(2)
        .margin_start(40)
        .css_classes(["story-details"])
        .visible(false)
        .build();
    top.append(&progress);
    top.append(&info);
    top.append(&details);

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

    let reply = gtk::Entry::builder().hexpand(true).build();
    let like_button = gtk::Button::builder()
        .icon_name("heart-outline-symbolic")
        .css_classes(["flat", "circular"])
        .build();
    let bottom = gtk::Box::builder()
        .spacing(8)
        .valign(gtk::Align::End)
        .css_classes(["story-bottom"])
        .build();
    bottom.append(&reply);
    bottom.append(&like_button);
    let notice = gtk::Label::builder()
        .css_classes(["osd", "media-counter"])
        .halign(gtk::Align::Center)
        .valign(gtk::Align::End)
        .margin_bottom(128)
        .wrap(true)
        .visible(false)
        .build();
    let extras = gtk::Box::builder()
        .spacing(8)
        .halign(gtk::Align::Center)
        .valign(gtk::Align::End)
        .margin_bottom(72)
        .visible(false)
        .build();

    let overlay = gtk::Overlay::builder().child(&stack).css_classes(["story-viewer"]).build();
    overlay.add_overlay(&top);
    overlay.add_overlay(&prev);
    overlay.add_overlay(&next);
    overlay.add_overlay(&bottom);
    overlay.add_overlay(&extras);
    overlay.add_overlay(&notice);

    let stage = StoryStage::new(&overlay);
    // As big as the window: the story in the middle, people on the sides.
    let dialog = adw::Dialog::builder()
        .content_width(main.window.width().max(440))
        .content_height(main.window.height().max(600))
        .child(&stage)
        .title("Stories")
        .css_classes(["story-dialog"])
        .build();

    let viewer = Rc::new(Viewer {
        main: Rc::downgrade(main),
        me: main.client.user_id(),
        dialog: dialog.downgrade(),
        reels: RefCell::new(reels),
        reel: Cell::new(start),
        item: Cell::new(0),
        progress,
        bars: RefCell::new(Vec::new()),
        stack,
        picture,
        video,
        exclusive: RefCell::new(player.as_ref().map(|p| p.exclusive())),
        player,
        ticket: Cell::new(0),
        song_ticket: Cell::new(0),
        photo_seconds: Cell::new(PHOTO_SECONDS),
        stage,
        sides_for: Cell::new(None),
        requested: RefCell::new(HashSet::new()),
        avatar_slot,
        name,
        time,
        pause_button,
        mute_button,
        bottom,
        details,
        extras,
        download_button,
        share_button,
        reply,
        like_button,
        notice,
        notice_token: Cell::new(0),
        typing: Cell::new(false),
        paused_for_typing: Cell::new(false),
        paused: Cell::new(false),
        elapsed: Cell::new(0.0),
        ready: Cell::new(false),
        token: Cell::new(0),
    });

    let weak = Rc::downgrade(&viewer);
    let with = move |f: fn(&Rc<Viewer>)| {
        let weak = weak.clone();
        move || {
            if let Some(v) = weak.upgrade() {
                f(&v);
            }
        }
    };
    close.connect_clicked(glib::clone!(
        #[weak]
        dialog,
        move |_| {
            dialog.close();
        }
    ));
    let f = with(|v| v.previous());
    prev.connect_clicked(move |_| f());
    let f = with(|v| v.next());
    next.connect_clicked(move |_| f());
    let f = with(|v| v.toggle_pause());
    viewer.pause_button.connect_clicked(move |_| f());
    let f = with(|v| v.toggle_mute());
    viewer.mute_button.connect_clicked(move |_| f());
    let f = with(|v| v.toggle_like());
    viewer.like_button.connect_clicked(move |_| f());
    let f = with(|v| v.download());
    viewer.download_button.connect_clicked(move |_| f());
    let f = with(|v| v.share());
    viewer.share_button.connect_clicked(move |_| f());
    let f = with(|v| v.send_reply());
    viewer.reply.connect_activate(move |_| f());
    let weak = Rc::downgrade(&viewer);
    menu::attach(&overlay, move || weak.upgrade().map(|v| v.menu()));

    // Typing a reply pauses the story.
    let focus = gtk::EventControllerFocus::new();
    let f = with(|v| {
        v.typing.set(true);
        if !v.paused.get() {
            v.paused_for_typing.set(true);
            v.toggle_pause();
        }
    });
    focus.connect_enter(move |_| f());
    let f = with(|v| v.done_typing());
    focus.connect_leave(move |_| f());
    viewer.reply.add_controller(focus);

    // Before the focused button sees the key (Space would press it).
    let keys = gtk::EventControllerKey::builder()
        .propagation_phase(gtk::PropagationPhase::Capture)
        .build();
    let weak = Rc::downgrade(&viewer);
    keys.connect_key_pressed(move |_, key, _, _| {
        let Some(v) = weak.upgrade() else { return glib::Propagation::Proceed };
        if v.typing.get() {
            return glib::Propagation::Proceed;
        }
        match key {
            gdk::Key::Left => v.previous(),
            gdk::Key::Right => v.next(),
            gdk::Key::space => v.toggle_pause(),
            _ => return glib::Propagation::Proceed,
        }
        glib::Propagation::Stop
    });
    dialog.add_controller(keys);
    let weak = Rc::downgrade(&viewer);
    dialog.connect_closed(move |_| {
        if let Some(v) = weak.upgrade() {
            v.token.set(v.token.get() + 1);
            v.stop_media();
            v.exclusive.take();
        }
    });

    let weak = Rc::downgrade(&viewer);
    glib::timeout_add_local(Duration::from_millis(TICK_MS), move || match weak.upgrade() {
        Some(v) if v.dialog.upgrade().is_some() => {
            v.tick();
            glib::ControlFlow::Continue
        }
        _ => glib::ControlFlow::Break,
    });

    viewer.show(start, FIRST_UNSEEN);
    util::attach(&dialog, viewer);
    dialog.present(Some(&main.window));
}

/// Describe a person's stories once per run (counts only), to check that
/// what Instagram sends is what the viewer can show.
fn log_items(reel: &Reel, can_play: bool) {
    static LOGGED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if LOGGED.swap(true, std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    let items = &reel.items;
    let videos = items.iter().filter(|i| i.is_video()).count();
    let playable = items.iter().filter(|i| i.is_video() && i.video_url().is_some()).count();
    let images = items.iter().filter(|i| i.image_url(1080).is_some()).count();
    let unseen = items.iter().filter(|i| i.taken_at > reel.seen).count();
    eprintln!(
        "finsta: stories: {} items ({unseen} not watched yet), {videos} videos ({playable} with an address), {images} with an image; videos {}",
        items.len(),
        if can_play { "play" } else { "show their cover image" }
    );
}

fn show_liked(button: &gtk::Button, liked: bool) {
    button.set_icon_name(if liked { "heart-filled-symbolic" } else { "heart-outline-symbolic" });
    button.set_tooltip_text(Some(if liked { "Unlike" } else { "Like" }));
    if liked {
        button.add_css_class("story-liked");
    } else {
        button.remove_css_class("story-liked");
    }
}

impl Viewer {
    fn stop_media(&self) {
        for ticket in [self.ticket.replace(0), self.song_ticket.replace(0)] {
            if ticket != 0
                && let Some(player) = &self.player
            {
                player.release(ticket);
            }
        }
    }

    fn current(&self) -> Option<(Reel, Media)> {
        let reels = self.reels.borrow();
        let reel = reels.get(self.reel.get())?;
        let item = reel.items.get(self.item.get())?.clone();
        Some((reel.clone(), item))
    }

    fn show(self: &Rc<Self>, reel_index: usize, item_index: usize) {
        let token = self.token.get() + 1;
        self.token.set(token);
        self.stop_media();
        self.ready.set(false);
        self.elapsed.set(0.0);
        self.photo_seconds.set(PHOTO_SECONDS);
        self.reel.set(reel_index);
        self.item.set(0);

        let Some(reel) = self.reels.borrow().get(reel_index).cloned() else {
            self.close();
            return;
        };
        self.render_sides(false);
        util::clear_box(&self.avatar_slot);
        util::clear_box(&self.details);
        util::clear_box(&self.extras);
        self.details.set_visible(false);
        self.extras.set_visible(false);
        self.avatar_slot.append(&util::avatar(&reel.user, 32));
        self.name.set_label(&reel.user.username);
        self.bottom.set_visible(reel.user.pk != self.me);
        self.reply.set_placeholder_text(Some(&format!("Reply to {}…", reel.user.username)));
        self.mute_button.set_visible(false);

        if reel.items.is_empty() {
            self.stack.set_visible_child_name("loading");
            self.set_bars(1, 0);
            let Some(main) = self.main.upgrade() else { return };
            let client = main.client.clone();
            let id = reel.id.clone();
            // The people next to this one in the same request.
            let near = self.missing_near(reel_index);
            let weak = Rc::downgrade(self);
            glib::spawn_future_local(async move {
                let mut ids = vec![id.clone()];
                ids.extend(near.iter().map(|(_, id)| id.clone()));
                let res = rt::bg(async move { client.reels_media(&ids).await }).await;
                let Some(v) = weak.upgrade() else { return };
                if let Ok(map) = &res {
                    v.fill(map, &near);
                }
                if v.token.get() != token {
                    return;
                }
                match res {
                    Ok(mut map) => {
                        let fetched = map.remove(&id).unwrap_or_default();
                        if fetched.items.is_empty() {
                            // Expired or hidden; move on to the next person.
                            v.show_next_reel();
                            return;
                        }
                        if let Some(r) = v.reels.borrow_mut().get_mut(reel_index) {
                            r.items = fetched.items;
                            r.seen = r.seen.max(fetched.seen);
                            log_items(r, v.player.is_some());
                        }
                        v.render_sides(true);
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

        let item_index = if item_index == FIRST_UNSEEN {
            reel.items.iter().position(|i| i.taken_at > reel.seen).unwrap_or(0)
        } else {
            item_index.min(reel.items.len() - 1)
        };
        self.item.set(item_index);
        self.set_bars(reel.items.len(), item_index);
        let item = &reel.items[item_index];
        self.time.set_label(&util::relative_time(item.taken_at));
        show_liked(&self.like_button, item.has_liked);
        self.render_details(item);
        self.mark_seen(reel_index, item_index);
        self.prefetch(reel_index);

        let video = item.video_url().filter(|_| item.is_video());
        if let (Some(url), Some(player)) = (&video, &self.player) {
            self.video.set_paintable(gdk::Paintable::NONE);
            let opts = Opts { muted: MUTED.get(), looping: false, playing: !self.paused.get() };
            self.ticket.set(player.play(Some(&self.video), url, opts));
            self.mute_button.set_visible(true);
            self.stack.set_visible_child_name("video");
            self.ready.set(true);
            // The cover image, until the first frame arrives.
            let url = item.image_url(1080);
            let weak = Rc::downgrade(self);
            glib::spawn_future_local(async move {
                let Some(tex) = (match url {
                    Some(u) => images::texture(&u).await,
                    None => None,
                }) else {
                    return;
                };
                let Some(v) = weak.upgrade() else { return };
                if v.token.get() == token && v.video.paintable().is_none() {
                    v.video.set_paintable(Some(&tex));
                }
            });
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
                        v.play_song(token);
                    }
                    None => {
                        v.stack.set_visible_child_name("error");
                        v.ready.set(true);
                    }
                }
            });
        }
    }

    /// Remember (here, in the tray, and on Instagram) that this story was
    /// watched, so it isn't shown as new again.
    fn mark_seen(self: &Rc<Self>, reel_index: usize, item_index: usize) {
        let (reel, item) = {
            let mut reels = self.reels.borrow_mut();
            let Some(r) = reels.get_mut(reel_index) else { return };
            let Some(item) = r.items.get(item_index).cloned() else { return };
            let new = item.taken_at > r.seen;
            if new {
                r.seen = item.taken_at;
            }
            (r.clone(), new.then_some(item))
        };
        let Some(main) = self.main.upgrade() else { return };
        if let Some(home) = main.home.get() {
            home.reel_viewed(&reel);
        }
        let Some(item) = item else { return };
        let client = main.client.clone();
        glib::spawn_future_local(async move {
            let owner = reel.user.pk.clone();
            let res = rt::bg(async move { client.mark_story_seen(&reel.id, &owner, &item).await }).await;
            if let Err(e) = res {
                eprintln!("finsta: couldn't mark a story as watched: {e}");
            }
        });
    }

    fn set_item_liked(&self, reel_index: usize, item_index: usize, liked: bool) {
        if let Some(item) = self.reels.borrow_mut().get_mut(reel_index).and_then(|r| r.items.get_mut(item_index)) {
            item.has_liked = liked;
        }
        if self.reel.get() == reel_index && self.item.get() == item_index {
            show_liked(&self.like_button, liked);
        }
    }

    fn toggle_like(self: &Rc<Self>) {
        let Some((_, item)) = self.current() else { return };
        let (reel_index, item_index) = (self.reel.get(), self.item.get());
        let like = !item.has_liked;
        self.set_item_liked(reel_index, item_index, like);
        let Some(main) = self.main.upgrade() else { return };
        let client = main.client.clone();
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let res = rt::bg(async move { client.story_like(&item, like).await }).await;
            let Some(v) = weak.upgrade() else { return };
            if let Err(e) = res {
                eprintln!("finsta: story like failed: {e}");
                v.set_item_liked(reel_index, item_index, !like);
                v.show_notice(if like { "Couldn't like this story" } else { "Couldn't unlike this story" });
                if crate::api::is_login_required(&e)
                    && let Some(main) = v.main.upgrade()
                {
                    main.handle_error(&e);
                }
            }
        });
    }

    fn send_reply(self: &Rc<Self>) {
        let text = self.reply.text().trim().to_string();
        let Some((reel, item)) = self.current() else { return };
        if text.is_empty() {
            return;
        }
        let Some(main) = self.main.upgrade() else { return };
        self.reply.set_sensitive(false);
        let client = main.client.clone();
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let res = rt::bg(async move { client.story_reply(&reel.user, &item.id, &text).await }).await;
            let Some(v) = weak.upgrade() else { return };
            v.reply.set_sensitive(true);
            match res {
                Ok(how) => {
                    v.reply.set_text("");
                    v.show_notice(match how {
                        StoryReply::Reply => "Reply sent",
                        StoryReply::Message => "Sent as a message with a link to the story",
                    });
                    // Leave the reply box and play on.
                    v.done_typing();
                    v.pause_button.grab_focus();
                }
                Err(e) => {
                    eprintln!("finsta: story reply failed: {e}");
                    v.show_notice("Couldn't send your reply");
                    if crate::api::is_login_required(&e)
                        && let Some(main) = v.main.upgrade()
                    {
                        main.handle_error(&e);
                    }
                }
            }
        });
    }

    /// Music, who the story was reshared from, mentions, and buttons for a
    /// shared post or a link.
    fn render_details(self: &Rc<Self>, item: &Media) {
        util::clear_box(&self.details);
        util::clear_box(&self.extras);
        if let Some(song) = item.song()
            && let Some(main) = self.main.upgrade()
        {
            let from = self.current().map(|(reel, item)| story_url(&reel, &item));
            self.details.append(&post::music_label(&main, &song, from, Some(self.notifier())));
        }
        let open_profile = |label: String, user: User| {
            let b = gtk::Button::builder()
                .label(label)
                .css_classes(["flat", "story-link"])
                .halign(gtk::Align::Start)
                .build();
            let weak = Rc::downgrade(self);
            b.connect_clicked(move |_| {
                if let Some(v) = weak.upgrade()
                    && let Some(main) = v.main.upgrade()
                {
                    v.close();
                    main.open_profile(user.clone());
                }
            });
            b
        };
        if let Some(author) = item.reshared_from() {
            self.details.append(&open_profile(format!("Reshared from @{}", author.username), author));
        }
        let mentions = item.mentions();
        if !mentions.is_empty() {
            let row = gtk::Box::builder().spacing(2).build();
            row.append(&gtk::Label::builder().label("Mentions").margin_start(6).css_classes(["caption"]).build());
            for name in mentions.into_iter().take(4) {
                let user = User { username: name.clone(), ..Default::default() };
                row.append(&open_profile(format!("@{name}"), user));
            }
            self.details.append(&row);
        }
        if let Some(code) = item.shared_post_code() {
            let (label, path) = if item.shared_post_is_reel() { ("View Reel", "reel") } else { ("View Post", "p") };
            let url = format!("https://www.instagram.com/{path}/{code}/");
            let b = gtk::Button::builder().label(label).css_classes(["pill", "osd"]).build();
            let weak = Rc::downgrade(self);
            b.connect_clicked(move |_| {
                if let Some(v) = weak.upgrade()
                    && let Some(main) = v.main.upgrade()
                {
                    v.close();
                    main.open_link(&url);
                }
            });
            self.extras.append(&b);
        }
        if let Some(link) = item.story_link() {
            let host = link
                .split("://")
                .nth(1)
                .and_then(|r| r.split('/').next())
                .unwrap_or("link")
                .trim_start_matches("www.")
                .to_string();
            let b = gtk::Button::builder()
                .label(format!("Open {host}"))
                .tooltip_text(&link)
                .css_classes(["pill", "osd"])
                .build();
            let weak = Rc::downgrade(self);
            b.connect_clicked(move |b| {
                if let Some(v) = weak.upgrade()
                    && !v.paused.get()
                {
                    v.toggle_pause();
                }
                util::open_uri(b, &link);
            });
            self.extras.append(&b);
        }
        self.details.set_visible(self.details.first_child().is_some());
        self.extras.set_visible(self.extras.first_child().is_some());
    }

    /// Report something in the viewer (Finsta's toasts are behind it).
    fn notifier(self: &Rc<Self>) -> share::Notify {
        let weak = Rc::downgrade(self);
        Rc::new(move |text: &str| {
            if let Some(v) = weak.upgrade() {
                v.show_notice(text);
            }
        })
    }

    fn share(self: &Rc<Self>) {
        let Some((reel, item)) = self.current() else { return };
        let Some(main) = self.main.upgrade() else { return };
        // Hold the story while the share sheet is open.
        let paused_here = !self.paused.get();
        if paused_here {
            self.toggle_pause();
        }
        let weak = Rc::downgrade(self);
        let resume: Box<dyn Fn()> = Box::new(move || {
            if paused_here
                && let Some(v) = weak.upgrade()
                && v.paused.get()
            {
                v.toggle_pause();
            }
        });
        let shared = share::Shared { url: story_url(&reel, &item), kind: share::Kind::Story };
        share::open(&main, shared, Some(self.notifier()), Some(resume));
    }

    fn download(self: &Rc<Self>) {
        let Some((reel, item)) = self.current() else { return };
        let Some(main) = self.main.upgrade() else { return };
        self.download_button.set_sensitive(false);
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let res = download::fetch(&main, &item, &reel.user.username).await;
            let Some(v) = weak.upgrade() else { return };
            v.download_button.set_sensitive(true);
            match res {
                Ok(saved) if saved.len() == 1 => v.show_notice("Saved to Downloads/Finsta"),
                Ok(saved) => v.show_notice(&format!("Saved {} files to Downloads/Finsta", saved.len())),
                Err(e) => {
                    eprintln!("finsta: story download failed: {e}");
                    v.show_notice("Couldn't download this story");
                }
            }
        });
    }

    fn done_typing(self: &Rc<Self>) {
        self.typing.set(false);
        if self.paused_for_typing.replace(false) && self.paused.get() {
            self.toggle_pause();
        }
    }

    fn show_notice(self: &Rc<Self>, text: &str) {
        let token = self.notice_token.get() + 1;
        self.notice_token.set(token);
        self.notice.set_label(text);
        self.notice.set_visible(true);
        let weak = Rc::downgrade(self);
        glib::timeout_add_local_once(Duration::from_millis(2500), move || {
            if let Some(v) = weak.upgrade()
                && v.notice_token.get() == token
            {
                v.notice.set_visible(false);
            }
        });
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
        let ticket = self.ticket.get();
        let fraction = match self.player.as_ref().filter(|_| ticket != 0) {
            // Unplayable (expired, or a format that isn't supported): move on.
            Some(player) if player.failed(ticket) || !player.owns(ticket) => 1.0,
            Some(player) => player.progress(ticket).unwrap_or(0.0),
            None => {
                self.elapsed.set(self.elapsed.get() + TICK_MS as f64 / 1000.0);
                self.elapsed.get() / self.photo_seconds.get()
            }
        };
        if let Some(bar) = self.bars.borrow().get(self.item.get()) {
            bar.set_fraction(fraction.clamp(0.0, 1.0));
        }
        if fraction >= 1.0 {
            self.next();
        }
    }

    fn toggle_pause(self: &Rc<Self>) {
        let paused = !self.paused.get();
        self.paused.set(paused);
        if let Some(player) = &self.player {
            player.set_playing(self.ticket.get(), !paused);
            player.set_playing(self.song_ticket.get(), !paused);
        }
        self.pause_button.set_icon_name(if paused {
            "media-playback-start-symbolic"
        } else {
            "media-playback-pause-symbolic"
        });
        self.pause_button
            .set_tooltip_text(Some(if paused { "Play" } else { "Pause" }));
    }

    fn toggle_mute(self: &Rc<Self>) {
        let muted = !MUTED.get();
        MUTED.set(muted);
        if let Some(player) = &self.player {
            player.set_muted(self.ticket.get(), muted);
            player.set_muted(self.song_ticket.get(), muted);
        }
        video::show_muted(&self.mute_button, muted);
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
            self.show(self.reel.get() + 1, FIRST_UNSEEN);
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

    /// Draw the cards of the people before and after the current one (only
    /// when that person changed, unless `force`).
    fn render_sides(self: &Rc<Self>, force: bool) {
        let current = self.reel.get();
        if !force && self.sides_for.get() == Some(current) {
            return;
        }
        self.sides_for.set(Some(current));
        let reels = self.reels.borrow();
        let mut cards = Vec::new();
        for slot in [-2i32, -1, 1, 2] {
            let Some(index) = current.checked_add_signed(slot as isize).filter(|i| *i < reels.len()) else {
                continue;
            };
            let card = side_card(&reels[index], &self.me);
            let weak = Rc::downgrade(self);
            card.connect_clicked(move |_| {
                if let Some(v) = weak.upgrade() {
                    v.show(index, FIRST_UNSEEN);
                }
            });
            cards.push((card.upcast(), slot));
        }
        drop(reels);
        self.stage.set_cards(cards);
    }

    /// The people next to `index` whose stories aren't loaded or asked for
    /// yet (now marked as asked for).
    fn missing_near(&self, index: usize) -> Vec<(usize, String)> {
        let reels = self.reels.borrow();
        let mut requested = self.requested.borrow_mut();
        let last = reels.len().saturating_sub(1);
        (index.saturating_sub(2)..=(index + 2).min(last))
            .filter(|i| *i != index)
            .filter_map(|i| {
                let r = &reels[i];
                (r.items.is_empty() && requested.insert(r.id.clone())).then(|| (i, r.id.clone()))
            })
            .collect()
    }

    /// Take in the stories fetched for `wanted` (index, id).
    fn fill(&self, fetched: &HashMap<String, Reel>, wanted: &[(usize, String)]) {
        let mut reels = self.reels.borrow_mut();
        for (i, id) in wanted {
            if let (Some(f), Some(r)) = (fetched.get(id), reels.get_mut(*i))
                && r.id == *id
                && r.items.is_empty()
            {
                r.items = f.items.clone();
                r.seen = r.seen.max(f.seen);
            }
        }
    }

    /// Load the stories of the people beside `index`, for their cards and
    /// so moving on to them is quick (one request).
    fn prefetch(self: &Rc<Self>, index: usize) {
        let near = self.missing_near(index);
        if near.is_empty() {
            return;
        }
        let Some(main) = self.main.upgrade() else { return };
        let client = main.client.clone();
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let ids: Vec<String> = near.iter().map(|(_, id)| id.clone()).collect();
            let res = rt::bg(async move { client.reels_media(&ids).await }).await;
            let Some(v) = weak.upgrade() else { return };
            match res {
                Ok(map) => {
                    v.fill(&map, &near);
                    v.render_sides(true);
                }
                Err(e) => {
                    let mut requested = v.requested.borrow_mut();
                    for (_, id) in &near {
                        requested.remove(id);
                    }
                    eprintln!("finsta: stories: couldn't load the people nearby: {e}");
                }
            }
        });
    }

    /// A photo story with music plays its part of the song.
    fn play_song(self: &Rc<Self>, token: u64) {
        let Some((_, item)) = self.current() else { return };
        if item.is_video() || item.song().is_none() || self.player.is_none() {
            return;
        }
        let Some(main) = self.main.upgrade() else { return };
        let client = main.client.clone();
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let res = rt::bg(async move { client.song_clip(&item).await }).await;
            let Some(v) = weak.upgrade() else { return };
            if v.token.get() != token {
                return;
            }
            match res {
                Ok(Some(clip)) => {
                    let Some(player) = &v.player else { return };
                    let opts = Opts { muted: MUTED.get(), looping: true, playing: !v.paused.get() };
                    let seg = post::segment(clip.start_ms, clip.duration_ms);
                    v.song_ticket.set(player.play_part(None, &clip.url, opts, Some(seg)));
                    v.mute_button.set_visible(true);
                    if let Some(ms) = clip.duration_ms {
                        v.photo_seconds.set((ms as f64 / 1000.0).clamp(PHOTO_SECONDS, SONG_SECONDS));
                    }
                }
                Ok(None) => {}
                Err(e) => eprintln!("finsta: music: couldn't load a story's song: {e}"),
            }
        });
    }

    /// The right-click menu of the story on screen. The story waits while
    /// it's open.
    fn menu(self: &Rc<Self>) -> Menu {
        let mut m = Menu::new();
        let Some((reel, item)) = self.current() else { return m };
        let paused_here = !self.paused.get();
        if paused_here {
            self.toggle_pause();
        }
        let weak = Rc::downgrade(self);
        let w = weak.clone();
        m.on_closed(move || {
            if paused_here
                && let Some(v) = w.upgrade()
                && v.paused.get()
            {
                v.toggle_pause();
            }
        });
        let action = |f: fn(&Rc<Viewer>)| {
            let weak = weak.clone();
            move || {
                if let Some(v) = weak.upgrade() {
                    f(&v);
                }
            }
        };
        m.item(if paused_here { "Pause" } else { "Play" }, action(|v| v.toggle_pause()));
        if self.mute_button.is_visible() {
            m.item(if MUTED.get() { "Unmute" } else { "Mute" }, action(|v| v.toggle_mute()));
        }
        if reel.user.pk != self.me {
            m.item(if item.has_liked { "Unlike" } else { "Like" }, action(|v| v.toggle_like()));
        }
        m.section();
        m.item("Share…", action(|v| v.share()));
        let url = story_url(&reel, &item);
        let (w, u) = (weak.clone(), url.clone());
        m.item("Copy Link", move || {
            if let Some(v) = w.upgrade() {
                v.notice.clipboard().set_text(&u);
                v.show_notice("Link copied");
            }
        });
        m.item("Download", action(|v| v.download()));
        m.section();
        let user = reel.user.clone();
        let name = format!("@{}", user.username);
        let (w, u) = (weak.clone(), user.clone());
        m.item(format!("View {name}'s Profile"), move || {
            if let Some(v) = w.upgrade()
                && let Some(main) = v.main.upgrade()
            {
                v.close();
                main.open_profile(u.clone());
            }
        });
        if user.pk != self.me {
            let (w, u) = (weak.clone(), user.clone());
            m.item(format!("Message {name}"), move || {
                if let Some(v) = w.upgrade()
                    && let Some(main) = v.main.upgrade()
                {
                    v.close();
                    main.open_chat_with_user(u.clone());
                }
            });
        }
        let w = weak.clone();
        m.item("Open on instagram.com", move || {
            if let Some(v) = w.upgrade() {
                util::open_uri(&v.notice, &url);
            }
        });
        m
    }

    fn close(&self) {
        self.token.set(self.token.get() + 1);
        self.stop_media();
        self.exclusive.take();
        if let Some(d) = self.dialog.upgrade() {
            d.close();
        }
    }
}

/// A story's address on instagram.com.
fn story_url(reel: &Reel, item: &Media) -> String {
    let pk = item.id.split('_').next().unwrap_or(&item.id);
    format!("https://www.instagram.com/stories/{}/{pk}/", reel.user.username)
}

/// The card for someone before or after the story on screen: their next
/// story, dimmed, with their picture and name on top.
fn side_card(reel: &Reel, me: &str) -> gtk::Button {
    let picture = gtk::Picture::builder()
        .content_fit(gtk::ContentFit::Cover)
        .can_shrink(true)
        .build();
    let preview = reel.items.iter().find(|i| i.taken_at > reel.seen).or(reel.items.first());
    if let Some(item) = preview {
        images::load_picture(&picture, item.image_url(480));
    }
    let overlay = gtk::Overlay::builder().child(&picture).build();
    overlay.add_overlay(&gtk::Box::builder().css_classes(["story-card-shade"]).build());
    let info = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(6)
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .margin_start(8)
        .margin_end(8)
        .build();
    info.append(&util::avatar(&reel.user, 52));
    let name = if reel.user.pk == me { "Your story".to_string() } else { reel.user.username.clone() };
    info.append(
        &gtk::Label::builder()
            .label(&name)
            .css_classes(["heading"])
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .max_width_chars(14)
            .build(),
    );
    let newest = reel.items.iter().map(|i| i.taken_at).max().unwrap_or(reel.latest_reel_media);
    if newest > 0 {
        info.append(&gtk::Label::builder().label(util::relative_time(newest)).css_classes(["caption"]).build());
    }
    overlay.add_overlay(&info);
    let button = gtk::Button::builder()
        .child(&overlay)
        .css_classes(["story-card"])
        .overflow(gtk::Overflow::Hidden)
        .tooltip_text(format!("{name}'s stories"))
        .build();
    if reel.has_unseen() {
        button.add_css_class("unseen");
    }
    button
}
