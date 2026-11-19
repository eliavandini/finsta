"""A fake Instagram API for working on Finsta's UI without a real account.

    python3 dev/mock_api.py 8765 [request-log-file]
    FINSTA_API_BASE=http://127.0.0.1:8765/api/v1/ XDG_CONFIG_HOME=/tmp/finsta-dev cargo run

Direct messages work like on instagram.com: /direct/inbox/ is a page that
embeds a Lightspeed payload, and /chat is the MQTT-over-WebSocket chat
connection. Point Finsta at them too:

    FINSTA_WEB_BASE=http://127.0.0.1:8765/ FINSTA_CHAT_URL=ws://127.0.0.1:8765/chat

Like instagram.com today, the inbox page doesn't carry the conversations:
Finsta finds the GraphQL query that loads them in the inbox worker's
scripts. Add --embed to put them in the page instead, or --no-graphql to
make that query fail so they come over the chat connection.

With "www" as the third argument, users/{id}/info/ answers 429, as
instagram.com did for web sessions in November 2026.

Sign in with the session cookie "1000%3Aabc%3A1". Needs Pillow for the
generated placeholder images.

Songs on photo posts and photo stories play the audio file in
FINSTA_MOCK_AUDIO, if set. GET /mock/incoming?key=30001&text=Hi makes a new
message arrive in that conversation (30002, the group, is muted).
"""
import base64
import hashlib
import io
import json
import math
import os
import re
import struct
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import urlparse, parse_qs

from PIL import Image, ImageDraw

PORT = int(sys.argv[1]) if len(sys.argv) > 1 else 8765
WWW = len(sys.argv) > 3 and sys.argv[3] == "www"
BASE = f"http://127.0.0.1:{PORT}"
NOW = int(time.time())
ME = "1000"
LOG = []

COLORS = [(232, 93, 117), (88, 140, 240), (80, 190, 140), (245, 180, 60), (150, 100, 220), (60, 180, 200), (230, 120, 60)]


def img(name, w=1080, h=1080):
    return f"{BASE}/img/{name}_{w}x{h}.jpg"


def reply(parent, i):
    """The i-th reply (from 0) to comment `parent`."""
    return {"pk": parent * 1000 + i, "text": f"@someone reply {i + 1} to comment {parent}", "user": USERS["1004" if i % 2 else "1005"],
            "created_at": NOW - 3000 + 60 * i, "parent_comment_id": parent}


def anim(name, w=200, h=200):
    """An animated WebP (a ball going round, see-through around it), like GIFs and stickers."""
    return f"{BASE}/anim/{name}_{w}x{h}.webp"


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
        "like_count": likes, "comment_count": comments, "has_liked": False, "has_viewer_saved": SAVED.get(str(mid), False),
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


VIDEOS = sorted(os.listdir(os.environ["FINSTA_MOCK_VIDEOS"])) if os.environ.get("FINSTA_MOCK_VIDEOS") else []
AUDIO = os.environ.get("FINSTA_MOCK_AUDIO")
MUTED_CONVOS = {30002}


def with_song_audio(m):
    """What the app's API adds to a photo's music: where its audio is, and
    which part plays (instagram.com's queries leave this out)."""
    m = json.loads(json.dumps(m))
    music = m.get("music_metadata") or {}
    stickers = m.get("story_music_stickers") or []
    info = (music.get("music_info") or {}).get("music_asset_info") if music else (stickers[0]["music_asset_info"] if stickers else None)
    if info is not None and AUDIO:
        info["progressive_download_url"] = f"{BASE}/aud/song.ogg"
        part = {"audio_asset_start_time_in_ms": 2000, "overlap_duration_in_ms": 8000}
        if music:
            music["music_info"]["music_consumption_info"] = part
        else:
            stickers[0]["music_consumption_info"] = part
    return m


STORY_LIKES = {}
# reel -> taken_at of the newest story you've watched; someone's first two stories are already watched.
SEEN = {"1001": NOW - 3600 * 4, "1005": NOW - 3600}


def story_items(pk, n):
    items = []
    for i in range(n):
        sid = f"9{pk}{i}"
        it = {"id": f"{sid}_{pk}", "pk": int(sid), "media_type": 1, "taken_at": NOW - 3600 * (n - i),
              "has_liked": STORY_LIKES.get(sid, False),
              "original_width": 1080, "original_height": 1920,
              "image_versions2": {"candidates": [{"url": img(f"story{pk}{i}", 1080, 1920), "width": 1080, "height": 1920}]}}
        if VIDEOS and i % 3 != 2:  # two videos, then a photo
            it["media_type"] = 2
            it["video_versions"] = [{"type": 101, "url": f"{BASE}/vid/{VIDEOS[(int(pk) + i) % len(VIDEOS)]}", "width": 720, "height": 1280}]
        if i == 2:
            it["story_music_stickers"] = [{"music_asset_info": {"title": "Summer Song", "display_artist": "The Mockers"}, "id": "m1"}]
        if i == 3:
            it["story_feed_media"] = [{"media_code": "C503xyz", "id": "503_1005", "product_type": "feed", "x": 0.5, "y": 0.5}]
            it["reshared_story_media_author"] = {"username": "natgeo_fan", "id": "1003", "profile_pic_url": USERS["1003"]["profile_pic_url"]}
        if i == 4:
            it["story_link_stickers"] = [{"story_link": {"url": "https://example.com/article"}, "id": "l1"}]
            it["story_bloks_stickers"] = [{"bloks_sticker": {"sticker_data": {"ig_mention": {"username": "luca_b", "full_name": "Luca"}}, "id": "b1"}, "id": "b1"}]
        items.append(it)
    return items


def reel(pk):
    return {"id": int(pk), "user": USERS[pk], "latest_reel_media": NOW - 3600, "seen": SEEN.get(pk, 0)}


# ----- Direct messages: instagram.com's web inbox (Lightspeed over MQTT) -----

ME_CHAT = 17840000000000000  # "you" in the chat system, which isn't your profile id
LS_VERSION = "9876543210123456"
CHAT_USERS = {20000 + int(pk) - 1000: pk for pk in ["1001", "1002", "1003", "1004", "1005"]}
CONVOS = {  # thread key -> [name, type (1 = 1:1, 2 = group), members, read watermark ms]
    30001: ["", 1, [20001], (NOW - 9000) * 1000],
    30002: ["Weekend trip 🚐", 2, [20002, 20004, 20005], (NOW - 4000) * 1000],
    30003: ["", 1, [20003], (NOW - 100) * 1000],
}
OLDER_CONVOS = {30004: ["", 1, [20005], (NOW - 864000) * 1000]}


def msg(mid, sender, ago, text="", attach=None):
    return {"mid": f"mid.$mock{mid}", "sender": sender, "ts": (NOW - ago) * 1000, "text": text, "attach": attach, "reply": None}


MESSAGES = {
    30001: [msg(f"f{i}", 20001 if i % 2 else ME_CHAT, 200000 + 600 * (40 - i), f"Older message {i}") for i in range(40)] + [
        msg("a1", 20001, 90500, "Did you see the photos from Saturday?"),
        msg("a2", ME_CHAT, 90000, "Not yet, send them!"),
        msg("a3", 20001, 3100, attach=("blob", img("dmphoto", 1080, 1350))),
        {**msg("a4", ME_CHAT, 3000, "Wow 😍"), "reactions": [("\u2764", 20001)]},
        msg("a5", 20001, 320, attach=("xma", "https://www.instagram.com/p/C503xyz/", "Golden hour in Bologna")),
        msg("a5r", 20001, 315, attach=("xma", "https://www.instagram.com/reel/C777xyz/", "A reel about mountains")),
        msg("a5s", 20001, 312, attach=("xma", "https://www.instagram.com/stories/marta.rossi/910012/?utm_source=ig", None)),
        msg("a5b", 20001, 310, attach=("raven",)),
        msg("a5c", 20001, 305, attach=("audio",)),
        msg("a5d", 20001, 304, attach=("gif", anim("hello-sticker", 200, 200), True)),
        msg("a5e", 20001, 303, attach=("cutout", anim("cutout", 160, 200))),
        {**msg("a6", 20001, 300, "Are you coming tonight? 🍕"), "reactions": [("\u2764", ME_CHAT), ("\u2764\ufe0f", 20001)]},
    ],
    30002: [msg("g1", ME_CHAT, 4300, "Saturday works for me"), msg("g2", 20002, 4200, "Who has a car?"),
            msg("g3", 20004, 4000, "I'll bring the tent")],
    30003: [msg("h1", ME_CHAT, 172800, "Thanks for the tip!")],
    30004: [msg("o1", 20005, 864000, "Long time no see!")],
}


def ls_value(v):
    if v is None:
        return [9]
    if isinstance(v, int) and not isinstance(v, bool) and abs(v) > 2**31:
        return [19, str(v)]
    return v


def ls_program(calls):
    """A Lightspeed payload calling each (procedure, {position: value})."""
    steps = []
    for name, fields in calls:
        args = [fields.get(i) for i in range(max(fields) + 3)]
        steps.append([5, name] + [ls_value(a) for a in args])
    # Wrapped in a conditional, like real payloads.
    return json.dumps({"name": None, "step": [1, [23, [2, 0], [1] + steps]]})


