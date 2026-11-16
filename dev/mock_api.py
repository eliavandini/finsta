"""A fake Instagram API for working on Finsta's UI without a real account.

    python3 dev/mock_api.py 8765 [request-log-file]
    FINSTA_API_BASE=http://127.0.0.1:8765/api/v1/ XDG_CONFIG_HOME=/tmp/finsta-dev cargo run

To test the fallback to the API host for routes instagram.com doesn't serve,
run a second instance with "nodm" as the main host (it answers direct_v2
routes with an HTML 404 page) and point FINSTA_API_HOST at the first one:

    python3 dev/mock_api.py 8766 /dev/null nodm
    FINSTA_API_BASE=http://127.0.0.1:8766/api/v1/ FINSTA_API_HOST=http://127.0.0.1:8765/api/v1/ cargo run

Sign in with the session cookie "1000%3Aabc%3A1". Needs Pillow for the
generated placeholder images.
"""
import io
import json
import re
import sys
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import urlparse, parse_qs

from PIL import Image, ImageDraw

PORT = int(sys.argv[1]) if len(sys.argv) > 1 else 8765
NO_DM = len(sys.argv) > 3 and sys.argv[3] == "nodm"
BASE = f"http://127.0.0.1:{PORT}"
NOW = int(time.time())
ME = "1000"
LOG = []

COLORS = [(232, 93, 117), (88, 140, 240), (80, 190, 140), (245, 180, 60), (150, 100, 220), (60, 180, 200), (230, 120, 60)]


def img(name, w=1080, h=1080):
    return f"{BASE}/img/{name}_{w}x{h}.jpg"


def user(pk, username, full, following=True):
    return {
        "pk": int(pk), "pk_id": str(pk), "username": username, "full_name": full,
        "profile_pic_url": img(f"avatar{pk}", 150, 150), "is_private": False, "is_verified": pk == "1003",
        "friendship_status": {"following": following},
    }


USERS = {
    "1000": user("1000", "you", "Your Name"),
    "1001": user("1001", "marta.rossi", "Marta Rossi"),
    "1002": user("1002", "luca_b", "Luca Bianchi"),
    "1003": user("1003", "natgeo_fan", "Giulia Verdi"),
    "1004": user("1004", "sam.k", "Sam Keller"),
    "1005": user("1005", "anna.photo", "Anna Moretti"),
    "2001": user("2001", "brand_shop", "Brand Shop", following=False),
    "2002": user("2002", "random_reels", "Random Reels", following=False),
}


def media(mid, pk, caption, ago, w=1080, h=1350, likes=120, comments=8, mtype=1, carousel=None):
    m = {
        "id": f"{mid}_{pk}", "pk": int(mid), "code": f"C{mid}xyz", "media_type": mtype,
        "taken_at": NOW - ago, "user": USERS[pk], "caption": {"text": caption} if caption else None,
        "like_count": likes, "comment_count": comments, "has_liked": False,
        "original_width": w, "original_height": h,
        "image_versions2": {"candidates": [{"url": img(f"post{mid}", w, h), "width": w, "height": h}]},
    }
    if carousel:
        m["media_type"] = 8
        m["carousel_media"] = [
            {"id": f"{mid}{i}", "media_type": 1, "original_width": w, "original_height": h,
             "image_versions2": {"candidates": [{"url": img(f"post{mid}c{i}", w, h), "width": w, "height": h}]}}
            for i in range(carousel)
        ]
        del m["image_versions2"]
    return m


def timeline():
    items = [
        {"media_or_ad": media("501", "1001", "Sunday hike above the lake 🏔️ Perfect weather, tired legs. https://example.com/trail", 3600 * 2, likes=342, comments=17)},
        {"media_or_ad": {**media("900", "2001", "BUY NOW — sponsored", 100), "injected": {"label": "Sponsored"}, "ad_id": "1"}},
        {"suggested_users": {"suggestions": []}},
        {"media_or_ad": media("502", "1002", "New desk setup finally done", 3600 * 5, w=1080, h=1080, likes=88, comments=4, carousel=3)},
        {"media_or_ad": media("503", "1005", "Golden hour in Bologna. Shot on film, developed at home — this roll came out better than I hoped. More from the series soon.", 3600 * 9, w=1080, h=720, likes=1530, comments=61)},
        {"media_or_ad": media("901", "2002", "you might like this (not followed)", 3600)},
        {"media_or_ad": media("504", "1004", "", 86400 + 3600, w=1080, h=1350, likes=12, comments=0)},
        {"end_of_feed_demarcator": {"title": "You're all caught up"}},
        {"media_or_ad": media("902", "2002", "suggested post after demarcator", 3600)},
    ]
    return {"feed_items": items, "more_available": True, "next_max_id": "page2", "status": "ok"}


