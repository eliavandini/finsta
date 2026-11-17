//! Saving posts, reels and stories to the Downloads folder.

use std::path::PathBuf;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

use crate::api::Media;
use crate::api::web::download_urls;
use crate::rt;
use crate::ui::window::Main;

/// Downloads/Finsta (or ~/Downloads/Finsta).
fn folder() -> PathBuf {
    glib::user_special_dir(glib::UserDirectory::Downloads)
        .unwrap_or_else(|| glib::home_dir().join("Downloads"))
        .join("Finsta")
}

fn file_stem(media: &Media, owner: &str) -> String {
    let owner: String = owner
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_'))
        .collect();
    let id = if media.code.is_empty() { media.id.split('_').next().unwrap_or("").to_string() } else { media.code.clone() };
    let id: String = id.chars().filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_')).collect();
    match (owner.is_empty(), id.is_empty()) {
        (false, false) => format!("{owner}_{id}"),
        (true, false) => id,
        (false, true) => format!("{owner}_{}", media.taken_at),
        (true, true) => format!("instagram_{}", media.taken_at),
    }
}

/// A flat button that downloads `media` (every photo and video in it).
pub fn button(main: &Rc<Main>, media: &Media, owner: &str) -> gtk::Button {
    let b = gtk::Button::builder()
        .icon_name("folder-download-symbolic")
        .tooltip_text("Download")
        .css_classes(["flat", "circular"])
        .build();
    let weak = Rc::downgrade(main);
    let media = media.clone();
    let owner = owner.to_string();
    b.connect_clicked(move |b| {
        if let Some(main) = weak.upgrade() {
            save(&main, &media, &owner, Some(b));
        }
    });
    b
}

/// Download every photo and video in `media` to Downloads/Finsta, returning
/// where they were saved.
pub async fn fetch(main: &Rc<Main>, media: &Media, owner: &str) -> anyhow::Result<Vec<PathBuf>> {
    let urls = download_urls(media);
    if urls.is_empty() {
        anyhow::bail!("There's nothing to download here");
    }
    let stem = file_stem(media, owner);
    let client = main.client.clone();
    rt::bg(async move {
        let dir = folder();
        tokio::fs::create_dir_all(&dir).await?;
        let count = urls.len();
        let mut saved = Vec::new();
        for (i, (url, ext)) in urls.iter().enumerate() {
            let bytes = client.download(url).await?;
            let name = if count > 1 { format!("{stem}_{}.{ext}", i + 1) } else { format!("{stem}.{ext}") };
            let path = dir.join(name);
            tokio::fs::write(&path, bytes).await?;
            saved.push(path);
        }
        anyhow::Ok(saved)
    })
    .await
}

/// Download `media` and say so in a toast.
pub fn save(main: &Rc<Main>, media: &Media, owner: &str, button: Option<&gtk::Button>) {
    let weak = Rc::downgrade(main);
    let button = button.map(|b| b.downgrade());
    if let Some(b) = button.as_ref().and_then(|b| b.upgrade()) {
        b.set_sensitive(false);
    }
    let main = main.clone();
    let media = media.clone();
    let owner = owner.to_string();
    glib::spawn_future_local(async move {
        let res = fetch(&main, &media, &owner).await;
        drop(main);
        if let Some(b) = button.as_ref().and_then(|b| b.upgrade()) {
            b.set_sensitive(true);
        }
        let Some(main) = weak.upgrade() else { return };
        match res {
            Ok(saved) => {
                let what = if saved.len() == 1 { "1 file".to_string() } else { format!("{} files", saved.len()) };
                let toast = adw::Toast::builder()
                    .title(format!("Saved {what} to Downloads/Finsta"))
                    .button_label("Show")
                    .build();
                let window = main.window.downgrade();
                let first = saved.into_iter().next();
                toast.connect_button_clicked(move |_| {
                    if let (Some(window), Some(path)) = (window.upgrade(), first.as_ref()) {
                        let file = gtk::gio::File::for_path(path);
                        gtk::FileLauncher::new(Some(&file)).open_containing_folder(
                            Some(&window),
                            gtk::gio::Cancellable::NONE,
                            |_| {},
                        );
                    }
                });
                main.toasts.add_toast(toast);
            }
            Err(e) => {
                eprintln!("finsta: download failed: {e}");
                main.toast("Couldn't download this. Check your connection and try again.");
            }
        }
    });
}