def contact_calls(cids):
    out = []
    for cid in cids:
        u = USERS[CHAT_USERS[cid]] if cid in CHAT_USERS else USERS[ME]
        out.append(("verifyContactRowExists", {0: cid, 1: 0, 2: u["profile_pic_url"], 3: u["full_name"], 4: 1, 20: u["username"]}))
    return out


def message_calls(key, msgs):
    out = []
    for m in msgs:
        out.append(("insertMessage", {0: m["text"], 3: key, 5: m["ts"], 8: m["mid"], 9: str(m["ts"] << 22), 10: m["sender"]}))
        a = m["attach"]
        if a and a[0] == "blob":
            out.append(("insertBlobAttachment", {3: a[1], 5: 1080, 6: 1350, 27: key, 30: m["mid"]}))
        elif a and a[0] == "xma":
            out.append(("insertXmaAttachment", {2: a[1], 4: img("xmapreview", 640, 640), 8: a[2], 27: key, 30: m["mid"]}))
    return out


def convo_calls(convos, last=5):
    out = []
    for key, (name, kind, members, read) in convos.items():
        msgs = MESSAGES[key]
        newest = msgs[-1]
        snippet = newest["text"] or "Sent an attachment"
        out.append(("deleteThenInsertThread", {0: newest["ts"], 1: read, 2: snippet, 3: name, 4: "", 7: key, 8: 0, 9: kind, 18: newest["sender"]}))
        for cid in [ME_CHAT] + members:
            out.append(("addParticipantIdToGroupThread", {0: key, 1: cid, 2: read if cid == ME_CHAT else newest["ts"]}))
        out += contact_calls([ME_CHAT] + members)
        out += message_calls(key, msgs[-last:])
    return out


DOC_ID = "7357432314358409"
INBOX_DOC = "24000000000000001"
LIST_DOC = "24000000000000002"
MESSAGES_DOC = "24000000000000003"
SEND_DOC = "24000000000000004"
MARK_DOC = "24000000000000005"
WEB_DOCS = {"25000000000000006": "PolarisFeedTimelineRootV2Query", "25000000000000001": "PolarisProfilePageContentQuery", "25000000000000002": "PolarisProfilePostsQuery",
            "25000000000000003": "PolarisStoriesV3ReelPageStandaloneQuery", "25000000000000004": "usePolarisLikeMediaXIGLikeMutation",
            "25000000000000005": "usePolarisLikeMediaXIGUnlikeMutation", "25000000000000007": "PolarisAPIReelSeenMutation",
            "25000000000000008": "usePolarisStoryLikesLikeMutation", "25000000000000009": "usePolarisStoryLikesUnlikeMutation",
            "25000000000000010": "PolarisPostRootQuery", "25000000000000011": "PolarisSearchBoxRefetchableQuery",
            "25000000000000012": "IGDOmniPickerSearchResultsListQuery", "25000000000000013": "usePolarisSaveMediaSaveMutation",
            "25000000000000014": "usePolarisSaveMediaUnsaveMutation", "25000000000000015": "PolarisSavedCollectionPickerQuery"}
SAVED = {"703": True}  # media pk -> saved
FOLDERS = {"17841": ["Recipes", set()], "17842": ["Travel", set()]}
SAVE_FAILS = "--save-fails" in sys.argv  # the website's save mutation fails, so the older route is used
# Saved-style definitions: what the app reads to fill in and fit inputs.
WEB_MODULES = (
    '__d("PolarisFeedTimelineRootV2Query.graphql",["PolarisFeedTimelineRootV2Query_instagramRelayOperation"],(function(t,n,r,o,a,i){var e=(function(){'
    'var e={defaultValue:null,kind:"LocalArgument",name:"after"},t={defaultValue:null,kind:"LocalArgument",name:"before"},'
    'r={defaultValue:null,kind:"LocalArgument",name:"data"},o={defaultValue:null,kind:"LocalArgument",name:"first"},'
    'l={defaultValue:null,kind:"LocalArgument",name:"last"},s={defaultValue:null,kind:"LocalArgument",name:"variant"};'
    'return{fragment:{argumentDefinitions:[e,t,r,o,l,s]}}})();a.exports=e}),null);'

    '__d("PolarisProfilePostsQuery.graphql",["PolarisProfilePostsQuery_instagramRelayOperation","PolarisShortDramaEnabled.relayprovider"],'
    '(function(t,n,r,o,a,i){"use strict";var e=(function(){var e={defaultValue:null,kind:"LocalArgument",name:"data"},'
    't={defaultValue:null,kind:"LocalArgument",name:"username"},r={defaultValue:null,kind:"LocalArgument",name:"first"},'
    'o={defaultValue:null,kind:"LocalArgument",name:"after"},l={defaultValue:null,kind:"LocalArgument",name:"before"},'
    's={defaultValue:null,kind:"LocalArgument",name:"last"},u={defaultValue:null,kind:"LocalArgument",name:"__relay_internal__pv__PolarisShortDramaEnabledrelayprovider"};'
    'return{fragment:{argumentDefinitions:[e,t,r,o,l,s,u]},params:{providedVariables:{__relay_internal__pv__PolarisShortDramaEnabledrelayprovider:n("PolarisShortDramaEnabled.relayprovider")}}}})();a.exports=e}),null);'
    + "".join(
        f'__d("usePolarisLikeMediaXIG{kind}Mutation.graphql",["usePolarisLikeMediaXIG{kind}Mutation_instagramRelayOperation"],'
        '(function(t,n,r,o,a,i){"use strict";var e=(function(){var e={defaultValue:null,kind:"LocalArgument",name:"media_id"};'
        'return{fragment:{argumentDefinitions:[e]}}})();a.exports=e}),null);'.replace('name:"media_id"', 'name:"input"') for kind in ("Like", "Unlike"))
    + "".join(
        f'__d("usePolarisStoryLikes{kind}Mutation.graphql",[],(function(t,n,r,o,a,i){{var e={{defaultValue:null,kind:"LocalArgument",name:"input"}};'
        'a.exports={fragment:{argumentDefinitions:[e]}}}),null);' for kind in ("Like", "Unlike"))
    + '__d("PolarisAPIReelSeenMutation.graphql",[],(function(t,n,r,o,a,i){var e=["reelId","reelMediaId","reelMediaOwnerId","reelMediaTakenAt","viewSeenAt"]'
      '.map(function(x){return{defaultValue:null,kind:"LocalArgument",name:x}});'
      'a.exports={fragment:{argumentDefinitions:[{defaultValue:null,kind:"LocalArgument",name:"reelId"},{defaultValue:null,kind:"LocalArgument",name:"reelMediaId"},'
      '{defaultValue:null,kind:"LocalArgument",name:"reelMediaOwnerId"},{defaultValue:null,kind:"LocalArgument",name:"reelMediaTakenAt"},'
      '{defaultValue:null,kind:"LocalArgument",name:"viewSeenAt"}]}}}),null);'
)
REPLY_DOC = "24000000000000006"
REACT_DOC = "24000000000000007"
SHARE_DOC = "24000000000000008"
GIF_DOC = "24000000000000009"
GIPHY_KEY = "mockgiphykey"  # FINSTA_GIPHY_BASE=http://127.0.0.1:8765/giphy/ and this key in Preferences (GIFs and stickers)
SLIDE_DOCS = {INBOX_DOC: "inbox", LIST_DOC: "more threads", MESSAGES_DOC: "messages", SEND_DOC: "send", MARK_DOC: "mark read", REPLY_DOC: "story reply",
              REACT_DOC: "react", SHARE_DOC: "story share", GIF_DOC: "gif"}
REPLY_FAILS = "--reply-fails" in sys.argv  # story replies fail natively (as on instagram.com in November 2026)
OFFMSYS = "--offmsys" in sys.argv  # instagram.com as of November 2026
EMBED = "--embed" in sys.argv  # put the conversations in the page, as instagram.com did until late 2026


def base():
    return f"http://127.0.0.1:{PORT}"


