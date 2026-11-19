# Finsta

A calm Instagram client for the Linux desktop, written in Rust with GTK 4 and
libadwaita. It does two things:

- **Direct messages**: your conversations with people and groups.
- **A home feed** with posts and stories from accounts you follow, and only
  those accounts.

There are no reels, no explore page, no suggested posts and no ads.

## Design rules

- **The home feed is the only place that loads more as you scroll**, and it
  ends after a week: it's Instagram's "Following" feed (the people you
  follow, newest first, nothing suggested), and Finsta stops at posts older
  than seven days. On the older ranked feed, used only as a fallback, it
  stops at Instagram's "You're all caught up" marker instead.
- **Feed filtering**: ads and injected items are dropped, and every post and
  story is checked against the list of accounts you follow.
- **No endless scrolling anywhere else.** Profiles show 12 posts with a "Show
  More Posts" button, search shows a short fixed list, older messages and
  older conversations load only when you press a button.
- **Shared posts and reels open one at a time.** A post or reel sent in a
  chat shows a preview; clicking it opens just that post, with its
  comments, so a conversation can't turn into a reel feed. A shared story
  opens in the story viewer.
- **Videos play by themselves while they're on screen**, muted and one at a
  time, and stop when you scroll past them. The speaker button turns the
  sound on for all of them, and also plays the song of a photo post that
  has music. Click a video to pause or resume it (or to play it instead of
  the one playing), and drag the bar along its bottom to jump. Stories play
  with sound, songs on photo stories included.

## Layout

