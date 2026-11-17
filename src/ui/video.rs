//! Video and voice playback for the whole app.
//!
//! Two problems shaped this:
//! - GTK's own player (`gtk::MediaFile`) builds a GStreamer pipeline per
//!   video, and tearing one down soon after it started playing can crash the
//!   app (seen with GTK 4.24 and GStreamer 1.28).
//! - GStreamer's GTK video sinks (GTK's own and gst-plugin-gtk4's) share
//!   GTK's GL context with GStreamer's threads. In Finsta's runs, after a
//!   video had played, GTK could no longer create or use GL contexts.
//!
//! So there is one pipeline (playbin) for the whole app, and it is never torn
//! down. It decodes to plain RGBA frames, which GTK draws like any other
//! image: GStreamer never touches GTK's GL. Only one video or voice message
//! plays at a time; whoever starts one takes the player from the last user.

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::rc::{Rc, Weak};
use std::time::Duration;

use gstreamer as gst;
use gstreamer::prelude::*;
use adw::prelude::*;
use gtk::{gdk, glib};

#[derive(Clone, Copy)]
pub struct Opts {
    pub muted: bool,
    pub looping: bool,
    pub playing: bool,
}

struct Target {
    ticket: u64,
    picture: Option<glib::WeakRef<gtk::Picture>>,
    /// What the picture showed before, put back when the video stops.
    cover: Option<gdk::Paintable>,
    tick: Option<gtk::TickCallbackId>,
    on_lost: Option<Box<dyn Fn()>>,
    on_end: Option<Box<dyn Fn()>>,
    frames: Rc<Cell<u32>>,
    started: std::time::Instant,
}

pub struct Player {
    playbin: gst::Element,
    sink: gst::Element,
    target: RefCell<Option<Target>>,
    tickets: Cell<u64>,
    looping: Cell<bool>,
    /// A loop's seek back to the start is under way.
    loop_seek: Cell<bool>,
    /// The server can't seek: loop by starting over instead.
    loop_restart: Cell<bool>,
    ended: Cell<bool>,
    failed: Cell<bool>,
    has_frame: Rc<Cell<bool>>,
    exclusive: Cell<u32>,
    watch: RefCell<Option<gst::bus::BusWatchGuard>>,
}

thread_local! {
    static PLAYER: RefCell<Option<Option<Rc<Player>>>> = const { RefCell::new(None) };
}

impl Player {
    /// The app's player; None if GStreamer can't be used. Main thread only.
    pub fn get() -> Option<Rc<Player>> {
        PLAYER.with(|p| {
            p.borrow_mut()
                .get_or_insert_with(|| match Self::new() {
                    Ok(player) => Some(player),
                    Err(e) => {
                        eprintln!("finsta: videos can't play: {e}");
                        None
                    }
                })
                .clone()
        })
    }

    fn new() -> Result<Rc<Self>, String> {
        gst::init().map_err(|e| format!("GStreamer: {e}"))?;
        let bin = gst::parse::bin_from_description(
            "videoconvert name=convert ! video/x-raw,format=RGBA ! appsink name=sink max-buffers=1 drop=true enable-last-sample=false",
            true,
        )
        .map_err(|_| "GStreamer's videoconvert or appsink (gst-plugins-base) is missing".to_string())?;
        if let Some(convert) = bin.by_name("convert")
            && convert.find_property("n-threads").is_some()
        {
            convert.set_property("n-threads", 0u32);
        }
        let sink = bin.by_name("sink").ok_or("no appsink")?;
        let playbin = gst::ElementFactory::make("playbin")
            .property("video-sink", &bin)
            .build()
            .map_err(|_| "GStreamer's playbin (gst-plugins-base) is missing".to_string())?;
        let player = Rc::new(Player {
            playbin,
            sink,
            target: RefCell::new(None),
            tickets: Cell::new(0),
            looping: Cell::new(false),
            loop_seek: Cell::new(false),
            loop_restart: Cell::new(false),
            ended: Cell::new(false),
            failed: Cell::new(false),
            has_frame: Rc::new(Cell::new(false)),
            exclusive: Cell::new(0),
            watch: RefCell::new(None),
        });
        let bus = player.playbin.bus().ok_or("no bus")?;
        let weak = Rc::downgrade(&player);
        let watch = bus
            .add_watch_local(move |_, msg| {
                if let Some(p) = weak.upgrade() {
                    p.on_message(msg);
                }
                glib::ControlFlow::Continue
            })
            .map_err(|e| format!("bus: {e}"))?;
        player.watch.replace(Some(watch));
        Ok(player)
    }

