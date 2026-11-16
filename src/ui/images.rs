//! Downloads, decodes (off the main thread) and caches remote images.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::sync::OnceLock;
use std::time::Duration;

use gtk::prelude::*;
use gtk::{gdk, glib};
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

async fn fetch_and_decode(url: String) -> Option<gdk::Texture> {
    let _permit = limiter().acquire().await.ok()?;
    let resp = http().get(&url).send().await.ok()?.error_for_status().ok()?;
    let bytes = resp.bytes().await.ok()?;
    let bytes = glib::Bytes::from_owned(bytes.to_vec());
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