def story_items(pk, n):
    return [{"id": f"s{pk}{i}_{pk}", "media_type": 1, "taken_at": NOW - 3600 * (i + 1),
             "original_width": 1080, "original_height": 1920,
             "image_versions2": {"candidates": [{"url": img(f"story{pk}{i}", 1080, 1920), "width": 1080, "height": 1920}]}}
            for i in range(n)]


def reel(pk, seen=False):
    latest = NOW - 3600
    return {"id": int(pk), "user": USERS[pk], "latest_reel_media": latest, "seen": latest if seen else 0}


def dm(item_id, uid, ago, itype, **kw):
    return {"item_id": item_id, "user_id": int(uid), "timestamp": str((NOW - ago) * 1_000_000), "item_type": itype, **kw}


THREADS = {
    "340282366841710300949128100000000001": {
        "thread_id": "340282366841710300949128100000000001", "thread_title": "", "is_group": False,
        "users": [USERS["1001"]], "last_activity_at": (NOW - 300) * 1_000_000,
        "last_seen_at": {ME: {"item_id": "m0", "timestamp": str((NOW - 9000) * 1_000_000)}},
        "items": [
            dm("r3", "1001", 240, "raven_media", visual_media={"view_mode": "once", "seen_user_ids": [], "seen_count": 0, "media": {"media_type": 1, "original_width": 1080, "original_height": 1920, "image_versions2": {"candidates": [{"url": img("viewonce", 1080, 1920), "width": 1080, "height": 1920}]}}}),
            dm("r2", "1001", 260, "raven_media", visual_media={"view_mode": "replayable", "seen_user_ids": [], "seen_count": 0, "media": {"media_type": 1, "original_width": 1080, "original_height": 1920, "image_versions2": {"candidates": [{"url": img("replayable", 1080, 1920), "width": 1080, "height": 1920}]}}}),
            dm("r1", ME, 280, "raven_media", visual_media={"view_mode": "once", "seen_user_ids": ["1001"], "seen_count": 1, "media": {"media_type": 1}, "expiring_media_action_summary": {"type": "raven_opened", "count": 1}}),
            dm("m9", "1001", 300, "text", text="Are you coming tonight? 🍕"),
            dm("r0", "1001", 310, "raven_media", visual_media={"view_mode": "once", "seen_user_ids": [ME], "seen_count": 1, "media": {"media_type": 2}}),
            dm("m8", "1001", 320, "media_share", media_share=media("503", "1005", "Golden hour in Bologna", 3600 * 9, w=1080, h=720)),
            dm("m7", ME, 3000, "like", like="❤️"),
            dm("m6", "1001", 3100, "media", media={"media_type": 1, "original_width": 1080, "original_height": 1350, "image_versions2": {"candidates": [{"url": img("dmphoto", 1080, 1350), "width": 1080, "height": 1350}]}}),
            dm("m5", ME, 3200, "text", text="Haha that's great. Check this out: https://gnome.org", reactions={"emojis": [{"emoji": "😂", "sender_id": 1001}]}),
            dm("m4", "1001", 3300, "clip", clip={"clip": {**media("777", "2002", "a reel someone shared", 5000), "product_type": "clips"}}),
            dm("m3", "1001", 3400, "text", text="Morning! Did you see the photos from Saturday?"),
            dm("m2", ME, 90000, "text", text="Talk tomorrow!"),
            dm("m1", "1001", 90500, "action_log", action_log={"description": "Marta liked a message"}),
        ],
        "has_older": True, "oldest_cursor": "cur1",
    },
    "340282366841710300949128100000000002": {
        "thread_id": "340282366841710300949128100000000002", "thread_title": "Weekend trip 🚐", "is_group": True,
        "users": [USERS["1002"], USERS["1004"], USERS["1005"]], "last_activity_at": (NOW - 4000) * 1_000_000,
        "last_seen_at": {ME: {"item_id": "g3", "timestamp": str((NOW - 4000) * 1_000_000)}},
        "items": [
            dm("g3", "1004", 4000, "text", text="I'll bring the tent"),
            dm("g2", "1002", 4200, "text", text="Who has a car?"),
            dm("g1", ME, 4300, "text", text="Saturday works for me"),
        ],
        "has_older": False,
    },
    "340282366841710300949128100000000003": {
        "thread_id": "340282366841710300949128100000000003", "thread_title": "", "is_group": False,
        "users": [USERS["1003"]], "last_activity_at": (NOW - 200000) * 1_000_000,
        "last_seen_at": {ME: {"item_id": "x1", "timestamp": str((NOW - 200000) * 1_000_000)}},
        "items": [dm("x1", ME, 200000, "text", text="Thanks for the tip!")],
        "has_older": False,
    },
}

