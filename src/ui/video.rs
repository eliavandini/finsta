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

/// Play only part of the file: from `start`, for `length` (to the end if
/// None). Used for the part of a song a post or story plays.
#[derive(Clone, Copy, PartialEq)]
pub struct Segment {
    pub start: Duration,
    pub length: Option<Duration>,
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
    /// The part being played, if not the whole file.
    segment: Cell<Option<Segment>>,
    /// Jump to the segment once the file is ready, then play if wanted.
    segment_pending: Cell<bool>,
    want_playing: Cell<bool>,
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
            segment: Cell::new(None),
            segment_pending: Cell::new(false),
            want_playing: Cell::new(false),
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

    /// Jump to the start of the segment (or of the file), keeping its end.
    fn seek_to_start(&self) -> bool {
        match self.segment.get() {
            Some(seg) => {
                let start = gst::ClockTime::from_nseconds(seg.start.as_nanos() as u64);
                let (stop_type, stop) = match seg.length {
                    Some(len) => (gst::SeekType::Set, Some(start + gst::ClockTime::from_nseconds(len.as_nanos() as u64))),
                    None => (gst::SeekType::None, gst::ClockTime::NONE),
                };
                self.playbin
                    .seek(1.0, gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE, gst::SeekType::Set, Some(start), stop_type, stop)
                    .is_ok()
            }
            None => self
                .playbin
                .seek_simple(gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT, gst::ClockTime::ZERO)
                .is_ok(),
        }
    }