def inbox_page():
    worker = f"{base()}/rsrc.php/v4/yW/r/IGDAWMainV4Worker.js"
    defines = [
        # The chat refuses this id (the profile id), so Finsta must fall back to USER_ID.
        ["MqttWebConfig", [], {"fbid": "99999999999", "appID": 936619743392459, "endpoint": f"ws://127.0.0.1:{PORT}/chat",
                               "subscribedTopics": [], "capabilities": 10, "chatVisibility": True}, 1],
        ["MqttWebDeviceID", [], {"clientID": "6f1d2c3b-0000-4000-8000-123456789abc"}, 2],
        ["IGDMqttWebDeviceID", [], {"clientId": "ab12cd34-0000-4000-8000-ffffffffffff"}, 9],
        ["CurrentUserInitialData", [], {"USER_ID": str(ME_CHAT), "ACCOUNT_ID": str(ME_CHAT), "IG_USER_EIMU": str(ME_CHAT),
                                        "NON_FACEBOOK_USER_ID": ME, "NAME": "Your Name", "APP_ID": "936619743392459"}, 3],
        ["LSPlatformMessengerSyncParams", [], {"mailbox": json.dumps({"locale": "en_US"}), "contact": json.dumps({"locale": "en_US"}), "e2ee": "{}"}, 4],
        ["DTSGInitialData", [], {"token": "mockdtsg"}, 5],
        ["LSD", [], {"token": "mocklsd"}, 6],
        ["SiteData", [], {"server_revision": 1012345678, "haste_session": "20400.HYP:instagram_web_pkg.2.1...0", "hsi": "7400000000000000000",
                          "__spin_r": 1012345678, "__spin_b": "trunk", "__spin_t": NOW}, 7],
        ["IGDAWMainV4WebWorkerResource", [], {"url": worker, "name": "IGDAWMainV4"}, 8],
    ]
    require = [["PolarisDirectInboxRoot", "init", [], [{"preloaderID": "adp_PolarisDirectInboxQueryRelayPreloader_6720a2b1c3e5f9_0"}]]]
    if EMBED:
        calls = convo_calls(CONVOS) + [("upsertSyncGroupThreadsRange", {0: 1, 1: 1, 2: True})]
        require.append(["RelayPrefetchedStreamCache", "next", [], ["adp_inbox", {"__bbox": {"complete": True, "result": {"data": {"viewer": {
            "lightspeed_web_request_for_igd": {"payload": ls_program(calls), "dependencies": []}}}}}}]])
    data = {"require": [["ScheduledServerJS", "handle", None, [{"__bbox": {"define": defines, "require": require}}]]]}
    preload = "".join(f'<link rel="preload" href="{base()}/rsrc.php/v4/y{i}/r/bundle{i}.js" as="script">' for i in range(12))
    return (f'<!DOCTYPE html><html><head><title>Inbox • Direct</title>{preload}</head><body>'
            f'<script type="application/json" data-sjs>{json.dumps(data)}</script>'
            f'</body></html>')


def script(name):
    if name == "IGDAWMainV4Worker.js":
        return f'self.importScripts("{base()}/rsrc.php/v4/yL/r/LSWorkerBundle.js");'
    if name == "LSWorkerBundle.js" and OFFMSYS:
        # Like instagram.com in November 2026: no Lightspeed, plain GraphQL operations.
        return ('__d("PolarisDirectInboxQuery_instagramRelayOperation",[],(function(a,b,c,d,e,f){e.exports="' + INBOX_DOC + '"}),null);'
                '__d("IGDThreadListOffMsysPaginationQuery_instagramRelayOperation",[],(function(a,b,c,d,e,f){e.exports="' + LIST_DOC + '"}),null);'
                '__d("IGDMessageListOffMsysQuery_instagramRelayOperation",[],(function(a,b,c,d,e,f){e.exports="' + MESSAGES_DOC + '"}),null);'
                '__d("IGDirectTextSendMutation_instagramRelayOperation",[],(function(a,b,c,d,e,f){e.exports="' + SEND_DOC + '"}),null);'
                '__d("useIGDMarkThreadAsReadMutation_instagramRelayOperation",[],(function(a,b,c,d,e,f){e.exports="' + MARK_DOC + '"}),null);'
                '__d("IGDirectStoryShareReplyMutation_instagramRelayOperation",[],(function(a,b,c,d,e,f){e.exports="' + REPLY_DOC + '"}),null);'
                '__d("IGDirectReactionSendMutation_instagramRelayOperation",[],(function(a,b,c,d,e,f){e.exports="' + REACT_DOC + '"}),null);'
                '__d("IGDirectStoryShareMutation_instagramRelayOperation",[],(function(a,b,c,d,e,f){e.exports="' + SHARE_DOC + '"}),null);'
                '__d("IGDirectAnimatedMediaSendMutation_instagramRelayOperation",[],(function(a,b,c,d,e,f){e.exports="' + GIF_DOC + '"}),null);'
                + "".join(f'__d("{n}_instagramRelayOperation",[],(function(a,b,c,d,e,f){{e.exports="{d}"}}),null);' for d, n in WEB_DOCS.items())
                + WEB_MODULES +
                '__d("IGDMaxUnreadMessagesCount.relayprovider",[],(function(t,n,r,o,a,i,l){l.get=function(){return 20}}),null);'
                '__d("useIGDSendText",["IGDirectTextSendMutation.graphql"],(function(t,n,r,o,a,i,l){function s(e){return{ig_thread_igid:e.id,text:e.text}}l.default=s}),98);'
                '__d("IGDThreadListOffMsysPaginationQuery.graphql",["IGDThreadListOffMsysPaginationQuery_instagramRelayOperation"],(function(t,n,r,o,a,i){"use strict";'
                'var e=function(){var e={defaultValue:null,kind:"LocalArgument",name:"after"},t={defaultValue:20,kind:"LocalArgument",name:"first"},'
                'n={defaultValue:null,kind:"LocalArgument",name:"folder"};return{fragment:{argumentDefinitions:[e,t,n],kind:"Fragment",name:"IGDThreadListOffMsysPaginationQuery"}}}();a.exports=e}),null);'
                '__d("PolarisDirectInboxQuery.graphql",[],(function(t,n,r,o,a,i){"use strict";var e={defaultValue:null,kind:"LocalArgument",name:"thread_count"};a.exports={fragment:{argumentDefinitions:[e]}}}),null);')
    if name == "LSWorkerBundle.js":
        return ('__d("LSVersion",[],(function(a,b,c,d,e,f){e.exports="' + LS_VERSION + '"}),66);'
                '__d("LSPlatformGraphQLLightspeedRequestForIGDQuery_instagramRelayOperation",[],(function(a,b,c,d,e,f){e.exports="' + DOC_ID + '"}),null);'
                '__d("IGDInboxTrayQuery_instagramRelayOperation",[],(function(a,b,c,d,e,f){e.exports="1234567890123"}),null);'
                'function s(){return{ls_fdid:"",ls_sv:c("LSVersion")}}var q=b("IGDThreadMessagesQuery");')
    return f'__d("{name[:-3]}",[],(function(a,b,c,d,e,f){{e.exports={{}}}}),null);' * 50


def inbox_snapshot():
    return ls_program(convo_calls(CONVOS) + [("upsertSyncGroupThreadsRange", {0: 1, 1: 1, 2: True})])


# ----- instagram.com's current inbox ("Slide", GraphQL) -----

def slide_user(cid):
    u = USERS[CHAT_USERS[cid]]
    return {"id": u["pk_id"], "username": u["username"], "full_name": u["full_name"], "profile_pic_url": u["profile_pic_url"],
            "is_verified": u["is_verified"], "interop_messaging_user_fbid": str(cid)}


def slide_content(m):
    a = m["attach"]
    if not a:
        return {"__typename": "SlideMessageText", "text_body": m["text"]}
    if a[0] == "blob":
        return {"__typename": "SlideMessageImageContent", "attachments": [{"attachment_fbid": "1", "preview_cdn_url": a[1], "attachment_cdn_url": a[1],
                                                                             "preview_width": 1080, "preview_height": 1350}]}
    if a[0] == "gif":
        return {"__typename": "SlideMessageAnimatedMediaContent", "animated_media": [{"attachment_webp_url": a[1], "preview_width": 200,
                                                                                      "preview_height": 200 if a[2:] == (True,) else 150, "is_sticker": a[2:] == (True,)}]}
    if a[0] == "cutout":
        return {"__typename": "SlideMessageCutoutStickerXMAContent", "preview_url": a[1], "preview_width": 160, "preview_height": 200}
    if a[0] == "raven":
        return {"__typename": "SlideMessageRavenImageContent", "attachment": {"__typename": "XFBSlideAttachment", "preview_cdn_url": img("secret", 100, 100)}}
    if a[0] == "audio":
        return {"__typename": "SlideMessageAudiosContent", "audio_attachments": [{"attachment_cdn_url": f"{BASE}/vid/{VIDEOS[0]}" if VIDEOS else "x", "playable_duration_ms": 4000}]}
    reel_link = "/reel/" in a[1] or "/stories/" in a[1]
    w, h = (720, 1280) if reel_link else (640, 640)
    return {"__typename": "SlideMessageXMAContent", "xma_text_body": None,
            "xma": {"__typename": "SlideMessagePortraitXMA" if reel_link else "SlideMessageStandardXMA", "target_url": a[1],
                    "header_title_text": "natgeo_fan", "title_text": a[2],
                    "preview_image": {"url": img("xmapreview" + ("reel" if reel_link else ""), w, h), "width": w, "height": h}}}


def slide_message(m):
    return {"id": "node" + m["mid"], "message_id": m["mid"], "sender_fbid": str(m["sender"]), "timestamp_ms": str(m["ts"]), "__typename": "SlideMessage",
            "is_reported": False, "tombstone_reason": None, "content": slide_content(m), "igd_snippet": m["text"] or "sent an attachment",
            "reactions": [{"reaction": e, "sender_fbid": str(s), "reaction_timestamp_ms": str(m["ts"] + 1000)} for e, s in m.get("reactions", [])],
            "replied_to_message": m["reply"] and {"id": "node" + m["reply"]["mid"], "sender_fbid": str(m["reply"]["sender"]), "content": slide_content(m["reply"])}}