SENT = []


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *a):
        pass

    def send_json(self, obj, status=200):
        body = json.dumps(obj).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Set-Cookie", "csrftoken=mockcsrf; Path=/")
        self.end_headers()
        self.wfile.write(body)

    def serve_image(self, name):
        m = re.match(r"(.+)_(\d+)x(\d+)\.jpg", name)
        label, w, h = m.group(1), int(m.group(2)), int(m.group(3))
        scale = 2
        im = Image.new("RGB", (w // scale, h // scale), COLORS[hash(label) % len(COLORS)])
        d = ImageDraw.Draw(im)
        c2 = COLORS[(hash(label) + 3) % len(COLORS)]
        for i in range(0, h // scale, 12):
            d.line([(0, i), (w // scale, i + w // (scale * 3))], fill=c2, width=4)
        d.text((20, 20), label, fill=(255, 255, 255))
        buf = io.BytesIO()
        im.save(buf, "JPEG", quality=80)
        body = buf.getvalue()
        self.send_response(200)
        self.send_header("Content-Type", "image/jpeg")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def route(self, method):
        u = urlparse(self.path)
        q = parse_qs(u.query)
        path = u.path
        body = {}
        if method == "POST":
            n = int(self.headers.get("Content-Length", 0))
            body = parse_qs(self.rfile.read(n).decode())
        LOG.append(f"{method} {path} {dict(q)} {body}")
        with open(sys.argv[2] if len(sys.argv) > 2 else "/dev/null", "a") as f:
            f.write(f"{method} :{PORT}{path} q={dict(q)} body={body} cookie={self.headers.get('Cookie')} csrf={self.headers.get('X-CSRFToken')} app={self.headers.get('X-IG-App-ID')} xhr={self.headers.get('X-Requested-With')} origin={self.headers.get('Origin')} ua={self.headers.get('User-Agent')}\n")
        if path.startswith("/img/"):
            return self.serve_image(path[5:])
        p = path.replace("/api/v1/", "")
        if NO_DM and p.startswith("direct_v2/"):
            # Like instagram.com if it stopped serving a route: an HTML 404 page.
            body = b"<!DOCTYPE html><html><head><title>Page Not Found \xe2\x80\xa2 Instagram</title></head></html>"
            self.send_response(404)
            self.send_header("Content-Type", "text/html")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
            return
        if m := re.match(r"users/(\d+)/info/", p):
            u = dict(USERS.get(m.group(1), USERS["1001"]))
            u.update({"biography": "Photos, mountains & coffee ☕\nBologna 🇮🇹", "external_url": "https://example.com", "follower_count": 1234, "following_count": 321, "media_count": 15})
            return self.send_json({"user": u, "status": "ok"})
        if re.match(r"friendships/\d+/following/", p):
            return self.send_json({"users": [USERS[k] for k in ["1001", "1002", "1003", "1004", "1005"]], "status": "ok"})
        if p == "feed/timeline/":
            if body.get("max_id"):
                return self.send_json({"feed_items": [], "more_available": False, "status": "ok"})
            return self.send_json(timeline())
        if p == "feed/reels_tray/":
            return self.send_json({"tray": [reel("1000"), reel("1001"), reel("2001"), reel("1003"), reel("1005", seen=True), reel("1002")], "status": "ok"})
        if p == "feed/reels_media/":
            ids = q.get("reel_ids", [])
            return self.send_json({"reels": {i: {**reel(i), "items": story_items(i, 3)} for i in ids}, "status": "ok"})
        if p == "direct_v2/inbox/":
            threads = []
            for t in THREADS.values():
                tt = dict(t)
                tt["items"] = t["items"][:5]
                threads.append(tt)
            return self.send_json({"inbox": {"threads": threads, "has_older": False}, "viewer": USERS[ME], "status": "ok"})
        if m := re.match(r"direct_v2/visual_threads/(\d+)/(item_seen|item_replayed)/", p):
            ids = body.get("item_ids", ["[]"])[0].strip("[]").split(",")
            for item in THREADS[m.group(1)]["items"]:
                if item["item_id"] in ids and "visual_media" in item:
                    v = item["visual_media"]
                    if ME not in v["seen_user_ids"]:
                        v["seen_user_ids"].append(ME)
                    v["seen_count"] += 1
                    if v["view_mode"] == "once" or m.group(2) == "item_replayed":
                        v["media"] = {"media_type": v["media"]["media_type"]}
            return self.send_json({"status": "ok"})
        if m := re.match(r"direct_v2/threads/(\d+)/items/.+/seen/", p):
            return self.send_json({"status": "ok"})
        if m := re.match(r"direct_v2/threads/(\d+)/$", p):
            t = dict(THREADS[m.group(1)])
            items = SENT + t["items"] if m.group(1).endswith("1") else t["items"]
            if q.get("cursor"):
                t["items"] = [dm("old1", "1001", 400000, "text", text="(older message)")]
                t["has_older"] = False
                t["oldest_cursor"] = None
            else:
                t["items"] = items
            return self.send_json({"thread": t, "status": "ok"})
        if p.startswith("direct_v2/threads/broadcast/"):
            text = body.get("text", body.get("link_text", [""]))[0]
            SENT.insert(0, dm(f"sent{len(SENT)}", ME, 0, "text", text=text))
            return self.send_json({"action": "item_ack", "status_code": "200", "payload": {"thread_id": "340282366841710300949128100000000001", "item_id": "x"}, "status": "ok"})
        if p == "direct_v2/threads/get_by_participants/":
            return self.send_json({"status": "ok"})
        if m := re.match(r"friendships/show/(\d+)/", p):
            return self.send_json({"following": m.group(1) in USERS and m.group(1) < "2000", "followed_by": True, "status": "ok"})
        if m := re.match(r"feed/user/(\d+)/", p):
            pk = m.group(1) if m.group(1) in USERS else "1001"
            start = 0 if not q.get("max_id") else 12
            items = [media(str(600 + start + i), pk, f"post {start + i}", 86400 * (i + 1), w=1080, h=1080, carousel=3 if i % 5 == 1 else None) for i in range(12 if start == 0 else 3)]
            return self.send_json({"items": items, "more_available": start == 0, "next_max_id": "m2" if start == 0 else None, "status": "ok"})
        if p == "web/search/topsearch/":
            return self.send_json({"users": [{"position": i, "user": USERS[k]} for i, k in enumerate(["1001", "1005", "2002"])], "places": [], "hashtags": [], "status": "ok"})
        if m := re.match(r"friendships/(create|destroy)/(\d+)/", p):
            return self.send_json({"friendship_status": {"following": m.group(1) == "create", "followed_by": True, "outgoing_request": False}, "status": "ok"})
        if re.match(r"web/likes/\d+/(like|unlike)/", p):
            return self.send_json({"status": "ok"})
        if re.match(r"web/comments/\d+/add/", p):
            me = USERS[ME]
            return self.send_json({"id": "9001", "text": body.get("comment_text", [""])[0], "created_time": NOW, "from": {"id": ME, "username": me["username"], "full_name": me["full_name"], "profile_picture": me["profile_pic_url"]}, "status": "ok"})
        if p == "web/accounts/logout/ajax/":
            return self.send_json({"status": "ok"})
        if re.match(r"media/.+/comments/", p):
            return self.send_json({"comments": [
                {"pk": 1, "text": "Wow, stunning! 😍", "user": USERS["1002"], "created_at": NOW - 5000, "comment_like_count": 3},
                {"pk": 2, "text": "Where is this?", "user": USERS["1004"], "created_at": NOW - 4000},
            ], "status": "ok"})
        if re.match(r"media/.+/(like|unlike)/", p):
            return self.send_json({"status": "ok"})
        return self.send_json({"status": "fail", "message": f"mock: unknown {p}"}, 404)

    def do_GET(self):
        self.route("GET")

    def do_POST(self):
        self.route("POST")


ThreadingHTTPServer(("127.0.0.1", PORT), Handler).serve_forever()
