//! Video playback for the story viewer.
//!
//! GTK's own player (`gtk::MediaFile`) builds a GStreamer pipeline for every
//! video and tears it down when the next one starts. Tearing one down soon
//! after it started playing can crash the app (seen with GTK 4.24 and
//! GStreamer 1.28, with any renderer), and tapping through stories does
//! exactly that. So stories use one pipeline for the whole app — playbin
//! with GStreamer's GTK 4 sink — that is never torn down: each video only
//! changes its address.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gstreamer as gst;
use gstreamer::prelude::*;
use gtk::{gdk, glib};

pub struct StoryPlayer {
    playbin: gst::Element,
    paintable: gdk::Paintable,
    ended: Rc<Cell<bool>>,
    failed: Rc<Cell<bool>>,
    _watch: gst::bus::BusWatchGuard,
}

thread_local! {
    static PLAYER: RefCell<Option<Option<Rc<StoryPlayer>>>> = const { RefCell::new(None) };
}

impl StoryPlayer {
    /// The app's story player; None if GStreamer or its GTK 4 sink
    /// (gst-plugin-gtk4) isn't available. Main thread only.
    pub fn get() -> Option<Rc<StoryPlayer>> {
        PLAYER.with(|p| {
            p.borrow_mut()
                .get_or_insert_with(|| match Self::new() {
                    Ok(player) => Some(Rc::new(player)),
                    Err(e) => {
                        eprintln!("finsta: story videos can't play: {e}");
                        None
                    }
                })
                .clone()
        })
    }

    fn new() -> Result<Self, String> {
        gst::init().map_err(|e| format!("GStreamer: {e}"))?;
        let sink = gst::ElementFactory::make("gtk4paintablesink")
            .build()
            .map_err(|_| "GStreamer's GTK 4 sink (gst-plugin-gtk4) is missing".to_string())?;
        let paintable = sink.property::<gdk::Paintable>("paintable");
        let playbin = gst::ElementFactory::make("playbin")
            .property("video-sink", &sink)
            .build()
            .map_err(|_| "GStreamer's playbin (gst-plugins-base) is missing".to_string())?;
        let ended = Rc::new(Cell::new(false));
        let failed = Rc::new(Cell::new(false));
        let bus = playbin.bus().ok_or("no bus")?;
        let watch = bus
            .add_watch_local({
                let ended = ended.clone();
                let failed = failed.clone();
                move |_, msg| {
                    match msg.view() {
                        gst::MessageView::Eos(_) => ended.set(true),
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
                            eprintln!("finsta: story video failed ({kind})");
                            failed.set(true);
                        }
                        _ => {}
                    }
                    glib::ControlFlow::Continue
                }
            })
            .map_err(|e| format!("bus: {e}"))?;
        Ok(StoryPlayer { playbin, paintable, ended, failed, _watch: watch })
    }

    pub fn paintable(&self) -> &gdk::Paintable {
        &self.paintable
    }

    /// Start playing `url` (or load it paused).
    pub fn load(&self, url: &str, playing: bool) {
        self.ended.set(false);
        self.failed.set(false);
        let _ = self.playbin.set_state(gst::State::Ready);
        self.playbin.set_property("uri", url);
        let state = if playing { gst::State::Playing } else { gst::State::Paused };
        if self.playbin.set_state(state).is_err() {
            self.failed.set(true);
        }
    }

    pub fn set_playing(&self, playing: bool) {
        let _ = self.playbin.set_state(if playing { gst::State::Playing } else { gst::State::Paused });
    }

    /// Stop and let go of the network stream; the pipeline itself stays.
    pub fn stop(&self) {
        let _ = self.playbin.set_state(gst::State::Ready);
        self.ended.set(false);
    }

    /// How far the video has played, from 0 to 1 (None until known).
    pub fn progress(&self) -> Option<f64> {
        if self.ended.get() {
            return Some(1.0);
        }
        let position = self.playbin.query_position::<gst::ClockTime>()?;
        let duration = self.playbin.query_duration::<gst::ClockTime>()?;
        (duration.nseconds() > 0).then(|| position.nseconds() as f64 / duration.nseconds() as f64)
    }

    pub fn failed(&self) -> bool {
        self.failed.get()
    }
}
