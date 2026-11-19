//! How the home feed shows posts (columns, size, comments beside posts),
//! saved in ~/.config/finsta/settings.json (which also holds the app's
//! preferences, see prefs.rs).

use std::cell::Cell;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum PostSize {
    Small,
    #[default]
    Medium,
    Large,
}

impl PostSize {
    /// The width of a post's photo or video.
    pub fn width(self) -> i32 {
        match self {
            PostSize::Small => 360,
            PostSize::Medium => 480,
            PostSize::Large => 600,
        }
    }

    pub const ALL: [PostSize; 3] = [PostSize::Small, PostSize::Medium, PostSize::Large];

    pub fn label(self) -> &'static str {
        match self {
            PostSize::Small => "Small",
            PostSize::Medium => "Medium",
            PostSize::Large => "Large",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct FeedLayout {
    pub columns: u32,
    pub size: PostSize,
    /// Comments in a panel beside each post instead of a dialog.
    pub side_comments: bool,
}

impl Default for FeedLayout {
    fn default() -> Self {
        FeedLayout { columns: 1, size: PostSize::Large, side_comments: false }
    }
}

/// The width of the panel beside a post.
pub const SIDE_WIDTH: i32 = 340;
/// The space between columns.
pub const GAP: i32 = 18;

impl FeedLayout {
    /// The width one column needs for a post at its full size.
    pub fn column_width(&self) -> i32 {
        self.size.width() + if self.side_comments { SIDE_WIDTH } else { 0 }
    }

    /// The narrowest a column can comfortably get.
    pub fn min_column_width(&self) -> i32 {
        if self.side_comments { 300 + SIDE_WIDTH } else { 300 }
    }

    /// How many columns fit in `width` pixels (at most the chosen number).
    pub fn columns_for(&self, width: i32) -> u32 {
        let fit = (width + GAP) / (self.min_column_width() + GAP);
        (fit.max(1) as u32).min(self.columns.clamp(1, 4))
    }
}

pub(crate) fn path() -> Option<PathBuf> {
    Some(dirs::config_dir()?.join("finsta").join("settings.json"))
}

pub(crate) fn read_all() -> Value {
    path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .filter(Value::is_object)
        .unwrap_or_else(|| Value::Object(Default::default()))
}

thread_local! {
    static FEED: Cell<Option<FeedLayout>> = const { Cell::new(None) };
}

pub fn feed() -> FeedLayout {
    if let Some(l) = FEED.get() {
        return l;
    }
    let mut l: FeedLayout = serde_json::from_value(read_all()["feed"].clone()).unwrap_or_default();
    l.columns = l.columns.clamp(1, 4);
    FEED.set(Some(l));
    l
}

pub fn set_feed(layout: FeedLayout) {
    FEED.set(Some(layout));
    write_key("feed", serde_json::to_value(layout).unwrap_or_default());
}

/// Save one part of settings.json, keeping the rest.
pub(crate) fn write_key(key: &str, value: Value) {
    let Some(path) = path() else { return };
    let mut all = read_all();
    all[key] = value;
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Err(e) = std::fs::write(&path, serde_json::to_string_pretty(&all).unwrap_or_default()) {
        eprintln!("finsta: couldn't save the settings ({})", e.kind());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn columns_fit_the_width() {
        let l = FeedLayout { columns: 3, size: PostSize::Medium, side_comments: false };
        assert_eq!(l.columns_for(2000), 3);
        assert_eq!(l.columns_for(700), 2);
        assert_eq!(l.columns_for(200), 1);
        let side = FeedLayout { side_comments: true, ..l };
        assert_eq!(side.columns_for(1000), 1);
        assert_eq!(side.columns_for(1400), 2);
    }
}