def slide_thread(key, convos):
    name, kind, members, read = convos[key]
    msgs = MESSAGES[key]
    return {"node": {"id": f"slide{key}", "__typename": "SlideThread", "as_ig_direct_thread": {
        "id": f"t{key}", "thread_id": f"34028236684171030{key}", "thread_key": str(key), "thread_fbid": str(key),
        "is_group": kind == 2, "thread_title": name, "thread_image_url": None,
        "last_activity_timestamp_ms": str(msgs[-1]["ts"]), "users": [slide_user(c) for c in members],
        "viewer": {"id": ME, "interop_messaging_user_fbid": str(ME_CHAT), "profile_pic_url": USERS[ME]["profile_pic_url"]},
        "marked_as_unread": False, "is_muted": key in MUTED_CONVOS,
        "slide_read_receipts": [{"participant_fbid": str(ME_CHAT), "watermark_timestamp_ms": str(read)}],
        "slide_messages": {"edges": [{"node": slide_message(m), "cursor": m["mid"]} for m in reversed(msgs[-2:])],
                           "page_info": {"end_cursor": None, "has_next_page": False}}}}, "cursor": f"c{key}"}


def slide_answer(op, v):
    if op == "mark read":
        key = next((k for k in MESSAGES if f"34028236684171030{k}" == v["metadata"]["ig_thread_igid"]), None)
        m = next((m for m in MESSAGES.get(key, []) if m["mid"] == v["data"]["message_id"]), None)
        if m is None or v["data"].get("item_id") != "":
            return {"errors": [{"message": "Invalid message", "severity": "CRITICAL", "code": 1675030}], "data": None}
        CONVOS[key][3] = max(CONVOS[key][3], m["ts"])
        return {"data": {"xig_direct_item_seen_mutation_with_slide_messaging_response": {
            "__typename": "XFBSlideReadReceipt", "participant_eimu_id": str(ME_CHAT), "watermark_timestamp_ms": str(m["ts"])}}}
    if op == "inbox":
        threads = [slide_thread(k, CONVOS) for k in CONVOS]
        return {"data": {"get_slide_mailbox_for_iris_subscription": {"__typename": "SlideMailbox", "id": "mailbox1",
                "iris_inactive_subscription_uq_seq_id": "1", "threads_by_folder": {"edges": threads,
                "page_info": {"end_cursor": "page2", "has_next_page": True}}}}, "extensions": {"is_final": True}}
    if op == "more threads":
        assert v["id"] == "mailbox1" and v["cursor"] == "page2", v
        return {"data": {"fetch__SlideMailbox": {"id": "mailbox1", "threads_by_folder": {"edges": [slide_thread(k, OLDER_CONVOS) for k in OLDER_CONVOS],
                "page_info": {"end_cursor": None, "has_next_page": False}}}}}
    if op == "messages":
        key = int(v["id"].removeprefix("slide"))
        newest_first = list(reversed(MESSAGES[key]))
        start = int(v["after"]) if v.get("after") else 0
        page = newest_first[start:start + v["first"]]
        more = start + v["first"] < len(newest_first)
        return {"data": {"fetch__SlideThread": {"id": v["id"], "as_ig_direct_thread": {"id": f"t{key}", "slide_messages": {
                "edges": [{"node": slide_message(m), "cursor": m["mid"]} for m in page],
                "page_info": {"end_cursor": str(start + v["first"]) if more else None, "has_next_page": more,
                              "has_previous_page": start > 0, "start_cursor": str(start)}}}}}}
    if op == "react":
        # As instagram.com's IGDMAPISendReaction builds it.
        i = v.get("input") or {}
        key = int(i["thread_id"]) if str(i.get("thread_id", "")).isdigit() and int(i["thread_id"]) in MESSAGES else None
        m = next((m for m in MESSAGES.get(key, []) if "node" + m["mid"] == i.get("message_id")), None)
        mine = next((e for e, who in (m or {}).get("reactions", []) if who == ME_CHAT), None)
        if set(v) != {"input"} or set(i) != {"emoji", "item_id", "message_id", "reaction_status", "thread_id"} or m is None \
                or i["item_id"] != "" or not i.get("emoji") or i["reaction_status"] not in ("created", "deleted") \
                or (i["reaction_status"] == "deleted" and i["emoji"] != mine):
            return {"errors": [{"message": "A server error field_exception occured.", "severity": "CRITICAL", "code": 1675030}],
                    "data": {"xig_direct_reaction_send_with_slide_messaging_response": None}}
        m["reactions"] = [r for r in m.get("reactions", []) if r[1] != ME_CHAT]
        if i["reaction_status"] == "created":
            m["reactions"].append((i["emoji"], ME_CHAT))
        return {"data": {"xig_direct_reaction_send_with_slide_messaging_response": {"success": True}}}
    if op == "gif":
        # As instagram.com's IGDMAPISendThirdPartyGIF builds it.
        d, sd = v.get("data") or {}, v.get("send_data") or {}
        if set(d) != {"is_random", "is_sticker", "reply_to_message_id", "sticker_id"} \
                or set(sd) != {"forwarded_from_thread_id", "is_forwarded_from_own_message", "offline_threading_id", "recipient_users", "reply_to_message_id", "send_attribution", "thread_id"}:
            return {"errors": [{"message": "A server error noncoercible_variable_value occured.", "severity": "CRITICAL", "code": 1675012}], "data": None}
        key = int(sd["thread_id"]) if str(sd.get("thread_id") or "").isdigit() and int(sd["thread_id"]) in MESSAGES else None
        if key is None or not str(d.get("sticker_id", "")).startswith("stk" if d["is_sticker"] else "gif") or not sd.get("offline_threading_id"):
            return {"errors": [{"message": "A server error missing_required_variable_value occured.", "severity": "CRITICAL", "code": 1675012}], "data": None}
        sent = msg(f"gf{time.time_ns()}", ME_CHAT, 0, attach=("gif", anim(d["sticker_id"], 200, 200 if d["is_sticker"] else 150), d["is_sticker"]))
        sent["ts"] = time.time_ns() // 1_000_000
        sent["reply"] = next((m for m in MESSAGES[key] if m["mid"] == d["reply_to_message_id"]), None)
        MESSAGES[key].append(sent)
        return {"data": {"direct_animated_media_send": {"message_id": sent["mid"], "id": "x"}}}
    if op == "story share":
        # As instagram.com's IGDMAPISendStoryShare builds it.
        d, sd = v.get("data") or {}, v.get("send_data") or {}
        if set(d) != {"is_shh_mode", "mutation_token", "nullable_text", "reel_id", "sampled", "share_client_context", "story_media_id", "story_share_type"} \
                or set(sd) != {"forwarded_from_thread_id", "is_forwarded_from_own_message", "offline_threading_id", "recipient_users", "reply_to_message_id", "thread_id"}:
            return {"errors": [{"message": "A server error noncoercible_variable_value occured.", "severity": "CRITICAL", "code": 1675012}], "data": None}
        pk = str(d.get("story_media_id", ""))
        key = int(sd["thread_id"]) if str(sd.get("thread_id") or "").isdigit() and int(sd["thread_id"]) in MESSAGES else \
            one_to_one(json.loads(sd["recipient_users"])[0]) if isinstance(sd.get("recipient_users"), str) else None
        if not pk.isdigit() or pk[1:5] not in USERS or key is None or not sd.get("offline_threading_id"):
            return {"errors": [{"message": "A server error missing_required_variable_value occured.", "severity": "CRITICAL", "code": 1675012}], "data": None}
        sent = msg(f"st{time.time_ns()}", ME_CHAT, 0, attach=("xma", f"https://www.instagram.com/stories/{USERS[pk[1:5]]['username']}/{pk}/", None))
        sent["ts"] = time.time_ns() // 1_000_000  # now, so messages sent one after another keep their order
        MESSAGES[key].append(sent)
        return {"data": {"direct_story_share_with_slide_message_response": {"message_id": sent["mid"], "timestamp_ms": str(sent["ts"]), "id": "x"}}}
    if op == "story reply":
        if REPLY_FAILS:
            return {"errors": [{"message": "A server error noncoercible_variable_value occured.", "severity": "CRITICAL", "code": 1675012}], "data": None}
        d, sd = v.get("data") or {}, v.get("send_data") or {}
        if set(d) != {"is_shh_mode", "media_id", "reaction_emoji", "reel_id", "sampled", "share_client_context", "text"} \
                or set(sd) != {"forwarded_from_thread_id", "is_forwarded_from_own_message", "offline_threading_id", "recipient_users", "thread_id"}:
            return {"errors": [{"message": "A server error noncoercible_variable_value occured.", "severity": "CRITICAL", "code": 1675012}], "data": None}
        text = (d.get("text") or {}).get("sensitive_string_value")
        if not str(d.get("media_id", "")).isdigit() or not text or not sd.get("offline_threading_id") \
                or not (sd.get("thread_id") or isinstance(sd.get("recipient_users"), str)):
            return {"errors": [{"message": "A server error missing_required_variable_value occured.", "severity": "CRITICAL", "code": 1675012}], "data": None}
        key = one_to_one(json.loads(sd["recipient_users"])[0]) if sd.get("recipient_users") else None
        sent = msg(f"r{time.time_ns()}", ME_CHAT, 0, text)
        if key is not None:
            MESSAGES[key].append(sent)
        return {"data": {"direct_story_share_reply_with_slide_message_response": {"message_id": sent["mid"], "timestamp_ms": str(sent["ts"]), "id": "x"}}}
    if op == "send":
        key = next((k for k in MESSAGES if f"34028236684171030{k}" == v.get("ig_thread_igid")), None)
        if key is None and v.get("recipient_igids"):
            key = one_to_one(v["recipient_igids"][0])
            chat = next((c for c, pk in CHAT_USERS.items() if pk == str(v["recipient_igids"][0])), None)
            if key is None and chat is not None:  # Instagram starts the conversation
                key = max(list(CONVOS) + list(OLDER_CONVOS)) + 1
                CONVOS[key] = ["", 1, [chat], NOW * 1000]
                MESSAGES[key] = []
        if key is None or not v.get("offline_threading_id") or not v.get("text"):
            return {"errors": [{"message": "A server error missing_required_variable_value occured.", "severity": "CRITICAL", "code": 1675012}], "data": None}
        sent = msg(f"s{time.time_ns()}", ME_CHAT, 0, v["text"])
        sent["ts"] = time.time_ns() // 1_000_000  # now, so messages sent one after another keep their order
        if v.get("reply_to_message_id") is not None:  # a reply: to a message id in this conversation
            sent["reply"] = next((m for m in MESSAGES[key] if m["mid"] == v["reply_to_message_id"]), None)
            if sent["reply"] is None:
                return {"errors": [{"message": "A server error field_exception occured.", "severity": "CRITICAL", "code": 1675030}], "data": None}
        MESSAGES[key].append(sent)
        return {"data": {"xig_direct_text_send_with_slide_messaging_response": {"message_id": sent["mid"], "timestamp_ms": str(sent["ts"]), "id": "x"}}}