    fn on_message(&self, msg: &gst::Message) {
        match msg.view() {
            gst::MessageView::AsyncDone(_) if self.segment_pending.replace(false) => {
                if !self.seek_to_start() {
                    eprintln!("finsta: music: couldn't jump to the song's part; playing from the start");
                }
                if self.want_playing.get() {
                    let _ = self.playbin.set_state(gst::State::Playing);
                }
            }
            gst::MessageView::Error(_) if self.segment.get().is_some() && !self.loop_seek.get() => {
                // Most likely the server can't jump into the file (the
                // error can come after the jump seemed to work): play it
                // from the start, once.
                eprintln!("finsta: music: the server can't jump to the song's part; playing from the start");
                self.segment.set(None);
                self.failed.set(false);
                self.restart();
            }
            gst::MessageView::Eos(_) => {
                if self.looping.get() && self.loop_restart.get() {
                    self.restart();
                } else if self.looping.get() {
                    self.loop_seek.set(true);
                    if !self.seek_to_start() {
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
                self.segment.set(None);
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
        if self.segment.get().is_some() {
            self.segment_pending.set(true);
            let _ = self.playbin.set_state(gst::State::Paused);
        } else {
            let _ = self.playbin.set_state(gst::State::Playing);
        }
    }

    /// Play `url`, showing it in `picture` (None: sound only). Whoever had
    /// the player loses it. Returns a ticket for the other calls.
    pub fn play(&self, picture: Option<&gtk::Picture>, url: &str, opts: Opts) -> u64 {
        self.play_part(picture, url, opts, None)
    }

    /// Like `play`, for part of the file only.
    pub fn play_part(&self, picture: Option<&gtk::Picture>, url: &str, opts: Opts, segment: Option<Segment>) -> u64 {
        self.detach(true);
        self.segment.set(segment.filter(|s| !s.start.is_zero() || s.length.is_some()));
        self.segment_pending.set(self.segment.get().is_some());
        self.want_playing.set(opts.playing);
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
        // A segment starts paused, jumps to its start, then plays.
        let state = if opts.playing && !self.segment_pending.get() { gst::State::Playing } else { gst::State::Paused };
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
            self.want_playing.set(playing);
            if self.segment_pending.get() {
                return;
            }
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
        if let Some(seg) = self.segment.get()
            && let Some(len) = seg.length
        {
            let into = position.nseconds().saturating_sub(seg.start.as_nanos() as u64);
            return (!len.is_zero()).then(|| into as f64 / len.as_nanos() as f64);
        }
        let duration = self.playbin.query_duration::<gst::ClockTime>()?;
        (duration.nseconds() > 0).then(|| position.nseconds() as f64 / duration.nseconds() as f64)
    }

    /// Jump to `fraction` (0 to 1) of `ticket`'s video.
    pub fn seek(&self, ticket: u64, fraction: f64) {
        if !self.owns(ticket) || self.segment.get().is_some() {
            return;
        }
        let Some(duration) = self.playbin.query_duration::<gst::ClockTime>() else { return };
        let to = gst::ClockTime::from_nseconds((duration.nseconds() as f64 * fraction.clamp(0.0, 1.0)) as u64);
        self.ended.set(false);
        // A paused video shows the new frame only as a fresh "preroll".
        self.has_frame.set(false);
        let _ = self.playbin.seek_simple(gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT, to);
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

/// Stop whatever is playing (the window is going away).
pub fn stop_all() {
    if let Some(player) = PLAYER.with(|p| p.borrow().clone().flatten()) {
        player.detach(true);
        let _ = player.playbin.set_state(gst::State::Ready);
    }
    CURRENT.set(None);
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
    // A paused video's first frame (also after a jump) only arrives as the
    // "preroll", and asking for a sample first throws it away.
    let preroll = if first { sink.emit_by_name::<Option<gst::Sample>>("try-pull-preroll", &[&0u64]) } else { None };
    let sample = match preroll {
        Some(s) => s,
        None => sink.emit_by_name::<Option<gst::Sample>>("try-pull-sample", &[&0u64])?,
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

/// Where a song's audio is, once known.
pub type SongReady = Box<dyn FnOnce(Option<(String, Segment)>)>;

enum SongState {
    Unknown,
    Finding,
    Ready(String, Segment),
    Failed,
}

enum Source {
    /// A video, drawn in the slot's picture.
    Video(String),
    /// A photo post's song: sound only, found when first needed.
    Song { state: Rc<RefCell<SongState>>, find: Rc<dyn Fn(SongReady)> },
}

struct Slot {
    id: u64,
    /// What has to be on screen (for a video, the picture it's drawn in).
    widget: glib::WeakRef<gtk::Widget>,
    source: Source,
}

thread_local! {
    static SLOTS: RefCell<Vec<Slot>> = const { RefCell::new(Vec::new()) };
    static NEXT_SLOT: Cell<u64> = const { Cell::new(0) };
    /// The slot playing now, and its ticket.
    static CURRENT: Cell<Option<(u64, u64)>> = const { Cell::new(None) };
    /// You paused the video that's playing now.
    static PAUSED: Cell<bool> = const { Cell::new(false) };
    static FAILED: RefCell<HashSet<String>> = RefCell::new(HashSet::new());
    static POLLING: Cell<bool> = const { Cell::new(false) };
    static MUTED: Cell<bool> = const { Cell::new(true) };
    static MUTE_BUTTONS: RefCell<Vec<glib::WeakRef<gtk::Button>>> = const { RefCell::new(Vec::new()) };
}

fn add_slot(widget: &gtk::Widget, source: Source) {
    let id = NEXT_SLOT.with(|n| {
        n.set(n.get() + 1);
        n.get()
    });
    SLOTS.with(|s| s.borrow_mut().push(Slot { id, widget: widget.downgrade(), source }));
    if !POLLING.with(|p| p.replace(true)) {
        glib::timeout_add_local(Duration::from_millis(250), || {
            poll();
            glib::ControlFlow::Continue
        });
    }
    widget.connect_map(|_| poll());
}

/// Starts playing (muted, looping) when most of it is on screen, and stops
/// when it scrolls away. Only the most visible video plays.
pub fn autoplay(picture: &gtk::Picture, url: String) {
    add_slot(picture.upcast_ref(), Source::Video(url));
}

/// A photo post's song: plays (looping) while `widget` is the most visible
/// thing on screen and the sound is on. `find` looks up its audio the first
/// time it's needed.
pub fn autoplay_song(widget: &impl IsA<gtk::Widget>, find: impl Fn(SongReady) + 'static) {
    add_slot(widget.as_ref(), Source::Song { state: Rc::new(RefCell::new(SongState::Unknown)), find: Rc::new(find) });
}

/// What a slot would play now, if it's chosen.
enum Playable {
    Video(gtk::Picture, String),
    Song(String, Segment),
}

fn poll() {
    SLOTS.with(|s| s.borrow_mut().retain(|x| x.widget.upgrade().is_some()));
    let Some(player) = Player::get() else { return };
    let mut current = CURRENT.get().filter(|(_, t)| player.owns(*t));
    if let Some((slot, ticket)) = current
        && player.failed(ticket)
    {
        SLOTS.with(|s| {
            if let Some(x) = s.borrow().iter().find(|x| x.id == slot) {
                match &x.source {
                    Source::Video(url) => {
                        FAILED.with(|f| f.borrow_mut().insert(url.clone()));
                    }
                    Source::Song { state, .. } => *state.borrow_mut() = SongState::Failed,
                }
            }
        });
        player.release(ticket);
        current = None;
    }
    CURRENT.set(current);
    if player.exclusive.get() > 0 {
        return;
    }
    let muted = MUTED.get();
    // Visible slots that could play, and songs still to look up.
    let mut to_find = Vec::new();
    let visible: Vec<(u64, Playable, f32)> = SLOTS.with(|s| {
        s.borrow()
            .iter()
            .filter_map(|x| {
                let widget = x.widget.upgrade()?;
                let f = crate::ui::util::visible_fraction(&widget);
                let playable = match &x.source {
                    Source::Video(url) if FAILED.with(|f| f.borrow().contains(url)) => return None,
                    Source::Video(url) => Playable::Video(widget.downcast().ok()?, url.clone()),
                    // Silent songs aren't worth loading.
                    Source::Song { .. } if muted => return None,
                    Source::Song { state, find } => match &*state.borrow() {
                        SongState::Ready(url, seg) => Playable::Song(url.clone(), *seg),
                        SongState::Unknown if f >= 0.6 => {
                            to_find.push((state.clone(), find.clone()));
                            return None;
                        }
                        _ => return None,
                    },
                };
                Some((x.id, playable, f))
            })
            .collect()
    });
    for (state, find) in to_find {
        *state.borrow_mut() = SongState::Finding;
        find(Box::new(move |found| {
            *state.borrow_mut() = match found {
                Some((url, seg)) => SongState::Ready(url, seg),
                None => SongState::Failed,
            };
            poll();
        }));
    }
    let best = visible
        .into_iter()
        .filter(|(_, _, f)| *f >= 0.6)
        .max_by(|a, b| a.2.total_cmp(&b.2));
    // What's playing keeps playing while a good part of it shows, unless
    // something else is fully on screen and it isn't.
    if let Some((slot, _)) = current
        && let Some(f) = SLOTS.with(|s| {
            s.borrow()
                .iter()
                .find(|x| x.id == slot)
                .and_then(|x| x.widget.upgrade())
                .map(|w| crate::ui::util::visible_fraction(&w))
        })
        && f >= 0.35
        && !best.as_ref().is_some_and(|(id, _, bf)| *id != slot && *bf >= 0.95 && f < 0.75)
    {
        return;
    }
    match best {
        Some((id, playable, _)) if current.is_some() || player.is_idle() => {
            let ticket = match playable {
                Playable::Video(picture, url) => {
                    player.play(Some(&picture), &url, Opts { muted, looping: true, playing: true })
                }
                Playable::Song(url, seg) => {
                    player.play_part(None, &url, Opts { muted: false, looping: true, playing: true }, Some(seg))
                }
            };
            CURRENT.set(Some((id, ticket)));
            PAUSED.set(false);
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

/// The ticket of the video drawn in `picture`, if it's the one playing now.
fn ticket_of(picture: &gtk::Picture) -> Option<u64> {
    let (slot, ticket) = CURRENT.get()?;
    let shown = SLOTS.with(|s| {
        s.borrow().iter().any(|x| x.id == slot && x.widget.upgrade().is_some_and(|w| &w == picture.upcast_ref::<gtk::Widget>()))
    });
    shown.then_some(ticket)
}

/// A post video's controls, over it in `overlay`: click it to pause or play
/// (or to play it, if another video is playing), and a bar to seek.
pub fn controls(overlay: &gtk::Overlay, picture: &gtk::Picture) {
    let bar = gtk::Scale::builder()
        .adjustment(&gtk::Adjustment::new(0.0, 0.0, 1.0, 0.01, 0.05, 0.0))
        .draw_value(false)
        .valign(gtk::Align::End)
        .margin_start(10)
        .margin_end(60)
        .margin_bottom(14)
        .css_classes(["osd", "video-seek"])
        .visible(false)
        .build();
    let paused_icon = gtk::Image::builder()
        .icon_name("media-playback-start-symbolic")
        .pixel_size(32)
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .css_classes(["osd", "video-paused"])
        .can_target(false)
        .visible(false)
        .build();
    overlay.add_overlay(&paused_icon);
    overlay.add_overlay(&bar);

    let pic = picture.downgrade();
    bar.connect_change_value(move |_, _, value| {
        if let (Some(player), Some(ticket)) = (Player::get(), pic.upgrade().as_ref().and_then(ticket_of)) {
            player.seek(ticket, value);
        }
        glib::Propagation::Proceed
    });

    let (pic, b, icon) = (picture.downgrade(), bar.downgrade(), paused_icon.downgrade());
    let update = move || {
        let (Some(picture), Some(bar), Some(icon)) = (pic.upgrade(), b.upgrade(), icon.upgrade()) else { return false };
        let ticket = ticket_of(&picture);
        let progress = ticket.and_then(|t| Player::get()?.progress(t));
        bar.set_visible(progress.is_some());
        if let Some(p) = progress {
            bar.set_value(p);
        }
        icon.set_visible(ticket.is_some() && PAUSED.get());
        true
    };
    let update = Rc::new(update);
    let u = update.clone();
    glib::timeout_add_local(Duration::from_millis(250), move || {
        if u() { glib::ControlFlow::Continue } else { glib::ControlFlow::Break }
    });

    let click = gtk::GestureClick::new();
    let pic = picture.downgrade();
    click.connect_released(move |gesture, presses, _, _| {
        let (Some(picture), Some(player)) = (pic.upgrade(), Player::get()) else { return };
        if presses != 1 || player.exclusive.get() > 0 {
            return;
        }
        gesture.set_state(gtk::EventSequenceState::Claimed);
        match ticket_of(&picture) {
            Some(ticket) if !player.failed(ticket) => {
                let paused = !PAUSED.get();
                PAUSED.set(paused);
                player.set_playing(ticket, !paused);
            }
            _ => play_now(&player, &picture),
        }
        update();
    });
    picture.add_controller(click);
}

/// Play the video drawn in `picture` now, instead of what's playing.
fn play_now(player: &Player, picture: &gtk::Picture) {
    let found = SLOTS.with(|s| {
        s.borrow().iter().find_map(|x| match &x.source {
            Source::Video(url) if x.widget.upgrade().is_some_and(|w| &w == picture.upcast_ref::<gtk::Widget>()) => {
                Some((x.id, url.clone()))
            }
            _ => None,
        })
    });
    let Some((id, url)) = found else { return };
    if let Some((_, ticket)) = CURRENT.get() {
        player.release(ticket);
    }
    FAILED.with(|f| f.borrow_mut().remove(&url));
    let ticket = player.play(Some(picture), &url, Opts { muted: MUTED.get(), looping: true, playing: true });
    CURRENT.set(Some((id, ticket)));
    PAUSED.set(false);
}

fn is_song_slot(id: u64) -> bool {
    SLOTS.with(|s| s.borrow().iter().any(|x| x.id == id && matches!(x.source, Source::Song { .. })))
}

/// How much of `p` is on screen, from 0 to 1.
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
        if let (Some(player), Some((slot, ticket))) = (Player::get(), CURRENT.get()) {
            if muted && is_song_slot(slot) {
                // A muted song is just silence: stop it.
                player.release(ticket);
                CURRENT.set(None);
            } else {
                player.set_muted(ticket, muted);
            }
        }
        MUTE_BUTTONS.with(|b| {
            b.borrow_mut().retain(|w| w.upgrade().is_some());
            for w in b.borrow().iter().filter_map(|w| w.upgrade()) {
                show_muted(&w, muted);
            }
        });
        // Sound on: a song on screen can start.
        poll();
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