    fn on_message(&self, msg: &gst::Message) {
        match msg.view() {
            gst::MessageView::Eos(_) => {
                if self.looping.get() && self.loop_restart.get() {
                    self.restart();
                } else if self.looping.get() {
                    self.loop_seek.set(true);
                    if self
                        .playbin
                        .seek_simple(gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT, gst::ClockTime::ZERO)
                        .is_err()
                    {
                        self.loop_restart.set(true);
                        self.restart();
                    }
                } else {
                    self.ended.set(true);
                    let cb = self.target.borrow_mut().as_mut().and_then(|t| t.on_end.take());
                    if let Some(cb) = cb {
                        cb();
                    }
                }
            }
            gst::MessageView::Error(_) if self.loop_seek.replace(false) => {
                // The server couldn't jump back to the start.
                self.loop_restart.set(true);
                self.restart();
            }
            gst::MessageView::Error(e) => {
                // Only the kind of error: messages can contain addresses.
                let err = e.error();
                let kind = if let Some(k) = err.kind::<gst::ResourceError>() {
                    format!("resource {k:?}")
                } else if let Some(k) = err.kind::<gst::StreamError>() {
                    format!("stream {k:?}")
                } else if let Some(k) = err.kind::<gst::CoreError>() {
                    format!("core {k:?}")
                } else {
                    "other".to_string()
                };
                eprintln!("finsta: video failed ({kind})");
                self.failed.set(true);
            }
            _ => {}
        }
    }

    /// Play the current video again from the start (keeping its last frame
    /// on screen meanwhile).
    fn restart(&self) {
        let _ = self.playbin.set_state(gst::State::Ready);
        if let Some(bus) = self.playbin.bus() {
            while bus.pop().is_some() {}
        }
        let _ = self.playbin.set_state(gst::State::Playing);
    }

    /// Play `url`, showing it in `picture` (None: sound only). Whoever had
    /// the player loses it. Returns a ticket for the other calls.
    pub fn play(&self, picture: Option<&gtk::Picture>, url: &str, opts: Opts) -> u64 {
        self.detach(true);
        let ticket = self.tickets.get() + 1;
        self.tickets.set(ticket);
        self.ended.set(false);
        self.failed.set(false);
        self.has_frame.set(false);
        self.looping.set(opts.looping);
        self.loop_seek.set(false);
        self.loop_restart.set(false);
        let _ = self.playbin.set_state(gst::State::Ready);
        // Anything still queued is about the previous video.
        if let Some(bus) = self.playbin.bus() {
            while bus.pop().is_some() {}
        }
        self.playbin.set_property("uri", url);
        self.playbin.set_property("mute", opts.muted);
        let state = if opts.playing { gst::State::Playing } else { gst::State::Paused };
        if self.playbin.set_state(state).is_err() {
            self.failed.set(true);
        }
        let frames = Rc::new(Cell::new(0u32));
        let tick = picture.map(|p| {
            let sink = self.sink.clone();
            let has_frame = self.has_frame.clone();
            let frames = frames.clone();
            p.add_tick_callback(move |picture, _| {
                if let Some(texture) = next_frame(&sink, !has_frame.get()) {
                    has_frame.set(true);
                    frames.set(frames.get() + 1);
                    picture.set_paintable(Some(&texture));
                }
                glib::ControlFlow::Continue
            })
        });
        self.target.replace(Some(Target {
            ticket,
            picture: picture.map(|p| p.downgrade()),
            cover: picture.and_then(|p| p.paintable()),
            tick,
            on_lost: None,
            on_end: None,
            frames,
            started: std::time::Instant::now(),
        }));
        ticket
    }

    /// Called when another video takes the player away from `ticket`.
    pub fn on_lost(&self, ticket: u64, f: impl Fn() + 'static) {
        if let Some(t) = self.target.borrow_mut().as_mut().filter(|t| t.ticket == ticket) {
            t.on_lost = Some(Box::new(f));
        }
    }

    /// Called once when `ticket`'s video plays to the end (not when looping).
    pub fn on_end(&self, ticket: u64, f: impl Fn() + 'static) {
        if let Some(t) = self.target.borrow_mut().as_mut().filter(|t| t.ticket == ticket) {
            t.on_end = Some(Box::new(f));
        }
    }