# ----- instagram.com's current GraphQL for profiles, posts, stories, likes -----

LIKES = {}


def nulls(m):
    """GraphQL spells out absent fields as null."""
    return {"carousel_media": None, "video_versions": None, "caption": None, "location": None, **m}


def following_feed():
    """The people you follow, newest first: 30 posts over two weeks, with an
    ad and a suggestions unit mixed in."""
    authors = ["1001", "1002", "1003", "1004", "1005"]
    edges = []
    for i in range(30):
        pk = authors[i % len(authors)]
        m = nulls(media(str(700 + i), pk, f"following post {i}", 3600 * 2 + i * 11 * 3600, w=1080, h=1080))
        m["organic_tracking_token"] = f"tok{700 + i}"
        if i == 2:  # a photo with music, like instagram.com sends it (no audio address)
            m["music_metadata"] = {"music_info": {"music_asset_info": {"audio_cluster_id": "5550002", "title": "Morning Light",
                                   "display_artist": "Paper Boats", "cover_artwork_thumbnail_uri": img("cover2", 200, 200)}}}
        if i == 1:
            m["product_type"] = "clips"
            m["clips_metadata"] = {"music_info": {"music_asset_info": {"audio_cluster_id": "5550001", "title": "Night Drive",
                                   "display_artist": "Neon Coast", "cover_artwork_thumbnail_uri": img("cover1", 200, 200)}}}
        if VIDEOS and i % 4 == 1:
            m["media_type"] = 2
            m["video_versions"] = [{"type": 101, "url": f"{BASE}/vid/{VIDEOS[i % len(VIDEOS)]}", "width": 720, "height": 1280}]
        edges.append({"node": {"__typename": "XDTFeedItem", "media": m, "ad": None, "end_of_feed_demarcator": None}, "cursor": str(i)})
        if i == 3:
            edges.append({"node": {"__typename": "XDTFeedItem", "media": None, "ad": {"ad_id": "1"}}, "cursor": "ad"})
        if i == 5:
            edges.append({"node": {"__typename": "XDTFeedItem", "media": None, "ad": None, "suggested_users": {"suggestions": []}}, "cursor": "su"})
    return edges


def media_by_code(code):
    mid = re.sub(r"\D", "", code) or "777"
    m = nulls(media(mid, "1003", f"the post behind {code}", 7200, w=1080, h=1350))
    m["code"] = code
    if code.startswith("C777") and VIDEOS:
        m["media_type"] = 2
        m["product_type"] = "clips"
        m["video_versions"] = [{"type": 101, "url": f"{BASE}/vid/{VIDEOS[0]}", "width": 720, "height": 1280}]
        m["clips_metadata"] = {"music_info": {"music_asset_info": {"title": "Mountain Theme", "display_artist": "Alpine Band"}}}
    return m


def one_to_one(pk):
    """The 1:1 conversation with someone, by profile id."""
    return next((k for k, c in CONVOS.items() if c[1] == 1 and CHAT_USERS.get(c[2][0]) == str(pk)), None)


def web_answer(name, v):
    pv = {k: x for k, x in v.items() if k.startswith("__relay_internal__pv__")}
    if name == "PolarisPostRootQuery":
        if not v.get("shortcode"):
            return {"errors": [{"message": "A server error missing_required_variable_value occured.", "severity": "CRITICAL", "code": 1675012}], "data": None}
        return {"data": {"xdt_api__v1__media__shortcode__web_info": {"items": [media_by_code(v["shortcode"])]}}}
    if name == "PolarisSearchBoxRefetchableQuery":
        q = ((v.get("data") or {}).get("query") or "").lower()
        hits = [u for u in USERS.values() if q in u["username"] or q in u["full_name"].lower()]
        return {"data": {"xdt_api__v1__fbsearch__topsearch_connection": {"users": [{"user": {**u, "pk": str(u["pk"]), "id": str(u["pk"])}} for u in hits], "hashtags": []}}}
    if name == "IGDOmniPickerSearchResultsListQuery":
        q = (v.get("search_text") or "").lower()
        hits = [slide_user(c) for c in CHAT_USERS if q in USERS[CHAT_USERS[c]]["username"]]
        return {"data": {"xfb_combinedIGSearchQuery": {"search_messenger_contact_v2": [{**h, "pk": h["id"]} for h in hits]}}}
    bad = {"errors": [{"message": "A server error noncoercible_variable_value occured.", "severity": "CRITICAL", "code": 1675030}], "data": None}
    if name.startswith("usePolarisSaveMedia"):
        i = v.get("input") or {}
        if SAVE_FAILS or set(v) != {"input"} or set(i) != {"media_id"} or not str(i["media_id"]).isdigit():
            return bad
        save = name == "usePolarisSaveMediaSaveMutation"
        SAVED[str(i["media_id"])] = save
        return {"data": {"xig_media_save" if save else "xig_media_unsave": {"id": str(i["media_id"])}}}
    if name == "PolarisSavedCollectionPickerQuery":
        return {"data": {"viewer": {"media_collections": {"edges": [{"node": {"collection_id": k, "collection_name": f[0], "id": k}}
                for k, f in FOLDERS.items()], "page_info": {"end_cursor": None, "has_next_page": False}}}}}
    if name == "PolarisAPIReelSeenMutation":
        reel_id, item = str(v.get("reelId")), str(v.get("reelMediaId"))
        if reel_id not in USERS or not item.isdigit() or not item.startswith(f"9{reel_id}") or v.get("reelMediaOwnerId") != reel_id \
                or not isinstance(v.get("reelMediaTakenAt"), int) or not isinstance(v.get("viewSeenAt"), int):
            return bad
        SEEN[reel_id] = max(SEEN.get(reel_id, 0), v["reelMediaTakenAt"])
        return {"data": {"xdt_mark_story_reel_seen": {"__typename": "XDTMarkSeenResponse"}}}
    if name.startswith("usePolarisStoryLikes"):
        i = v.get("input") or {}
        if set(v) != {"input"} or not str(i.get("media_id", "")).isdigit():
            return bad
        like = not name.startswith("usePolarisStoryLikesUnlike")
        STORY_LIKES[str(i["media_id"])] = like
        field = "xig_send_story_like" if like else "xig_unsend_story_like"
        return {"data": {field: {"__typename": "XIGSendStoryLikeResponsePayload"}}}
    if name == "PolarisFeedTimelineRootV2Query":
        d = v.get("data") or {}
        if v.get("variant") != "following" or d.get("pagination_source") != "following" or not d.get("device_id"):
            return {"errors": [{"message": "A server error noncoercible_variable_value occured.", "severity": "CRITICAL", "code": 1675030}], "data": None}
        edges = following_feed()
        start = int(v["after"]) if v.get("after") else 0
        page = edges[start:start + v["first"]]
        more = start + v["first"] < len(edges)
        return {"data": {"xdt_api__v1__feed__timeline__connection": {"pagination_source": "following", "edges": page,
                "page_info": {"end_cursor": str(start + v["first"]) if more else None, "has_next_page": more,
                              "has_previous_page": start > 0, "start_cursor": str(start)}}}}
    if name == "PolarisProfilePageContentQuery":
        u = dict(USERS.get(str(v["id"]), USERS["1001"]))
        u.update({"pk": str(u["pk"]), "id": str(u["pk"]), "biography": "Photos, mountains & coffee ☕\nBologna 🇮🇹 (GraphQL)",
                  "external_url": "https://example.com", "follower_count": 1234, "following_count": 321, "media_count": 15,
                  "hd_profile_pic_url_info": {"url": u["profile_pic_url"]}, "bio_links": [],
                  "friendship_status": {"following": u["friendship_status"]["following"], "followed_by": True, "outgoing_request": False, "blocking": False}})
        return {"data": {"user": u}, "extensions": {"is_final": True}}
    if name == "PolarisProfilePostsQuery":
        if v.get("data", {}).get("count") != 12 or pv != {"__relay_internal__pv__PolarisShortDramaEnabledrelayprovider": False}:
            return {"errors": [{"message": "A server error missing_required_variable_value occured.", "severity": "CRITICAL", "code": 1675012}], "data": None}
        pk = next((k for k, u in USERS.items() if u["username"] == v["username"]), "1001")
        start = int(v["after"]) if v.get("after") else 0
        count = 12 if start == 0 else 3
        items = [nulls(media(str(600 + start + i), pk, f"post {start + i} (GraphQL)", 86400 * (i + 1), w=1080, h=1080,
                             carousel=3 if i % 5 == 1 else None)) for i in range(count)]
        for m in items:
            m["has_liked"] = LIKES.get(str(m["pk"]), False)
        more = start == 0
        return {"data": {"xdt_api__v1__feed__user_timeline_graphql_connection": {
            "edges": [{"node": m, "cursor": str(m["pk"])} for m in items],
            "page_info": {"end_cursor": "12" if more else None, "has_next_page": more, "has_previous_page": False, "start_cursor": None}},
            "xdt_viewer": {"user": {"id": ME}}}}
    if name == "PolarisStoriesV3ReelPageStandaloneQuery":
        reels = [{**reel(i), "id": str(i), "items": [nulls(x) for x in story_items(i, 5)], "cover_media": None, "title": None} for i in v["reel_ids_arr"] if i in USERS]
        return {"data": {"xdt_api__v1__feed__reels_media": {"reels_media": reels, "unviewable_authors_infos": []}, "xdt_viewer": {"user": {"id": ME}}}}
    if name.startswith("usePolarisLikeMedia"):
        like = "XIGLike" in name
        i = v.get("input") or {}
        want = {"container_module", "media_id", "tracking_token"} if like else {"media_id", "tracking_token"}
        if set(v) != {"input"} or set(i) != want or not i.get("media_id"):
            return {"errors": [{"message": "A server error missing_required_variable_value occured.", "severity": "CRITICAL", "code": 1675012}], "data": None}
        LIKES[i["media_id"]] = like
        field = "xig_media_like" if like else "xig_media_unlike"
        return {"data": {field: {"media": {"id": i["media_id"], "has_liked": like}}}}


