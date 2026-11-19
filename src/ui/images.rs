//! Downloads, decodes (off the main thread) and caches remote images.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::sync::OnceLock;
use std::time::Duration;

use gtk::prelude::*;
use gtk::subclass::prelude::ObjectSubclassIsExt;
use gtk::{gdk, gdk_pixbuf, glib};
use tokio::sync::Semaphore;

use crate::rt;

const CACHE_SIZE: usize = 400;

thread_local! {
    static CACHE: RefCell<(HashMap<String, gdk::Texture>, VecDeque<String>)> =
        RefCell::new((HashMap::new(), VecDeque::new()));
}

fn http() -> &'static reqwest::Client {
    static HTTP: OnceLock<reqwest::Client> = OnceLock::new();
    HTTP.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .user_agent("Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.0 Safari/605.1.15")
            .build()
            .expect("failed to build HTTP client")
    })
}

fn limiter() -> &'static Semaphore {
    static SEM: OnceLock<Semaphore> = OnceLock::new();
    SEM.get_or_init(|| Semaphore::new(6))
}

async fn fetch(url: String) -> Option<glib::Bytes> {
    let _permit = limiter().acquire().await.ok()?;
    let resp = http().get(&url).send().await.ok()?.error_for_status().ok()?;
    let bytes = resp.bytes().await.ok()?;
    Some(glib::Bytes::from_owned(bytes.to_vec()))
}

async fn fetch_and_decode(url: String) -> Option<gdk::Texture> {
    let bytes = fetch(url).await?;
    tokio::task::spawn_blocking(move || gdk::Texture::from_bytes(&bytes).ok())
        .await
        .ok()
        .flatten()
}

pub fn cached(url: &str) -> Option<gdk::Texture> {
    CACHE.with(|c| c.borrow().0.get(url).cloned())
}

pub async fn texture(url: &str) -> Option<gdk::Texture> {
    if url.is_empty() {
        return None;
    }
    if let Some(tex) = cached(url) {
        return Some(tex);
    }
    let tex = rt::bg(fetch_and_decode(url.to_string())).await?;
    CACHE.with(|c| {
        let mut c = c.borrow_mut();
        if c.0.insert(url.to_string(), tex.clone()).is_none() {
            c.1.push_back(url.to_string());
        }
        while c.1.len() > CACHE_SIZE {
            if let Some(old) = c.1.pop_front() {
                c.0.remove(&old);
            }
        }
    });
    Some(tex)
}

/// Download and decode without keeping a copy in the cache.
pub async fn texture_uncached(url: &str) -> Option<gdk::Texture> {
    if url.is_empty() {
        return None;
    }
    rt::bg(fetch_and_decode(url.to_string())).await
}

pub fn load_picture(picture: &gtk::Picture, url: Option<String>) {
    let Some(url) = url.filter(|u| !u.is_empty()) else { return };
    if let Some(tex) = cached(&url) {
        picture.set_paintable(Some(&tex));
        return;
    }
    let weak = picture.downgrade();
    glib::spawn_future_local(async move {
        if let Some(tex) = texture(&url).await
            && let Some(picture) = weak.upgrade()
        {
            picture.set_paintable(Some(&tex));
        }
    });
}

/// Like `load_picture`, but plays it if it's animated (GIFs, stickers). Not
/// cached: an animation's frames take much more memory than a picture.
pub fn load_animated(picture: &gtk::Picture, url: Option<String>) {
    let Some(url) = url.filter(|u| !u.is_empty()) else { return };
    let weak = picture.downgrade();
    glib::spawn_future_local(async move {
        let Some(bytes) = rt::bg(fetch(url)).await else { return };
        // shortcut: decoded on the main thread (gdk-pixbuf animations can't move between threads);
        // fine at GIF and sticker sizes, decode frames on a thread if big ones stutter.
        let loader = gdk_pixbuf::PixbufLoader::new();
        if loader.write_bytes(&bytes).and_then(|_| loader.close()).is_err() {
            return;
        }
        if let (Some(frames), Some(picture)) = (loader.animation(), weak.upgrade()) {
            let frames = frames.iter(None);
            let shown = Frame::default();
            shown.imp().0.replace(Some(gdk::Texture::for_pixbuf(&frames.pixbuf())));
            picture.set_paintable(Some(&shown));
            play(weak, frames, shown);
        }
    });
}

/// Wait for the next frame, then show it.
fn play(weak: glib::WeakRef<gtk::Picture>, frames: gdk_pixbuf::PixbufAnimationIter, shown: Frame) {
    let Some(picture) = weak.upgrade() else { return };
    // None: a still picture, or the animation's last frame. Animations off: the first frame.
    let Some(delay) = frames.delay_time().filter(|_| picture.settings().is_gtk_enable_animations()) else { return };
    // Not on screen (scrolled away, another page): only check back now and then.
    let on_screen = |p: &gtk::Picture| crate::ui::util::visible_fraction(p) > 0.0;
    let delay = if on_screen(&picture) { delay.max(Duration::from_millis(20)) } else { Duration::from_millis(500) };
    glib::timeout_add_local_once(delay, move || {
        if weak.upgrade().is_some_and(|p| on_screen(&p)) && frames.advance(std::time::SystemTime::now()) {
            shown.imp().0.replace(Some(gdk::Texture::for_pixbuf(&frames.pixbuf())));
            // Only a redraw: a new paintable would lay out the whole page again.
            shown.invalidate_contents();
        }
        play(weak, frames, shown)
    });
}

mod frame {
    use gtk::gdk::subclass::prelude::*;
    use gtk::{gdk, glib, prelude::*};
    use std::cell::RefCell;

    /// An animation's current frame.
    #[derive(Default)]
    pub struct Frame(pub RefCell<Option<gdk::Texture>>);

    #[glib::object_subclass]
    impl ObjectSubclass for Frame {
        const NAME: &'static str = "FinstaFrame";
        type Type = super::Frame;
        type Interfaces = (gdk::Paintable,);
    }

    impl ObjectImpl for Frame {}

    impl PaintableImpl for Frame {
        fn flags(&self) -> gdk::PaintableFlags {
            gdk::PaintableFlags::STATIC_SIZE
        }
        fn intrinsic_width(&self) -> i32 {
            self.0.borrow().as_ref().map_or(0, |t| t.width())
        }
        fn intrinsic_height(&self) -> i32 {
            self.0.borrow().as_ref().map_or(0, |t| t.height())
        }
        fn snapshot(&self, snapshot: &gdk::Snapshot, width: f64, height: f64) {
            if let Some(t) = self.0.borrow().as_ref() {
                t.snapshot(snapshot, width, height);
            }
        }
    }
}

glib::wrapper! {
    pub struct Frame(ObjectSubclass<frame::Frame>) @implements gdk::Paintable;
}

impl Default for Frame {
    fn default() -> Self {
        glib::Object::new()
    }
}

pub fn load_avatar(avatar: &adw::Avatar, url: Option<String>) {
    let Some(url) = url.filter(|u| !u.is_empty()) else { return };
    if let Some(tex) = cached(&url) {
        avatar.set_custom_image(Some(&tex));
        return;
    }
    let weak = avatar.downgrade();
    glib::spawn_future_local(async move {
        if let Some(tex) = texture(&url).await
            && let Some(avatar) = weak.upgrade()
        {
            avatar.set_custom_image(Some(&tex));
        }
    });
}
