# Finsta

A calm Instagram client for the Linux desktop, written in Rust with GTK 4 and
libadwaita. It does two things:

- **Direct messages**: your conversations with people and groups.
- **A home feed** with posts and stories from accounts you follow, and only
  those accounts.

There are no reels, no explore page, no suggested posts and no ads.

## Design rules

- **The home feed is the only place that loads more as you scroll**, and it
  stops for good at Instagram's "You're all caught up" marker. Everything
  after that marker is suggested content, so Finsta never loads it.
- **Feed filtering**: ads and injected items are dropped, and every post and
  story is checked against the list of accounts you follow.
- **No endless scrolling anywhere else.** Profiles show 12 posts with a "Show
  More Posts" button, search shows a short fixed list, older messages and
  older conversations load only when you press a button.
- **Shared reels in chats open in your browser**, so a conversation can't
  turn into a reel feed.
- **Disappearing photos and videos follow the sender's choice.** They show as
  "Tap to view". Finsta tells Instagram when you open one, so the sender sees
  "Opened", and shows it only after Instagram confirms. "View once" can't be
  opened again; "Allow replay" allows one replay. Nothing is cached, and
  videos play once without a scrubber.

## Layout

- **Top bar**: a search field (people and conversations; <kbd>Ctrl</kbd>+<kbd>F</kbd>).
- **Sidebar**: Home, the list of conversations (filter by All / People /
  Groups), and your Profile.
- **Content**: home feed with stories, conversations, profiles and posts.
  On narrow windows the sidebar folds away.

## Installing

You need Rust (via [rustup](https://rustup.rs)) and the development packages
for GTK 4, libadwaita and WebKitGTK 6.0. Videos play through GStreamer, which
needs gst-plugins-good and an H.264 decoder such as gst-libav.

```sh
# Arch Linux
sudo pacman -S --needed base-devel rustup gtk4 libadwaita webkitgtk-6.0 gst-plugins-good gst-libav

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

## Signing in

- **Sign In with Instagram** opens Instagram's own login page in an embedded,
  temporary browser, so two-factor authentication and security checks work
  as usual. Finsta never sees your password; it keeps only the resulting
  cookies.
- **Session cookie**: paste the `sessionid` (and optionally `csrftoken`)
  cookie from a browser where you're signed in to instagram.com.

The session is stored in `~/.config/finsta/session.json` with `0600`
permissions. **Log Out** on your profile removes it.

## Caveats

Instagram has no public API for personal accounts, so Finsta uses the
internal API that instagram.com itself calls, with your web login's cookies
and the user agent of the browser you signed in with. (It used to pose as the
iPhone app, which Instagram now rejects.) That means:

- It can break whenever Instagram changes its API.
- Using unofficial clients is against Instagram's terms and could get an
  account flagged. Use it at your own risk.
- New messages are fetched by polling (every 8 seconds in an open
  conversation, every 30 seconds for the conversation list), not pushed.
- Sending photos, videos (including disappearing ones) and voice notes is not
  supported yet.
- Instagram can't tell when you screenshot a disappearing photo in Finsta, so
  the sender won't get a screenshot alert.
- Instagram itself dropped end-to-end encrypted chats on 8 May 2026, so all
  remaining conversations work through the normal message API.

## Development

`dev/mock_api.py` is a fake Instagram API with sample data, for working on
the UI without a real account:

```sh
python3 dev/mock_api.py 8765
FINSTA_API_BASE=http://127.0.0.1:8765/api/v1/ XDG_CONFIG_HOME=/tmp/finsta-dev cargo run
# then sign in with the session cookie 1000%3Aabc%3A1
```