def mqtt_string(b, i):
    n = struct.unpack(">H", b[i:i + 2])[0]
    return b[i + 2:i + 2 + n].decode(), i + 2 + n


def mqtt_packet(header, body):
    n, length = len(body), b""
    while True:
        byte = n % 128
        n //= 128
        length += bytes([byte | (0x80 if n else 0)])
        if not n:
            break
    return bytes([header]) + length + body


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

    def chat_log(self, line):
        with open(sys.argv[2] if len(sys.argv) > 2 else "/dev/null", "a") as f:
            f.write(f"CHAT {line}\n")

    def ws_send(self, data):
        n = len(data)
        head = bytes([0x82, n]) if n < 126 else bytes([0x82, 126]) + struct.pack(">H", n) if n < 65536 else bytes([0x82, 127]) + struct.pack(">Q", n)
        with self.ws_lock:
            self.wfile.write(head + data)
            self.wfile.flush()

    def ws_read(self):
        head = self.rfile.read(2)
        if len(head) < 2:
            return None
        opcode, n = head[0] & 0x0F, head[1] & 0x7F
        if n == 126:
            n = struct.unpack(">H", self.rfile.read(2))[0]
        elif n == 127:
            n = struct.unpack(">Q", self.rfile.read(8))[0]
        mask = self.rfile.read(4) if head[1] & 0x80 else b"\0\0\0\0"
        data = bytes(b ^ mask[i % 4] for i, b in enumerate(self.rfile.read(n)))
        return opcode, data

    def publish(self, topic, payload):
        t = topic.encode()
        self.ws_send(mqtt_packet(0x30, struct.pack(">H", len(t)) + t + payload.encode()))

    def chat(self):
        key = self.headers.get("Sec-WebSocket-Key", "")
        accept = base64.b64encode(hashlib.sha1((key + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11").encode()).digest()).decode()
        self.protocol_version = "HTTP/1.1"  # WebSocket upgrades need 1.1
        self.send_response(101)
        self.send_header("Upgrade", "websocket")
        self.send_header("Connection", "Upgrade")
        self.send_header("Sec-WebSocket-Accept", accept)
        self.end_headers()
        self.ws_lock = threading.Lock()
        self.chat_log(f"open {self.path} origin={self.headers.get('Origin')} cookie={'sessionid=' in (self.headers.get('Cookie') or '')} ua={self.headers.get('User-Agent')}")
        try:
            self.chat_loop()
        except ConnectionAbortedError:
            pass
        self.chat_log("closed")

    def chat_loop(self):
        buf = b""
        while (frame := self.ws_read()) is not None:
            opcode, data = frame
            if opcode == 8:
                break
            buf += data
            while len(buf) >= 2:
                n, mult, i = 0, 1, 1
                while True:
                    n += (buf[i] & 0x7F) * mult
                    mult *= 128
                    i += 1
                    if not buf[i - 1] & 0x80:
                        break
                if len(buf) < i + n:
                    break
                kind, flags, body, buf = buf[0] >> 4, buf[0] & 0x0F, buf[i:i + n], buf[i + n:]
                self.mqtt(kind, flags, body)

    def mqtt(self, kind, flags, body):
        if kind == 1:  # CONNECT
            proto, i = mqtt_string(body, 0)
            level, conn_flags, keepalive = body[i], body[i + 1], struct.unpack(">H", body[i + 2:i + 4])[0]
            client, i = mqtt_string(body, i + 4)
            username, i = mqtt_string(body, i)
            u = json.loads(username)
            ok = u.get("u") == str(ME_CHAT)
            self.chat_log(f"CONNECT {proto} level={level} flags={conn_flags:#x} keepalive={keepalive} client={client} user={u.get('u')} aid={u.get('aid')} d={u.get('d')} keys={sorted(u)} -> {'ok' if ok else 'refused'}")
            self.ws_send(bytes([0x20, 0x02, 0x00, 0x00 if ok else 0x05]))
            if not ok:
                raise ConnectionAbortedError
        elif kind == 8:  # SUBSCRIBE
            pid, topics, i = body[:2], [], 2
            while i < len(body):
                t, i = mqtt_string(body, i)
                topics.append(t)
                i += 1
            self.chat_log(f"SUBSCRIBE {topics}")
            self.ws_send(mqtt_packet(0x90, pid + bytes(len(topics))))
        elif kind == 3:  # PUBLISH
            topic, i = mqtt_string(body, 0)
            if (flags >> 1) & 3:
                self.ws_send(bytes([0x40, 0x02]) + body[i:i + 2])
                i += 2
            payload = body[i:].decode()
            if topic == "/ls_req":
                self.ls_request(json.loads(payload))
            else:
                self.chat_log(f"PUBLISH {topic} {payload}")
        elif kind == 12:  # PINGREQ
            self.ws_send(bytes([0xD0, 0x00]))

    def ls_request(self, req):
        inner = json.loads(req["payload"])
        if req.get("type") == 1:
            self.chat_log(f"sync database={inner.get('database')} version={inner.get('version')} cursor={inner.get('last_applied_cursor')} sync_params={inner.get('sync_params')}")
            payload = inbox_snapshot() if inner.get("database") == 1 else ls_program([])
            return self.publish("/ls_resp", json.dumps({"request_id": req["request_id"], "payload": payload, "sp": []}))
        calls = []
        for t in inner["tasks"]:
            p = json.loads(t["payload"])
            self.chat_log(f"task {t['label']} queue={t['queue_name']} version={inner.get('version_id')} type={req.get('type')} app={req.get('app_id')} payload={p}")
            if t["label"] == "228":
                key, ref = p["thread_key"], p["reference_timestamp_ms"]
                older = [m for m in MESSAGES.get(key, []) if m["ts"] < ref]
                calls += message_calls(key, older[-20:])
            elif t["label"] == "145":
                ref = p["reference_activity_timestamp"]
                if ref > NOW * 1000:
                    calls += convo_calls(CONVOS)
                else:
                    calls += convo_calls(OLDER_CONVOS) + [("upsertSyncGroupThreadsRange", {0: 1, 1: 1, 2: False})]
            elif t["label"] == "46":
                key = p["thread_id"]
                sent = msg(f"s{time.time_ns()}", ME_CHAT, 0, p["text"])
                MESSAGES[key].append(sent)
                calls += message_calls(key, [sent])
                if key == 30001:
                    threading.Timer(2.0, self.push_reply, args=(key,)).start()
        self.publish("/ls_resp", json.dumps({"request_id": req["request_id"], "payload": ls_program(calls), "sp": sorted({c[0] for c in calls})}))

    def push_reply(self, key):
        reply = msg(f"r{time.time_ns()}", 20001, 0, "Sounds good!")
        MESSAGES[key].append(reply)
        self.publish("/ls_resp", json.dumps({"request_id": None, "payload": ls_program(message_calls(key, [reply])), "sp": ["insertMessage"]}))

    def serve_anim(self, name):
        m = re.match(r"(.+)_(\d+)x(\d+)\.webp", name)
        label, w, h = m.group(1), int(m.group(2)), int(m.group(3))
        color = COLORS[hash(label) % len(COLORS)]
        frames = []
        for i in range(8):
            im = Image.new("RGBA", (w, h), (0, 0, 0, 0))
            d = ImageDraw.Draw(im)
            x, y = w // 2 + int(w / 4 * math.cos(i * math.pi / 4)), h // 2 + int(h / 4 * math.sin(i * math.pi / 4))
            d.ellipse([x - w // 6, y - w // 6, x + w // 6, y + w // 6], fill=color + (255,))
            d.text((4, 4), label, fill=(255, 255, 255, 255))
            frames.append(im)
        buf = io.BytesIO()
        frames[0].save(buf, "WEBP", save_all=True, append_images=frames[1:], duration=120, loop=0, lossless=True)
        body = buf.getvalue()
        self.send_response(200)
        self.send_header("Content-Type", "image/webp")
        self.send_header("Content-Length", str(len(body)))
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

    def serve_file(self, file, ctype):
        """Like a CDN: can send part of the file (unless --no-ranges)."""
        body = open(file, "rb").read()
        rng = re.match(r"bytes=(\d+)-(\d*)", self.headers.get("Range") or "")
        if rng and "--no-ranges" not in sys.argv:
            a = int(rng.group(1))
            b = int(rng.group(2)) if rng.group(2) else len(body) - 1
            self.send_response(206)
            self.send_header("Content-Range", f"bytes {a}-{b}/{len(body)}")
            body = body[a:b + 1]
        else:
            self.send_response(200)
        self.send_header("Accept-Ranges", "none" if "--no-ranges" in sys.argv else "bytes")
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        try:
            self.wfile.write(body)
        except (BrokenPipeError, ConnectionResetError):
            pass

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
        if path.startswith("/anim/"):
            return self.serve_anim(path[6:])
        m = re.fullmatch(r"/giphy/(gifs|stickers)/(trending|search)", path)
        if m:
            if q.get("api_key") != [GIPHY_KEY]:
                return self.send_json({"meta": {"status": 401, "msg": "Unauthorized"}}, 401)
            words, kind = q.get("q", ["trending"])[0], "stk" if m.group(1) == "stickers" else "gif"
            def images(i):
                w, h = 100, 75 + 10 * (i % 3)
                return {"fixed_width_still": {"url": img(f"{kind}-{words}-{i}", w * 2, h * 2), "width": str(w * 2), "height": str(h * 2)},
                        "fixed_width_small": {"url": "x", "webp": anim(f"{kind}-{words}-{i}", w, h), "width": str(w), "height": str(h)}}
            return self.send_json({"data": [{"id": f"{kind}{i}", "url": f"https://giphy.com/{m.group(1)}/{kind}{i}", "images": images(i)}
                                            for i in range(0 if words == "nothing" else 9)]})
        if path == "/aud/song.ogg" and AUDIO:
            return self.serve_file(AUDIO, "audio/ogg")
        if path == "/mock/incoming":
            key = int(q.get("key", ["30001"])[0])
            sender = CONVOS[key][2][0]
            MESSAGES[key].append(msg(f"in{time.time_ns()}", sender, NOW - int(time.time()), q.get("text", ["Hello?"])[0]))
            return self.send_json({"status": "ok"})
        if path.startswith("/vid/") and path[5:] in VIDEOS:
            return self.serve_file(os.path.join(os.environ["FINSTA_MOCK_VIDEOS"], path[5:]), "video/mp4")
        if path.startswith("/rsrc.php/"):
            js = script(path.rsplit("/", 1)[1]).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/javascript")
            self.send_header("Content-Length", str(len(js)))
            self.end_headers()
            self.wfile.write(js)
            return
        if path == "/api/graphql" and body.get("doc_id", [""])[0] in WEB_DOCS:
            f = {k: v[0] for k, v in body.items()}
            v = json.loads(f.get("variables", "{}"))
            name = WEB_DOCS[f["doc_id"]]
            with open(sys.argv[2] if len(sys.argv) > 2 else "/dev/null", "a") as log:
                log.write(f"GRAPHQL {name} variables={v} referer={self.headers.get('Referer')}\n")
            return self.send_json(web_answer(name, v))
        if path == "/api/graphql" and body.get("doc_id", [""])[0] in SLIDE_DOCS:
            f = {k: v[0] for k, v in body.items()}
            op = SLIDE_DOCS[f["doc_id"]]
            v = json.loads(f.get("variables", "{}"))
            with open(sys.argv[2] if len(sys.argv) > 2 else "/dev/null", "a") as log:
                log.write(f"GRAPHQL {op} {f.get('fb_api_req_friendly_name')} variables={v} av={f.get('av')}\n")
            return self.send_json(slide_answer(op, v))
        if path == "/api/graphql":
            f = {k: v[0] for k, v in body.items()}
            variables = json.loads(f.get("variables", "{}"))
            req = json.loads(variables.get("requestPayload", "{}"))
            checks = {"doc": f.get("doc_id") == DOC_ID, "dtsg": f.get("fb_dtsg") == "mockdtsg", "lsd": f.get("lsd") == "mocklsd",
                      "lsd_header": self.headers.get("X-FB-LSD") == "mocklsd", "name": self.headers.get("X-FB-Friendly-Name"),
                      "av": f.get("av"), "version": req.get("version"), "database": req.get("database"), "device": variables.get("deviceId"),
                      "fields": sorted(f)}
            with open(sys.argv[2] if len(sys.argv) > 2 else "/dev/null", "a") as log:
                log.write(f"GRAPHQL {checks}\n")
            if "--no-graphql" in sys.argv or not (checks["doc"] and checks["dtsg"]):
                return self.send_json({"errors": [{"message": "A server error noncoercible_variable_value occured. Check server logs for details.",
                                                   "severity": "CRITICAL", "code": 1675030}], "data": None})
            return self.send_json({"data": {"viewer": {"lightspeed_web_request_for_igd": {"payload": inbox_snapshot(), "dependencies": [], "experiment_value": None}}},
                                   "extensions": {"is_final": True}})
        if path == "/chat":
            return self.chat()
        if path == "/direct/inbox/":
            if "sessionid=" not in (self.headers.get("Cookie") or ""):
                self.send_response(302)
                self.send_header("Location", "/accounts/login/?next=/direct/inbox/")
                self.end_headers()
                return
            page = inbox_page().encode()
            self.send_response(200)
            self.send_header("Content-Type", "text/html; charset=utf-8")
            self.send_header("Content-Length", str(len(page)))
            self.end_headers()
            self.wfile.write(page)
            return
        p = path.replace("/api/v1/", "")
        if OFFMSYS and (p == "users/web_profile_info/" or re.match(r"users/\d+/info/", p)):
            self.send_response(429)
            self.send_header("Content-Length", "0")
            self.end_headers()
            return
        if OFFMSYS and (re.match(r"feed/user/", p) or re.match(r"web/likes/", p)):
            return self.send_json({"status": "fail", "message": "mock: gone"}, 404)
        if OFFMSYS and p == "feed/reels_media/":
            return self.send_json({"message": "We're sorry, but something went wrong. Please try again.", "status": "fail"}, 500)
        if WWW and re.match(r"users/\d+/info/", p):
            self.send_response(429)
            self.send_header("Content-Length", "0")
            self.end_headers()
            return
        if p == "accounts/edit/web_form_data/":
            me = USERS[ME]
            return self.send_json({"form_data": {"username": me["username"], "first_name": me["full_name"], "biography": ""}, "status": "ok"})
        if p == "users/web_profile_info/":
            name = q.get("username", [""])[0]
            u = next((u for u in USERS.values() if u["username"] == name), None)
            if not u:
                return self.send_json({"data": {"user": None}, "status": "ok"}, 404)
            return self.send_json({"data": {"user": {
                "id": str(u["pk"]), "username": u["username"], "full_name": u["full_name"],
                "profile_pic_url": u["profile_pic_url"], "profile_pic_url_hd": u["profile_pic_url"],
                "is_private": False, "is_verified": u["is_verified"], "biography": "Photos, mountains & coffee ☕\nBologna 🇮🇹",
                "external_url": "https://example.com", "edge_followed_by": {"count": 1234}, "edge_follow": {"count": 321},
                "edge_owner_to_timeline_media": {"count": 15}, "followed_by_viewer": u["friendship_status"]["following"],
                "follows_viewer": True, "requested_by_viewer": False}}, "status": "ok"})
        if m := re.match(r"users/(\d+)/info/", p):
            u = dict(USERS.get(m.group(1), USERS["1001"]))
            u.update({"biography": "Photos, mountains & coffee ☕\nBologna 🇮🇹", "external_url": "https://example.com", "follower_count": 1234, "following_count": 321, "media_count": 15})
            return self.send_json({"user": u, "status": "ok"})
        if re.match(r"friendships/\d+/following/", p):
            return self.send_json({"users": [USERS[k] for k in ["1001", "1002", "1003", "1004", "1005"]], "status": "ok"})
        if p == "feed/timeline/" and OFFMSYS:
            items = [{"media_or_ad": media("599", "1003", "the only unseen post", 3 * 86400)},
                     {"end_of_feed_demarcator": {"title": "You're all caught up"}},
                     {"media_or_ad": media("902", "2002", "suggested post after demarcator", 3600)}]
            return self.send_json({"feed_items": items, "more_available": True, "next_max_id": "page2", "status": "ok"})
        if p == "feed/timeline/":
            if body.get("max_id"):
                return self.send_json({"feed_items": [], "more_available": False, "status": "ok"})
            return self.send_json(timeline())
        if p == "feed/reels_tray/":
            return self.send_json({"tray": [reel("1000"), reel("1001"), reel("2001"), reel("1003"), reel("1005"), reel("1002")], "status": "ok"})
        if p == "feed/reels_media/":
            ids = q.get("reel_ids", [])
            return self.send_json({"reels": {i: {**reel(i), "items": story_items(i, 3)} for i in ids}, "status": "ok"})
        if m := re.match(r"friendships/show/(\d+)/", p):
            return self.send_json({"following": m.group(1) in USERS and m.group(1) < "2000", "followed_by": True, "status": "ok"})
        if m := re.match(r"feed/user/([^/]+)/username/", p):
            pk = next((k for k, u in USERS.items() if u["username"] == m.group(1)), "1001")
            m = re.match(r"(.*)", pk)
        if m := (m if p.endswith("/username/") else re.match(r"feed/user/(\d+)/", p)):
            pk = m.group(1) if m.group(1) in USERS else "1001"
            start = 0 if not q.get("max_id") else 12
            items = [media(str(600 + start + i), pk, f"post {start + i}", 86400 * (i + 1), w=1080, h=1080, carousel=3 if i % 5 == 1 else None) for i in range(12 if start == 0 else 3)]
            return self.send_json({"items": items, "more_available": start == 0, "next_max_id": "m2" if start == 0 else None, "status": "ok"})
        if p == "feed/saved/posts/":
            start = 0 if not q.get("max_id") else 9
            items = [{"media": media(str(800 + start + i), ["1001", "1003", "1005"][i % 3], f"saved post {start + i}", 86400 * (i + 2), w=1080, h=1080)}
                     for i in range(9 if start == 0 else 4)]
            return self.send_json({"items": items, "more_available": start == 0, "next_max_id": "s2" if start == 0 else None, "status": "ok"})
        if p == "collections/list/":
            if "--folders-v1-fails" in sys.argv:
                return self.send_json({"status": "fail", "message": "mock: no"}, 400)
            items = [{"collection_id": "ALL_MEDIA_AUTO_COLLECTION", "collection_name": "All posts", "collection_type": "ALL_MEDIA_AUTO_COLLECTION"}]
            for k, f in FOLDERS.items():
                items.append({"collection_id": int(k), "collection_name": f[0], "collection_type": "MEDIA", "collection_media_count": 4 + len(f[1]),
                              "cover_media_list": [{"image_versions2": {"candidates": [{"url": img(f"folder{k}", 640, 640), "width": 640, "height": 640}]}}]})
            return self.send_json({"items": items, "more_available": False, "status": "ok"})
        if m := re.match(r"feed/collection/(\d+)/posts/", p):
            base = 900 + (int(m.group(1)) % 10) * 10
            items = [{"media": media(str(base + i), ["1002", "1004"][i % 2], f"folder post {i}", 86400 * (i + 1), w=1080, h=1080)} for i in range(4)]
            items += [{"media": media(pk, "1001", "added to the folder", 3600, w=1080, h=1080)} for pk in FOLDERS[m.group(1)][1]]
            return self.send_json({"items": items, "more_available": False, "status": "ok"})
        if m := re.match(r"media/(\d+)/(save|unsave)/", p):
            SAVED[m.group(1)] = m.group(2) == "save"
            for fid in json.loads(body.get("added_collection_ids", ["[]"])[0]):
                FOLDERS[str(fid)][1].add(m.group(1))
            return self.send_json({"status": "ok"})
        if p == "tags/web_info/":  # gone, as on instagram.com in November 2026
            return self.send_json({"message": "We're sorry, but something went wrong. Please try again.", "status": "fail"}, 404)
        if (m := re.match(r"tags/(\w+)/sections/", p)) and method == "POST":
            tag = m.group(1)
            if body.get("tab", [""])[0] != "top":
                return self.send_json({"status": "fail", "message": "mock: tab"}, 400)
            medias = [{"media": media(str(850 + i), ["1002", "1004"][i % 2], f"#{tag} number {i}", 86400 * (i + 1), w=1080, h=1080)} for i in range(7)]
            return self.send_json({"sections": [{"layout_type": "media_grid", "layout_content": {"medias": medias[:3]}},
                                                {"layout_type": "media_grid", "layout_content": {"medias": medias[3:]}}],
                                   "more_available": True, "status": "ok"})
        if p == "fbsearch/web/top_serp/":
            word = q.get("query", [""])[0]
            medias = [{"media": media(str(850 + i), ["1002", "1004"][i % 2], f"a post about {word} #{i}", 86400 * (i + 1), w=1080, h=1080)} for i in range(7)]
            return self.send_json({"media_grid": {"sections": [{"layout_type": "media_grid", "layout_content": {"medias": medias[:3]}},
                                                               {"layout_type": "media_grid", "layout_content": {"medias": medias[3:]}}]}, "status": "ok"})
        if p == "direct_v2/threads/broadcast/reel_share/":
            ok = body.get("media_id") and body.get("text") and body.get("reel_id")
            if REPLY_FAILS and "--v1-reply-fails" in sys.argv:
                ok = False
            return self.send_json({"status": "ok" if ok else "fail", "message": None if ok else "mock: bad reply"}, 200 if ok else 400)
        if m := re.match(r"media/(\d+)/info/", p):
            pk = m.group(1)
            found = next((e["node"]["media"] for e in following_feed() if (e["node"]["media"] or {}).get("pk") == int(pk)), None)
            if found is None and pk.startswith("9") and pk[1:5] in USERS:
                found = next(({**x, "user": USERS[pk[1:5]]} for x in story_items(pk[1:5], 5) if str(x["pk"]) == pk), None)
            return self.send_json({"items": [with_song_audio(found or media_by_code(f"C{pk}xyz"))], "status": "ok"})
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
        if m := re.match(r"media/.+/comments/(\d+)/child_comments/", p):
            # Comment 1 has 3 replies, comment 3 has 25; 10 a page, oldest first.
            total, start = {1: 3, 3: 25}.get(int(m.group(1)), 0), int(q.get("max_id", ["0"])[0])
            more = start + 10 < total
            return self.send_json({"child_comments": [reply(int(m.group(1)), i) for i in range(start, min(start + 10, total))],
                                   "has_more_tail_child_comments": more, "next_max_child_cursor": str(start + 10) if more else None, "status": "ok"})
        if re.match(r"media/.+/comments/", p):
            return self.send_json({"comments": [
                {"pk": 1, "text": "Wow, stunning! 😍", "user": USERS["1002"], "created_at": NOW - 5000, "comment_like_count": 3,
                 "child_comment_count": 3, "preview_child_comments": [reply(1, 0)]},
                {"pk": 2, "text": "Where is this?", "user": USERS["1004"], "created_at": NOW - 4000, "preview_child_comments": None},
                {"pk": 3, "text": "Which camera do you use?", "user": USERS["1005"], "created_at": NOW - 3900, "child_comment_count": 25,
                 "preview_child_comments": []},
                {"pk": 4, "text": "", "user": USERS["1001"], "created_at": NOW - 3800, "giphy_media_info": {"first_party_cdn_proxied_images":
                 {"fixed_height": {"url": anim("gif", 200, 150), "width": "200", "height": "150"}}}},
                {"pk": 5, "text": "Here's mine", "user": USERS["1003"], "created_at": NOW - 3700, "comment_media": {"image_versions2":
                 {"candidates": [{"url": img("cmt", 600, 800), "width": 600, "height": 800}]}}},
            ], "status": "ok"})
        if re.match(r"media/.+/(like|unlike)/", p):
            return self.send_json({"status": "ok"})
        return self.send_json({"status": "fail", "message": f"mock: unknown {p}"}, 404)

    def do_GET(self):
        self.route("GET")

    def do_POST(self):
        self.route("POST")


ThreadingHTTPServer(("127.0.0.1", PORT), Handler).serve_forever()