- **Top bar**: a search field (<kbd>Ctrl</kbd>+<kbd>F</kbd>) for
  conversations, people you could message, accounts, a hashtag's top posts
  (one word, with or without the #), or the post a pasted link points to.
- **Sidebar**: Home, the list of conversations (filter by All / People /
  Groups), your Profile, and your Saved posts (the bookmark button).
- **Content**: home feed with stories (the tray spans the whole page),
  conversations, profiles and posts. On narrow windows the sidebar folds
  away.
- **Saved** (the bookmark button next to your profile): your folders across
  the top, All Posts, each folder you made on Instagram, and Songs, with
  the chosen folder's posts filling the page below.
- **Feed layout** (the grid button above the feed): 1 to 4 columns, small,
  medium or large posts, and comments beside each post. Fewer columns, and
  comments below posts, are used when the window is too narrow. Saved in
  `~/.config/finsta/settings.json`.
- **Posts** on a wide window show the photo or video on the left, and the
  account, caption, music, comments and a comment field on the right.
  Replies show under their comment ("View replies" loads all of them).
  Click a commenter's picture or name to open their profile.
  GIFs in comments show under their text. Instagram doesn't send photos in
  comments to apps like Finsta, so those show its placeholder note instead.
  Under each post: like, comment, share, download and save. After saving,
  "Add to Folder" puts the post in one of your folders.
- **Sharing**: the paper plane button on posts, reels and stories sends
  them to one or more conversations (or anyone you search for), with an
  optional message, or copies the link.
- **Songs**: the bookmark next to the song on a post, reel or story saves
  it to Saved › Songs, with a button to get back to what it was on.
- **Stories** play in the middle, with cards for the people before and
  after on either side (click one to jump to them). Opening someone with new
  stories starts at their first unwatched one and only goes on to other
  people with new stories; opening someone you've watched shows everyone.
  Stories show their music, who a reshared story is from, a button to open
  the post or reel it shares, links and mentions.
- **Right-click menus** (or long-press on a touchscreen) on posts, grid
  thumbnails, stories, the story tray, conversations, messages and people,
  with the usual actions: open, like, comment, share, save, download, copy
  the link, view the profile, send a message.
- **Messages**: right-click a message to react with an emoji (pick yours
  again to take it back), copy its text or open what it shares; reactions
  show under the message. Double-click a message to reply to it. Type `:`
  and a name (`:pizza`) to search for an emoji while you write. GIFs and
  stickers play while they're on screen. The GIF button next to the message
  box searches GIPHY for GIFs or stickers and sends the one you pick; it
  needs your own GIPHY API key (free at developers.giphy.com) in
  Preferences. Only your searches go to GIPHY.
- **Preferences** (main menu, <kbd>Ctrl</kbd>+<kbd>,</kbd>): run in the
  background (closing the window keeps Finsta running; Quit is in the main
  menu, <kbd>Ctrl</kbd>+<kbd>Q</kbd>), start at login, and notifications.
- **Notifications**: a new message notifies you while Finsta isn't the
  window you're using; clicking it opens the conversation. Conversations
  muted on Instagram don't notify, and any conversation's right-click menu
  turns its notifications off in Finsta. Messages that arrive while you're
  away aren't marked read until you come back to the window.
- **Downloads**: the download button on posts, reels and stories saves
  them, every photo of a carousel included, to `Downloads/Finsta`.

## Installing

You need Rust (via [rustup](https://rustup.rs)) and the development packages
for GTK 4, libadwaita, GStreamer and WebKitGTK 6.0. Videos play through
GStreamer, which needs gst-plugins-good and an H.264 decoder such as
gst-libav.

```sh
# Arch Linux
sudo pacman -S --needed base-devel rustup gtk4 libadwaita webkitgtk-6.0 gstreamer gst-plugins-good gst-libav

./install.sh              # for your user, into ~/.local
./install.sh --system     # for all users, into /usr/local (asks for sudo)
```

The script checks for missing libraries (and suggests the packages for Arch,
Debian/Ubuntu, Fedora and openSUSE), builds a release binary, and installs it
with a desktop entry and icon, so Finsta shows up in your app launcher.
`./install.sh --uninstall` (plus `--system` if you used it) removes it again;
your saved session in `~/.config/finsta` is left alone. Other options:
`--prefix DIR`, `--no-webkit`, and `DESTDIR` for packaging.

WebKitGTK is only used for the sign-in window. Without it (`--no-webkit`, or
`cargo build --no-default-features`) you sign in with a session cookie.

To just try it from the source tree: `cargo run --release`.

### Ready-made builds

The Release workflow (`.github/workflows/release.yml`) builds an AppImage
for Linux, a zip with `finsta.exe` for Windows and `Finsta.app` for macOS
on Apple silicon. Pushing a tag like `v0.2.0` publishes them as a GitHub
release; "Run workflow" on the Actions tab only builds them. The packaging
steps are in `packaging/`. These builds sign in with a session cookie only
(no WebKitGTK), and the macOS app isn't notarized: the first time, open it
with right-click → Open.

## Signing in

- **Sign In with Instagram** opens Instagram's own login page in an embedded,
  temporary browser, so two-factor authentication and security checks work
  as usual. Finsta never sees your password; it keeps only the resulting
  cookies.
- **Session cookie**: paste the `sessionid` (and optionally `csrftoken`)
  cookie from a browser where you're signed in to instagram.com.

Each account's session is stored in `~/.config/finsta/accounts/` with `0600`
permissions. **Log Out** on your profile removes it.

**Multiple accounts:** the arrow beside **Profile** at the bottom of the
sidebar lists your other accounts to switch to, and **Add Account…** signs in
another one. Finsta shows one account
at a time and reopens the last one used. Logging out of one switches to
another.

## Caveats

Instagram has no public API for personal accounts, so Finsta uses the
internal API that instagram.com itself calls, with your web login's cookies
and the user agent of the browser you signed in with. (It used to pose as the
iPhone app, which Instagram now rejects.) That means:

- It can break whenever Instagram changes its API.
- Using unofficial clients is against Instagram's terms and could get an
  account flagged. Use it at your own risk.
- Messages, the home feed, profiles, profile posts, stories and likes use
  the same GraphQL requests as instagram.com itself: its older routes for
  them now fail or have changed, and its inbox no longer runs on the
  "Lightspeed" chat protocol.
  Finsta finds the requests' current ids in the website's scripts every
  few days, and keeps the website's public code for them in
  `~/.cache/finsta/webcode/`. The older routes remain as fallbacks. This
  part is new; it logs the structure of what it can't read (never names or
  message text) with `dm:` and `web:` prefixes, visible with
  `journalctl --user | grep finsta:`.
- Opening a conversation marks it as read on Instagram, so the other
  person sees "Seen", just like on instagram.com. Likewise, watching a
  story marks it as seen, so you appear in its viewers, and the viewer
  starts at the first story you haven't watched yet.
- You can like stories and reply to them; replies arrive in your
  conversation with that person. If Instagram turns down the story reply
  (its format isn't fully known yet), Finsta tries the older route, and
  then sends your reply as a message with a link to the story.
- Message reactions, search, saving posts and saved folders use requests
  whose exact format Finsta learns from the website's scripts; until it has
  seen them, some of these may fail and say so in the log.
- Shared posts and reels are sent as links, which Instagram shows as a
  preview. Stories are sent as stories, like on instagram.com; if Instagram
  turns that down, as a link.
- Saved songs stay in Finsta (`~/.config/finsta/saved_songs.json`):
  instagram.com has no way to save a sound, so they don't appear in the
  app's saved audio.
- Not supported yet in messages: disappearing photos, voice messages,
  sending anything but text. They show a note to use instagram.com
  instead.
- Open conversations check for new messages every 3 seconds while you're
  in a live conversation (typing, sending or receiving in the last two
  minutes), every 8 seconds for a while after, and every 15 seconds once
  it's quiet or you're in another window. The conversation list refreshes
  every minute (every 30 seconds while Finsta is in the background with
  notifications on), so a notification can come up to that long after the
  message. When Instagram rate limits an endpoint, Finsta pauses that
  endpoint until Instagram says to retry.
- Instagram itself dropped end-to-end encrypted chats on 8 May 2026, so all
  remaining conversations work through the normal message system.

## Development

`dev/mock_api.py` is a fake Instagram with sample data, for working on the
UI without a real account. With `--offmsys` its inbox works like
instagram.com's current one; without it, like the older Lightspeed one:

```sh
python3 dev/mock_api.py 8765 /dev/null --offmsys
FINSTA_API_BASE=http://127.0.0.1:8765/api/v1/ FINSTA_WEB_BASE=http://127.0.0.1:8765/ \
FINSTA_CHAT_URL=ws://127.0.0.1:8765/chat XDG_CONFIG_HOME=/tmp/finsta-dev cargo run
# then sign in with the session cookie 1000%3Aabc%3A1
```