    fn detach(&self, notify: bool) {
        let Some(t) = self.target.take() else { return };
        // How smoothly the first few videos played (counts only).
        static LOGGED: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let secs = t.started.elapsed().as_secs_f64();
        if t.tick.is_some() && secs > 1.0 && LOGGED.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 3 {
            let frames = t.frames.get();
            eprintln!(
                "finsta: video: {frames} frames shown in {secs:.1}s ({:.0} per second){}",
                f64::from(frames) / secs,
                if self.failed.get() { ", failed" } else { "" }
            );
        }
        if let Some(tick) = t.tick {
            tick.remove();
        }
        if let Some(p) = t.picture.and_then(|w| w.upgrade()) {
            p.set_paintable(t.cover.as_ref());
        }
        if notify && let Some(f) = t.on_lost {
            f();
        }
    }

    pub fn owns(&self, ticket: u64) -> bool {
        self.target.borrow().as_ref().is_some_and(|t| t.ticket == ticket)
    }

    /// Stop `ticket`'s video, if it still has the player.
    pub fn release(&self, ticket: u64) {
        if self.owns(ticket) {
            self.detach(false);
            let _ = self.playbin.set_state(gst::State::Ready);
        }
    }

    pub fn set_playing(&self, ticket: u64, playing: bool) {
        if self.owns(ticket) {
            let _ = self.playbin.set_state(if playing { gst::State::Playing } else { gst::State::Paused });
        }
    }

    pub fn set_muted(&self, ticket: u64, muted: bool) {
        if self.owns(ticket) {
            self.playbin.set_property("mute", muted);
        }
    }

    /// How far `ticket`'s video has played, from 0 to 1 (None until known).
    pub fn progress(&self, ticket: u64) -> Option<f64> {
        if !self.owns(ticket) {
            return None;
        }
        if self.ended.get() {
            return Some(1.0);
        }
        let position = self.playbin.query_position::<gst::ClockTime>()?;
        let duration = self.playbin.query_duration::<gst::ClockTime>()?;
        (duration.nseconds() > 0).then(|| position.nseconds() as f64 / duration.nseconds() as f64)
    }

    pub fn failed(&self, ticket: u64) -> bool {
        self.owns(ticket) && self.failed.get()
    }

    /// Nothing is playing (or what was playing has ended or failed).
    fn is_idle(&self) -> bool {
        self.target.borrow().is_none() || self.ended.get() || self.failed.get()
    }

    /// Keep the feed from taking the player while a viewer is open.
    pub fn exclusive(self: &Rc<Self>) -> Exclusive {
        self.exclusive.set(self.exclusive.get() + 1);
        Exclusive(self.clone())
    }
}

/// While alive, feed videos don't start playing.
pub struct Exclusive(Rc<Player>);

impl Drop for Exclusive {
    fn drop(&mut self) {
        self.0.exclusive.set(self.0.exclusive.get().saturating_sub(1));
    }
}

/// The newest decoded frame, as a texture (None if there's no new one).
fn next_frame(sink: &gst::Element, first: bool) -> Option<gdk::Texture> {
    let sample = sink.emit_by_name::<Option<gst::Sample>>("try-pull-sample", &[&0u64]);
    // A paused video's first frame only arrives as the "preroll".
    let sample = match sample {
        Some(s) => s,
        None if first => sink.emit_by_name::<Option<gst::Sample>>("try-pull-preroll", &[&0u64])?,
        None => return None,
    };
    let caps = sample.caps()?;
    let s = caps.structure(0)?;
    let width = s.get::<i32>("width").ok()?;
    let height = s.get::<i32>("height").ok()?;
    let stride = width as usize * 4;
    let buffer = sample.buffer()?;
    let map = buffer.map_readable().ok()?;
    if width <= 0 || height <= 0 || map.len() < stride * height as usize {
        return None;
    }
    let bytes = glib::Bytes::from(&map[..stride * height as usize]);
    Some(gdk::MemoryTexture::new(width, height, gdk::MemoryFormat::R8g8b8a8Premultiplied, &bytes, stride).upcast())
}

// ----- Autoplay in the feed and on posts --------------------------------------

struct Slot {
    id: u64,
    picture: glib::WeakRef<gtk::Picture>,
    url: String,
}

