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
"""
import base64
import hashlib
import io
import json
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
    return {"mid": f"mid.$mock{mid}", "sender": sender, "ts": (NOW - ago) * 1000, "text": text, "attach": attach}


MESSAGES = {
    30001: [msg(f"f{i}", 20001 if i % 2 else ME_CHAT, 200000 + 600 * (40 - i), f"Older message {i}") for i in range(40)] + [
        msg("a1", 20001, 90500, "Did you see the photos from Saturday?"),
        msg("a2", ME_CHAT, 90000, "Not yet, send them!"),
        msg("a3", 20001, 3100, attach=("blob", img("dmphoto", 1080, 1350))),
        msg("a4", ME_CHAT, 3000, "Wow 😍"),
        msg("a5", 20001, 320, attach=("xma", "https://www.instagram.com/p/MOCK123/", "Golden hour in Bologna")),
        msg("a5b", 20001, 310, attach=("raven",)),
        msg("a5c", 20001, 305, attach=("audio",)),
        msg("a6", 20001, 300, "Are you coming tonight? 🍕"),
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
WEB_DOCS = {"25000000000000001": "PolarisProfilePageContentQuery", "25000000000000002": "PolarisProfilePostsQuery",
            "25000000000000003": "PolarisStoriesV3ReelPageStandaloneQuery", "25000000000000004": "usePolarisLikeMediaXIGLikeMutation",
            "25000000000000005": "usePolarisLikeMediaXIGUnlikeMutation"}
# Saved-style definitions: what the app reads to fill in and fit inputs.
WEB_MODULES = (
    '__d("PolarisProfilePostsQuery.graphql",["PolarisProfilePostsQuery_instagramRelayOperation","PolarisShortDramaEnabled.relayprovider"],'
    '(function(t,n,r,o,a,i){"use strict";var e=(function(){var e={defaultValue:null,kind:"LocalArgument",name:"data"},'
    't={defaultValue:null,kind:"LocalArgument",name:"username"},r={defaultValue:null,kind:"LocalArgument",name:"first"},'
    'o={defaultValue:null,kind:"LocalArgument",name:"after"},l={defaultValue:null,kind:"LocalArgument",name:"before"},'
    's={defaultValue:null,kind:"LocalArgument",name:"last"},u={defaultValue:null,kind:"LocalArgument",name:"__relay_internal__pv__PolarisShortDramaEnabledrelayprovider"};'
    'return{fragment:{argumentDefinitions:[e,t,r,o,l,s,u]},params:{providedVariables:{__relay_internal__pv__PolarisShortDramaEnabledrelayprovider:n("PolarisShortDramaEnabled.relayprovider")}}}})();a.exports=e}),null);'
    + "".join(
        f'__d("usePolarisLikeMediaXIG{kind}Mutation.graphql",["usePolarisLikeMediaXIG{kind}Mutation_instagramRelayOperation"],'
        '(function(t,n,r,o,a,i){"use strict";var e=(function(){var e={defaultValue:null,kind:"LocalArgument",name:"media_id"};'
        'return{fragment:{argumentDefinitions:[e]}}})();a.exports=e}),null);' for kind in ("Like", "Unlike"))
)
SLIDE_DOCS = {INBOX_DOC: "inbox", LIST_DOC: "more threads", MESSAGES_DOC: "messages", SEND_DOC: "send", MARK_DOC: "mark read"}
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
    if a[0] == "raven":
        return {"__typename": "SlideMessageRavenImageContent", "attachment": {"__typename": "XFBSlideAttachment", "preview_cdn_url": img("secret", 100, 100)}}
    if a[0] == "audio":
        return {"__typename": "SlideMessageAudiosContent", "audio_attachments": [{"attachment_cdn_url": "x", "playable_duration_ms": 4000}]}
    return {"__typename": "SlideMessageXMAContent", "xma_text_body": None,
            "xma": {"__typename": "SlideMessagePortraitXMA", "target_url": a[1], "header_title_text": "natgeo_fan", "title_text": a[2],
                    "preview_image": {"url": img("xmapreview", 640, 640)}}}


def slide_message(m):
    return {"id": "node" + m["mid"], "message_id": m["mid"], "sender_fbid": str(m["sender"]), "timestamp_ms": str(m["ts"]), "__typename": "SlideMessage",
            "is_reported": False, "tombstone_reason": None, "content": slide_content(m), "igd_snippet": m["text"] or "sent an attachment"}


def slide_thread(key, convos):
    name, kind, members, read = convos[key]
    msgs = MESSAGES[key]
    return {"node": {"id": f"slide{key}", "__typename": "SlideThread", "as_ig_direct_thread": {
        "id": f"t{key}", "thread_id": f"34028236684171030{key}", "thread_key": str(key), "thread_fbid": str(key),
        "is_group": kind == 2, "thread_title": name, "thread_image_url": None,
        "last_activity_timestamp_ms": str(msgs[-1]["ts"]), "users": [slide_user(c) for c in members],
        "viewer": {"id": ME, "interop_messaging_user_fbid": str(ME_CHAT), "profile_pic_url": USERS[ME]["profile_pic_url"]},
        "marked_as_unread": False, "is_muted": False,
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
    if op == "send":
        key = next((k for k in MESSAGES if f"34028236684171030{k}" == v.get("ig_thread_igid")), None)
        if key is None or not v.get("offline_threading_id") or not v.get("text"):
            return {"errors": [{"message": "A server error missing_required_variable_value occured.", "severity": "CRITICAL", "code": 1675012}], "data": None}
        sent = msg(f"s{time.time_ns()}", ME_CHAT, 0, v["text"])
        MESSAGES[key].append(sent)
        return {"data": {"xig_direct_text_send_with_slide_messaging_response": {"message_id": sent["mid"], "timestamp_ms": str(sent["ts"]), "id": "x"}}}


# ----- instagram.com's current GraphQL for profiles, posts, stories, likes -----

LIKES = {}


def nulls(m):
    """GraphQL spells out absent fields as null."""
    return {"carousel_media": None, "video_versions": None, "caption": None, "location": None, **m}


def web_answer(name, v):
    pv = {k: x for k, x in v.items() if k.startswith("__relay_internal__pv__")}
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
        reels = [{**reel(i), "id": str(i), "items": [nulls(x) for x in story_items(i, 3)], "cover_media": None, "title": None} for i in v["reel_ids_arr"] if i in USERS]
        return {"data": {"xdt_api__v1__feed__reels_media": {"reels_media": reels, "unviewable_authors_infos": []}, "xdt_viewer": {"user": {"id": ME}}}}
    if name.startswith("usePolarisLikeMedia"):
        if set(v) != {"media_id"}:
            return {"errors": [{"message": "Variables not declared", "severity": "CRITICAL", "code": 1675030}], "data": None}
        LIKES[v["media_id"]] = "XIGLike" in name
        return {"data": {"xdt_mark_media_like" if "XIGLike" in name else "xdt_mark_media_unlike": {"__typename": "XDTEmptyResponse", "success": True}}}


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
        if p == "feed/timeline/":
            if body.get("max_id"):
                return self.send_json({"feed_items": [], "more_available": False, "status": "ok"})
            return self.send_json(timeline())
        if p == "feed/reels_tray/":
            return self.send_json({"tray": [reel("1000"), reel("1001"), reel("2001"), reel("1003"), reel("1005", seen=True), reel("1002")], "status": "ok"})
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