thread_local! {
    static SLOTS: RefCell<Vec<Slot>> = const { RefCell::new(Vec::new()) };
    static NEXT_SLOT: Cell<u64> = const { Cell::new(0) };
    /// The slot playing now, and its ticket.
    static CURRENT: Cell<Option<(u64, u64)>> = const { Cell::new(None) };
    static FAILED: RefCell<HashSet<String>> = RefCell::new(HashSet::new());
    static POLLING: Cell<bool> = const { Cell::new(false) };
    static MUTED: Cell<bool> = const { Cell::new(true) };
    static MUTE_BUTTONS: RefCell<Vec<glib::WeakRef<gtk::Button>>> = const { RefCell::new(Vec::new()) };
}

/// Starts playing (muted, looping) when most of it is on screen, and stops
/// when it scrolls away. Only the most visible video plays.
pub fn autoplay(picture: &gtk::Picture, url: String) {
    let id = NEXT_SLOT.with(|n| {
        n.set(n.get() + 1);
        n.get()
    });
    SLOTS.with(|s| s.borrow_mut().push(Slot { id, picture: picture.downgrade(), url }));
    if !POLLING.with(|p| p.replace(true)) {
        glib::timeout_add_local(Duration::from_millis(250), || {
            poll();
            glib::ControlFlow::Continue
        });
    }
    picture.connect_map(|_| poll());
}

fn poll() {
    SLOTS.with(|s| s.borrow_mut().retain(|x| x.picture.upgrade().is_some()));
    let Some(player) = Player::get() else { return };
    let mut current = CURRENT.get().filter(|(_, t)| player.owns(*t));
    if let Some((slot, ticket)) = current
        && player.failed(ticket)
    {
        if let Some(url) = SLOTS.with(|s| s.borrow().iter().find(|x| x.id == slot).map(|x| x.url.clone())) {
            FAILED.with(|f| f.borrow_mut().insert(url));
        }
        player.release(ticket);
        current = None;
    }
    CURRENT.set(current);
    if player.exclusive.get() > 0 {
        return;
    }
    let visible: Vec<(u64, gtk::Picture, String, f32)> = SLOTS.with(|s| {
        s.borrow()
            .iter()
            .filter_map(|x| Some((x.id, x.picture.upgrade()?, x.url.clone())))
            .filter(|(_, _, url)| !FAILED.with(|f| f.borrow().contains(url)))
            .map(|(id, p, url)| {
                let f = visible_fraction(&p);
                (id, p, url, f)
            })
            .collect()
    });
    if let Some((slot, _)) = current
        && visible.iter().any(|(id, _, _, f)| *id == slot && *f >= 0.35)
    {
        return;
    }
    let best = visible
        .into_iter()
        .filter(|(_, _, _, f)| *f >= 0.6)
        .max_by(|a, b| a.3.total_cmp(&b.3));
    match best {
        Some((id, picture, url, _)) if current.is_some() || player.is_idle() => {
            let opts = Opts { muted: MUTED.get(), looping: true, playing: true };
            let ticket = player.play(Some(&picture), &url, opts);
            CURRENT.set(Some((id, ticket)));
        }
        Some(_) => {}
        None => {
            if let Some((_, ticket)) = current {
                player.release(ticket);
                CURRENT.set(None);
            }
        }
    }
}

/// How much of `p` is on screen, from 0 to 1.
fn visible_fraction(p: &gtk::Picture) -> f32 {
    if !p.is_mapped() || p.height() <= 0 {
        return 0.0;
    }
    let Some(root) = p.root() else { return 0.0 };
    let root = root.upcast_ref::<gtk::Widget>();
    if root.downcast_ref::<gtk::Window>().is_some_and(|w| w.is_suspended() || !w.is_visible()) {
        return 0.0;
    }
    let Some(b) = p.compute_bounds(root) else { return 0.0 };
    let (mut top, mut bottom) = (0.0f32, root.height() as f32);
    let mut ancestor = p.parent();
    while let Some(w) = ancestor {
        if w.is::<gtk::ScrolledWindow>()
            && let Some(sb) = w.compute_bounds(root)
        {
            top = top.max(sb.y());
            bottom = bottom.min(sb.y() + sb.height());
        }
        ancestor = w.parent();
    }
    let visible = (b.y() + b.height()).min(bottom) - b.y().max(top);
    (visible / b.height()).clamp(0.0, 1.0)
}

/// The sound button on autoplaying videos (all of them share one setting).
pub fn mute_button() -> gtk::Button {
    let button = gtk::Button::builder()
        .css_classes(["osd", "circular"])
        .halign(gtk::Align::End)
        .valign(gtk::Align::End)
        .margin_end(10)
        .margin_bottom(10)
        .build();
    show_muted(&button, MUTED.get());
    button.connect_clicked(|_| {
        let muted = !MUTED.get();
        MUTED.set(muted);
        if let (Some(player), Some((_, ticket))) = (Player::get(), CURRENT.get()) {
            player.set_muted(ticket, muted);
        }
        MUTE_BUTTONS.with(|b| {
            b.borrow_mut().retain(|w| w.upgrade().is_some());
            for w in b.borrow().iter().filter_map(|w| w.upgrade()) {
                show_muted(&w, muted);
            }
        });
    });
    MUTE_BUTTONS.with(|b| b.borrow_mut().push(button.downgrade()));
    button
}

pub fn show_muted(button: &gtk::Button, muted: bool) {
    button.set_icon_name(if muted { "audio-volume-muted-symbolic" } else { "audio-volume-high-symbolic" });
    button.set_tooltip_text(Some(if muted { "Unmute" } else { "Mute" }));
}

// ----- Viewers ---------------------------------------------------------------

/// Plays `url` in `picture` while `dialog` is open, with sound, and with a
/// play/pause button. `on_end` runs if it plays to the end (not looping).
pub fn play_in_dialog(
    dialog: &adw::Dialog,
    overlay: &gtk::Overlay,
    picture: &gtk::Picture,
    url: &str,
    looping: bool,
    on_end: impl Fn() + 'static,
) {
    let Some(player) = Player::get() else { return };
    let exclusive = RefCell::new(Some(player.exclusive()));
    let ticket = player.play(Some(picture), url, Opts { muted: false, looping, playing: true });
    player.on_end(ticket, on_end);
    let pause = gtk::Button::builder()
        .icon_name("media-playback-pause-symbolic")
        .tooltip_text("Pause")
        .css_classes(["osd", "circular"])
        .halign(gtk::Align::Start)
        .valign(gtk::Align::End)
        .margin_start(12)
        .margin_bottom(12)
        .build();
    let playing = Rc::new(Cell::new(true));
    let weak: Weak<Player> = Rc::downgrade(&player);
    pause.connect_clicked(move |b| {
        let Some(player) = weak.upgrade() else { return };
        let now = !playing.get();
        playing.set(now);
        player.set_playing(ticket, now);
        b.set_icon_name(if now { "media-playback-pause-symbolic" } else { "media-playback-start-symbolic" });
        b.set_tooltip_text(Some(if now { "Pause" } else { "Play" }));
    });
    overlay.add_overlay(&pause);
    dialog.connect_closed(move |_| {
        player.release(ticket);
        exclusive.take();
    });
}

/// Play/pause button for a voice message.
pub fn voice_button(url: String) -> gtk::Button {
    let button = gtk::Button::builder().css_classes(["flat", "circular"]).build();
    show_playing(&button, false);
    let ticket = Rc::new(Cell::new(0u64));
    let playing = Rc::new(Cell::new(false));
    button.connect_clicked(move |b| {
        let Some(player) = Player::get() else { return };
        if player.owns(ticket.get()) && playing.get() {
            player.set_playing(ticket.get(), false);
            playing.set(false);
            show_playing(b, false);
            return;
        }
        if player.owns(ticket.get()) && !player.ended.get() {
            player.set_playing(ticket.get(), true);
        } else {
            let t = player.play(None, &url, Opts { muted: false, looping: false, playing: true });
            ticket.set(t);
            for cb in [0, 1] {
                let weak = b.downgrade();
                let playing = playing.clone();
                let stop = move || {
                    playing.set(false);
                    if let Some(b) = weak.upgrade() {
                        show_playing(&b, false);
                    }
                };
                if cb == 0 { player.on_lost(t, stop) } else { player.on_end(t, stop) }
            }
        }
        playing.set(true);
        show_playing(b, true);
    });
    button
}

fn show_playing(button: &gtk::Button, playing: bool) {
    button.set_icon_name(if playing { "media-playback-pause-symbolic" } else { "media-playback-start-symbolic" });
    button.set_tooltip_text(Some(if playing { "Pause" } else { "Play" }));
}
