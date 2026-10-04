#!/usr/bin/env python3
"""A tiny stand-in for Telegram's servers, to try RyzikOS's Telegram app
without a real account (and in places Telegram can't be reached).

It speaks real MTProto 2.0 over TCP: the key exchange (with its own RSA
key instead of Telegram's), encrypted messages, containers, gzip, salts,
and just enough of the API: sign-in with a QR code (it counts as
scanned a few seconds after it is shown) or with a code and a two-step
verification password, file uploads, users, a chat list, history, sending, and an
answer pushed back a moment after you write. Messages have photos, files,
links, @names and emoji; photos and files download (one photo from
another data centre); the search finds a public channel you are not in,
which you can open by @spacenews and join.
Messages can be answered, edited, deleted, forwarded and pinned; Alice
shows "typing..." before she answers and is online; the bot answers with
inline buttons and a keyboard; the group has pinned messages and
answers that quote older messages.

    python3 -m venv /tmp/tgvenv && /tmp/tgvenv/bin/pip install telethon pillow
    /tmp/tgvenv/bin/python scripts/tg-test-server.py --conf telegram.conf

It prints the telegram.conf lines that point RyzikOS at it (QEMU's user
network reaches this computer at 10.0.2.2). In RyzikOS put them in
/Users/<name>/AppData/telegram.conf. Then sign in with any phone number,
the code 22222 (or 12345 to be asked for the password "everos").
"""

import argparse
import gzip
import hashlib
import io
import os
import random
import socketserver
import struct
import threading
import time

from telethon.crypto import AES, AESModeCTR
from telethon.extensions import BinaryReader
from telethon.tl import functions, types
from telethon.tl.tlobject import TLObject

DH_PRIME = int(
    "c71caeb9c6b1c9048e6c522f70f13f73980d40238e3e21c14934d037563d930f"
    "48198a0aa7c14058229493d22530f4dbfa336f6e0ac925139543aed44cce7c37"
    "20fd51f69458705ac68cd4fe6b6b13abdc9746512969328454f18faf8c595f64"
    "2477fe96bb2a941d5bcd1d4ac8cc49880708fa9b378e3c4f3a9060bee67cf9a4"
    "a4a695811051907e162753b56b0f6b410dba74d8a84b2a14b3144e0ef1284754"
    "fd17ed950d5965b4b9dd46582db1178d169c6bc465b0d6ff9ca3928fef5b9ae4"
    "e418fc15e83ebea0f87fa9ff5eed70050ded2849f47bf959d956850ce929851f"
    "0d8115f635b105ee2e4e15d04b2454bf6f4fadf034b10403119cd8e3b92fcc5b",
    16,
)
G = 3
PASSWORD = "everos"
ME = 1000
USERS = {
    ME: ("Jack", "", False, "jack"),
    1001: ("Алиса", "Смирнова", False, "alice"),
    1002: ("Bob", "", False, None),
    1003: ("RyzikBot", "", True, "ryzikbot"),
}
GROUP = 2001
CHANNEL = 3001
# a public channel we are not in, found by the search
SPACE = 3002
HOME_DC = 2
# files by id: (bytes, its picture's bytes or None)
FILES = {}


def picture(w, h, color, text):
    """A JPEG to send as a photo."""
    from PIL import Image, ImageDraw
    img = Image.new("RGB", (w, h), color)
    d = ImageDraw.Draw(img)
    for i in range(0, w, 40):
        d.line([(i, 0), (i + h, h)], fill=tuple(min(255, c + 40) for c in color), width=8)
    d.ellipse([w // 2 - h // 5, h // 2 - h // 5, w // 2 + h // 5, h // 2 + h // 5], fill=(255, 255, 255))
    d.text((12, 12), text, fill=(255, 255, 255))
    out = io.BytesIO()
    img.save(out, "JPEG", quality=80)
    return out.getvalue()


def photo(pid, w, h, color, text, dc=HOME_DC):
    big = picture(w, h, color, text)
    small = picture(w * 320 // max(w, h), h * 320 // max(w, h), color, text)
    FILES[pid] = (big, small)
    return types.MessageMediaPhoto(photo=types.Photo(
        id=pid, access_hash=pid * 3, file_reference=b"ref", date=0, dc_id=dc,
        sizes=[types.PhotoSize("m", small_w(w, h)[0], small_w(w, h)[1], len(small)),
               types.PhotoSize("y", w, h, len(big))]))


def small_w(w, h):
    return w * 320 // max(w, h), h * 320 // max(w, h)


def document(did, name, mime, data, attrs=(), thumb=None):
    FILES[did] = (data, thumb)
    thumbs = None
    if thumb:
        thumbs = [types.PhotoSize("m", 320, 180, len(thumb))]
    return types.MessageMediaDocument(document=types.Document(
        id=did, access_hash=did * 3, file_reference=b"ref", date=0, mime_type=mime, size=len(data),
        dc_id=HOME_DC, attributes=[types.DocumentAttributeFilename(name)] + list(attrs), thumbs=thumbs))


OLD_RSA_ONLY = False


def sha1(*p):
    return hashlib.sha1(b"".join(p)).digest()


def sha256(*p):
    return hashlib.sha256(b"".join(p)).digest()


def ib(n, size=None):
    size = size or (n.bit_length() + 7) // 8
    return n.to_bytes(size, "big")


def bi(b):
    return int.from_bytes(b, "big")


def tl_bytes(b):
    return TLObject.serialize_bytes(b)


def log(*a):
    print("[tg-test]", *a, flush=True)


# ---- RSA key ------------------------------------------------------------------

def rsa_key(path):
    """Our RSA key: (n, d), made once and kept in `path`."""
    if os.path.exists(path):
        n, d = open(path).read().split()
        return int(n, 16), int(d, 16)
    from cryptography.hazmat.primitives.asymmetric import rsa
    k = rsa.generate_private_key(public_exponent=65537, key_size=2048)
    n = k.public_key().public_numbers().n
    d = k.private_numbers().d
    with open(path, "w") as f:
        f.write("%x %x\n" % (n, d))
    return n, d


# ---- the world ------------------------------------------------------------------

class World:
    def __init__(self):
        self.lock = threading.Lock()
        self.next_id = 100
        now = int(time.time())
        self.history = {}  # peer key -> [types.Message]
        # a chat with Alice
        a = ("user", 1001)
        self.add(a, 1001, "Привет! Это тестовый сервер Telegram для RyzikOS.", now - 7200)
        self.add(a, ME, "Hi Alice! Does Russian text work?", now - 7100)
        self.add(a, 1001, "Да, работает: съешь же ещё этих мягких французских булок.", now - 7000)
        for i in range(60):
            self.add(("user", 1002), 1002 if i % 2 else ME, "Message number %d, to test scrolling back in time." % i, now - 90000 + i * 60)
        self.add(("user", 1003), 1003, "I am a bot. Write me anything and I will answer.", now - 300)
        self.add(("chat", GROUP), 1001, "Welcome to the RyzikOS group!", now - 5000)
        self.add(("chat", GROUP), 1002, "A long message that has to wrap over several lines, because it is much wider than a bubble can be in the Telegram window of RyzikOS.", now - 4000)
        self.add(("channel", CHANNEL), None, "RyzikOS now has a Telegram client.", now - 3000, media=True)
        # photos, files, links and emoji
        self.add(a, 1001, "", now - 6000, media=photo(501, 1280, 720, (60, 120, 200), "sea"))
        self.add(a, 1001, "Смотри какой закат \U0001F305\U0001F60D", now - 5900,
                 media=photo(502, 600, 900, (220, 110, 60), "sunset", dc=4))
        self.add(a, ME, "Класс! \U0001F44D\U0001F3FD \u2764\ufe0f", now - 5800)
        notes = "Shopping list:\n- milk\n- bread\n".encode() * 20
        self.add(a, 1001, "Вот список", now - 5700, media=document(601, "notes.txt", "text/plain", notes))
        self.add(a, 1001, "", now - 5600, media=document(602, "big-report.pdf", "application/pdf", os.urandom(1300000)))
        text = "Read https://github.com/cladecarvsna-creator/ryzikos and follow @spacenews or t.me/ryzikos_news"
        self.add(a, 1001, text, now - 5500, entities=[
            types.MessageEntityUrl(text.index("https"), len("https://github.com/cladecarvsna-creator/ryzikos")),
            types.MessageEntityMention(text.index("@space"), len("@spacenews")),
            types.MessageEntityUrl(text.index("t.me"), len("t.me/ryzikos_news"))])
        text = "Our website, with a preview"
        self.add(a, 1001, text, now - 5400, entities=[types.MessageEntityTextUrl(4, 7, "https://example.com/")],
                 media=types.MessageMediaWebPage(webpage=types.WebPage(
                     id=1, url="https://example.com/", display_url="example.com", hash=0,
                     site_name="Example", title="Example Domain: for use in documentation")))
        self.add(("channel", SPACE), None, "Welcome to Space News \U0001F680", now - 20000)
        self.add(("channel", SPACE), None, "The Moon tonight", now - 10000,
                 media=photo(503, 1024, 1024, (30, 30, 60), "moon"))
        self.add(("user", ME), ME, "Notes to self", now - 100000)
        # answers, a forwarded message and pinned ones
        g = ("chat", GROUP)
        first = self.history[g][0].id
        self.add(g, 1002, "Thanks! Glad to be here.", now - 3500, reply_to=first)
        self.add(g, 1001, "Rules: be nice, write in any language.", now - 3400, pinned=True)
        self.add(g, 1002, "Meeting on Friday at 18:00", now - 3300, pinned=True)
        self.add(g, 1001, "Look what I found", now - 3200,
                 fwd_from=types.MessageFwdHeader(date=now - 9000, from_id=types.PeerChannel(CHANNEL)))
        bot = ("user", 1003)
        self.add(bot, 1003, "Pick one:", now - 200, reply_markup=self.bot_buttons())
        self.unread = {a: 1, ("user", 1003): 1}
        self.read_out = {a: 0}
        self.pending = []  # (when, peer, from, text)
        self.joined = set()

    def bot_buttons(self):
        cb = lambda t, d: types.KeyboardInlineButton(t, types.InlineButtonTypeCallback(d))
        return types.ReplyInlineMarkup(rows=[
            types.KeyboardInlineButtonRow([cb("Yes \U0001F44D", b"yes"), cb("No", b"no")]),
            types.KeyboardInlineButtonRow([
                types.KeyboardInlineButton("Open the website", types.InlineButtonTypeUrl("https://example.com/"))]),
        ])

    def bot_keyboard(self):
        k = lambda t: types.KeyboardButton(t, types.ButtonTypeDefault())
        return types.ReplyKeyboardMarkup(rows=[
            types.KeyboardButtonRow([k("Hello"), k("Help")]),
            types.KeyboardButtonRow([k("What time is it?")]),
        ], resize=True)

    def find(self, key, mid):
        for m in self.history.get(key, []):
            if m.id == mid:
                return m
        return None

    def find_any(self, mid):
        """A message of a user or group chat (they share ids), and its chat."""
        for key, msgs in self.history.items():
            if key[0] != "channel":
                for m in msgs:
                    if m.id == mid:
                        return key, m
        return None, None

    def add(self, peer, from_id, text, date, media=False, out_for_me=None, entities=None,
            reply_to=None, fwd_from=None, reply_markup=None, pinned=None):
        self.next_id += 1
        kind, pid = peer
        peer_obj = {"user": types.PeerUser, "chat": types.PeerChat, "channel": types.PeerChannel}[kind](pid)
        m = types.Message(
            id=self.next_id,
            peer_id=peer_obj,
            date=date,
            message=text,
            out=(from_id == ME),
            from_id=types.PeerUser(from_id) if from_id and kind != "user" else None,
            media=(types.MessageMediaPhoto(photo=types.PhotoEmpty(1)) if media is True else media or None),
            entities=entities,
            reply_to=types.MessageReplyHeader(reply_to_msg_id=reply_to) if reply_to else None,
            fwd_from=fwd_from,
            reply_markup=reply_markup,
            pinned=pinned,
        )
        self.history.setdefault(peer, []).append(m)
        return m

    def users(self):
        out = []
        for uid, (first, last, bot, username) in USERS.items():
            status = {1001: types.UserStatusOnline(expires=int(time.time()) + 300),
                      1002: types.UserStatusOffline(was_online=int(time.time()) - 5400)}.get(uid)
            out.append(types.User(id=uid, is_self=uid == ME, bot=bot or None, bot_info_version=1 if bot else None,
                                  access_hash=uid * 7, username=username, status=status,
                                  first_name=first, last_name=last or None, phone="99966" + str(uid)))
        return out

    def chats(self):
        return [
            types.Chat(id=GROUP, title="RyzikOS devs", photo=types.ChatPhotoEmpty(), participants_count=3, date=0, version=1),
            types.Channel(id=CHANNEL, title="RyzikOS News", photo=types.ChatPhotoEmpty(), date=0, broadcast=True,
                          access_hash=555, username="ryzikos_news"),
            types.Channel(id=SPACE, title="Space News", photo=types.ChatPhotoEmpty(), date=0, broadcast=True,
                          access_hash=777, username="spacenews", left=SPACE not in self.joined),
        ]


WORLD = World()


def peer_key(p):
    if isinstance(p, types.InputPeerSelf):
        return ("user", ME)
    if isinstance(p, types.InputPeerUser):
        return ("user", p.user_id)
    if isinstance(p, types.InputPeerChat):
        return ("chat", p.chat_id)
    if isinstance(p, (types.InputPeerChannel, types.InputChannel)):
        return ("channel", p.channel_id)
    raise ValueError(p)


# ---- one client --------------------------------------------------------------------

class RpcError(Exception):
    def __init__(self, code, message):
        self.code, self.message = code, message


class Keys:
    """Authorization keys by key id, kept across connections and restarts
    (in the file `path`)."""
    keys = {}
    path = None

    @classmethod
    def load(cls, path):
        cls.path = path
        if os.path.exists(path):
            for line in open(path):
                key, salt = line.split()
                key = bytes.fromhex(key)
                cls.keys[sha1(key)[12:20]] = [key, int(salt)]

    @classmethod
    def add(cls, key, salt):
        cls.keys[sha1(key)[12:20]] = [key, salt]
        if cls.path:
            with open(cls.path, "a") as f:
                f.write("%s %d\n" % (key.hex(), salt))


class Handler(socketserver.BaseRequestHandler):
    def setup(self):
        self.buf = b""
        self.key = None
        self.salt = 0
        self.session = None
        self.seq = 0
        self.last_id = 0
        self.first = True
        self.signed_in = False
        self.lock = threading.Lock()
        self.alive = True
        # AES-CTR of the obfuscated transport, when the client uses it
        self.dec = None
        self.enc = None
        self.dc = HOME_DC
        self.main = False

    # transport
    def read_exact(self, n):
        while len(self.buf) < n:
            d = self.request.recv(65536)
            if not d:
                raise EOFError
            if self.dec:
                d = self.dec.decrypt(d)
            self.buf += d
        out, self.buf = self.buf[:n], self.buf[n:]
        return out

    def recv_packet(self):
        n = struct.unpack("<I", self.read_exact(4))[0]
        return self.read_exact(n)

    def send_packet(self, data):
        data = struct.pack("<I", len(data)) + data
        with self.lock:
            if self.enc:
                data = self.enc.encrypt(data)
            self.request.sendall(data)

    def msg_id(self):
        t = time.time()
        i = (int(t) << 32) | (int((t % 1) * 2**32) & ~3) | 1
        if i <= self.last_id:
            i = self.last_id + 4
        self.last_id = i
        return i

    def handle(self):
        head = self.read_exact(4)
        if head != b"\xee\xee\xee\xee":
            # obfuscated: 64 bytes with the keys, then all encrypted
            init = head + self.read_exact(60)
            rev = init[8:56][::-1]
            self.dec = AESModeCTR(init[8:40], init[40:56])
            self.enc = AESModeCTR(rev[:32], rev[32:48])
            plain = self.dec.decrypt(init)
            if plain[56:60] != b"\xee\xee\xee\xee":
                log("not the intermediate transport")
                return
            if self.buf:
                self.buf = self.dec.decrypt(self.buf)
            self.dc = abs(struct.unpack("<h", plain[60:62])[0])
            log("obfuscated transport, DC", self.dc)
        log("connection from", self.client_address)
        threading.Thread(target=self.pusher, daemon=True).start()
        try:
            while True:
                p = self.recv_packet()
                if p[:8] == b"\0" * 8:
                    self.plain(p)
                else:
                    self.encrypted(p)
        except EOFError:
            log("closed")
        finally:
            self.alive = False

    # ---- key exchange
    def plain(self, p):
        # like Telegram: a message whose time is far off is ignored
        sent = struct.unpack("<q", p[8:16])[0] >> 32
        if not -300 < sent - time.time() < 30:
            log("ignoring a plain message: client clock off by", int(sent - time.time()), "s")
            return
        length = struct.unpack("<i", p[16:20])[0]
        obj = BinaryReader(p[20:20 + length]).tgread_object()
        name = type(obj).__name__
        if name == "ReqPqMultiRequest":
            self.nonce = obj.nonce
            self.server_nonce = random.getrandbits(128) - 2**127
            ans = types.ResPQ(nonce=obj.nonce, server_nonce=self.server_nonce,
                              pq=ib(0x17ED48941A08F981), server_public_key_fingerprints=[FINGERPRINT])
        elif name == "ReqDHParamsRequest":
            assert obj.public_key_fingerprint == FINGERPRINT
            m = pow(bi(obj.encrypted_data), RSA_D, RSA_N).to_bytes(256, "big")
            temp_key = bytes(a ^ b for a, b in zip(m[:32], sha256(m[32:])))
            data_with_hash = AES.decrypt_ige(m[32:], temp_key, bytes(32))
            padded = data_with_hash[:192][::-1]
            if sha256(temp_key, padded) == data_with_hash[192:] and not OLD_RSA_ONLY:
                inner = BinaryReader(padded).tgread_object()
                log("key exchange: RSA_PAD")
            else:
                # the older way: a zero byte, SHA-1 of the data, the data
                inner = BinaryReader(m[21:]).tgread_object() if m[0] == 0 else None
                if inner is None or sha1(bytes(inner)) != m[1:21]:
                    log("key exchange: can't read the client's half, answering -404")
                    self.send_packet(struct.pack("<i", -404))
                    return
                log("key exchange: older RSA")
            assert inner.p == ib(0x494C553B) and inner.q == ib(0x53911073), "p, q"
            log("key exchange: client DC", getattr(inner, "dc", "not given"))
            self.new_nonce = inner.new_nonce.to_bytes(32, "little", signed=True)
            sn = self.server_nonce.to_bytes(16, "little", signed=True)
            self.a = random.getrandbits(2048)
            answer = bytes(types.ServerDHInnerData(nonce=self.nonce, server_nonce=self.server_nonce, g=G,
                                                   dh_prime=ib(DH_PRIME), g_a=ib(pow(G, self.a, DH_PRIME)),
                                                   server_time=int(time.time())))
            wh = sha1(answer) + answer
            wh += os.urandom(-len(wh) % 16)
            self.tmp_key = sha1(self.new_nonce, sn) + sha1(sn, self.new_nonce)[:12]
            self.tmp_iv = sha1(sn, self.new_nonce)[12:] + sha1(self.new_nonce, self.new_nonce) + self.new_nonce[:4]
            ans = types.ServerDHParamsOk(nonce=self.nonce, server_nonce=self.server_nonce,
                                         encrypted_answer=AES.encrypt_ige(wh, self.tmp_key, self.tmp_iv))
        elif name == "SetClientDHParamsRequest":
            plain = AES.decrypt_ige(obj.encrypted_data, self.tmp_key, self.tmp_iv)
            r = BinaryReader(plain[20:])
            inner = r.tgread_object()
            assert sha1(plain[20:20 + r.tell_position()]) == plain[:20], "client DH hash"
            key = ib(pow(bi(inner.g_b), self.a, DH_PRIME), 256)
            aux = sha1(key)[:8]
            h1 = sha1(self.new_nonce, b"\x01", aux)[4:20]
            sn = self.server_nonce.to_bytes(16, "little", signed=True)
            salt = struct.unpack("<q", bytes(a ^ b for a, b in zip(self.new_nonce[:8], sn[:8])))[0]
            Keys.add(key, salt)
            log("key exchange: done, key id", sha1(key)[12:20].hex())
            ans = types.DhGenOk(nonce=self.nonce, server_nonce=self.server_nonce,
                                new_nonce_hash1=int.from_bytes(h1, "little", signed=True))
        else:
            log("unexpected plain message", name)
            return
        body = bytes(ans)
        self.send_packet(b"\0" * 8 + struct.pack("<qi", self.msg_id(), len(body)) + body)

    # ---- encrypted messages
    def keys_for(self, msg_key, x):
        k = self.key
        a = sha256(msg_key, k[x:x + 36])
        b = sha256(k[40 + x:76 + x], msg_key)
        return a[:8] + b[8:24] + a[24:32], b[:8] + a[8:24] + b[24:32]

    def encrypted(self, p):
        key_id = p[:8]
        if key_id not in Keys.keys:
            log("unknown key: -404")
            self.send_packet(struct.pack("<i", -404))
            return
        self.key, salt = Keys.keys[key_id]
        msg_key = p[8:24]
        k, iv = self.keys_for(msg_key, 0)
        plain = AES.decrypt_ige(p[24:], k, iv)
        assert sha256(self.key[88:120], plain)[8:24] == msg_key, "msg_key"
        msg_salt, session, msg_id, seq, length = struct.unpack("<qqqii", plain[:32])
        assert len(plain) - 32 - length >= 12, "padding too short"
        body = plain[32:32 + length]
        server_time = msg_id >> 32
        if abs(server_time - time.time()) > 300:
            log("client clock off by", int(server_time - time.time()), "s")
            code = 16 if server_time < time.time() else 17
            self.send(bytes(types.BadMsgNotification(msg_id, seq, code)), session)
            return
        if self.session != session:
            self.session = session
            self.send(bytes(types.NewSessionCreated(msg_id, random.getrandbits(63), Keys.keys[key_id][1])), session)
        if msg_salt != Keys.keys[key_id][1]:
            log("wrong salt: bad_server_salt")
            self.send(bytes(types.BadServerSalt(msg_id, seq, 48, Keys.keys[key_id][1])), session)
            return
        obj = BinaryReader(body).tgread_object()
        self.rpc(msg_id, obj)

    def send(self, body, session=None, content=True):
        salt = Keys.keys[sha1(self.key)[12:20]][1]
        self.seq += 1
        plain = struct.pack("<qqqii", salt, session or self.session, self.msg_id(), self.seq * 2 - 1, len(body)) + body
        plain += os.urandom(12 + (-(len(plain) + 12) % 16))
        msg_key = sha256(self.key[96:128], plain)[8:24]
        k, iv = self.keys_for(msg_key, 8)
        self.send_packet(sha1(self.key)[12:20] + msg_key + AES.encrypt_ige(plain, k, iv))

    def rpc(self, msg_id, obj):
        while isinstance(obj, (functions.InvokeWithLayerRequest, functions.InitConnectionRequest)):
            if isinstance(obj, functions.InitConnectionRequest):
                log("initConnection: api_id", obj.api_id, obj.device_model, obj.system_version)
            obj = obj.query
        name = type(obj).__name__
        if name == "MsgsAck":
            return
        log("call", name)
        try:
            result = self.call(obj)
            if isinstance(result, list):
                data = struct.pack("<Ii", 0x1CB5C415, len(result)) + b"".join(bytes(x) for x in result)
            else:
                data = bytes(result)
        except RpcError as e:
            log("  error", e.message)
            data = bytes(types.RpcError(e.code, e.message))
        if len(data) > 512:
            data = struct.pack("<I", 0x3072CFA1) + tl_bytes(gzip.compress(data))
        body = struct.pack("<Iq", 0xF35C6D01, msg_id) + data
        if name == "GetDialogsRequest":
            # in a container, with a pong, to exercise that path
            pong = bytes(types.Pong(0, 1))
            items = [(self.msg_id(), 1, pong), (self.msg_id(), 3, body)]
            c = struct.pack("<II", 0x73F1F8DC, len(items))
            for i, s, b in items:
                c += struct.pack("<qii", i, s, len(b)) + b
            self.send(c)
        else:
            self.send(body)

    def call(self, obj):
        n = type(obj).__name__
        if n == "PingDelayDisconnectRequest":
            return types.Pong(0, obj.ping_id)
        if n == "SendCodeRequest":
            log("  phone", obj.phone_number, "api_id", obj.api_id)
            if not obj.phone_number.lstrip("+").isdigit():
                raise RpcError(400, "PHONE_NUMBER_INVALID")
            return types.auth.SentCode(type=types.auth.SentCodeTypeApp(5), phone_code_hash="hash123")
        if n == "SignInRequest":
            if obj.phone_code_hash != "hash123":
                raise RpcError(400, "PHONE_CODE_HASH_EMPTY")
            if obj.phone_code == "12345":
                raise RpcError(401, "SESSION_PASSWORD_NEEDED")
            if obj.phone_code != "22222":
                raise RpcError(400, "PHONE_CODE_INVALID")
            return self.authorization()
        if n == "GetPasswordRequest":
            self.srp_b = random.getrandbits(2048)
            salt1, salt2 = b"salt-one" + os.urandom(8), b"salt-two"
            self.salts = (salt1, salt2)
            x = bi(srp_hash(salt1, salt2, PASSWORD))
            self.v = pow(G, x, DH_PRIME)
            k = bi(sha256(ib(DH_PRIME, 256), ib(G, 256)))
            self.B = (k * self.v + pow(G, self.srp_b, DH_PRIME)) % DH_PRIME
            algo = types.PasswordKdfAlgoSHA256SHA256PBKDF2HMACSHA512iter100000SHA256ModPow(
                salt1=salt1, salt2=salt2, g=G, p=ib(DH_PRIME))
            return types.account.Password(
                new_algo=types.PasswordKdfAlgoUnknown(), new_secure_algo=types.SecurePasswordKdfAlgoUnknown(),
                secure_random=os.urandom(32), has_password=True, current_algo=algo,
                srp_B=ib(self.B, 256), srp_id=77, hint="the name of the OS")
        if n == "CheckPasswordRequest":
            A = bi(obj.password.A)
            u = bi(sha256(ib(A, 256), ib(self.B, 256)))
            S = pow(A * pow(self.v, u, DH_PRIME), self.srp_b, DH_PRIME)
            K = sha256(ib(S, 256))
            hp, hg = sha256(ib(DH_PRIME, 256)), sha256(ib(G, 256))
            m1 = sha256(bytes(a ^ b for a, b in zip(hp, hg)), sha256(self.salts[0]), sha256(self.salts[1]),
                        ib(A, 256), ib(self.B, 256), K)
            if m1 != obj.password.M1:
                raise RpcError(400, "PASSWORD_HASH_INVALID")
            log("  password ok")
            return self.authorization()
        if not self.signed_in and n not in ("GetUsersRequest",):
            pass
        if n == "GetUsersRequest":
            return [u for u in WORLD.users() if u.id == ME]
        if n == "GetDialogsRequest":
            self.main = True
            return self.dialogs()
        if n == "GetStateRequest":
            self.main = True
            return types.updates.State(pts=1, qts=0, date=int(time.time()), seq=1, unread_count=0)
        if n == "GetHistoryRequest":
            key = peer_key(obj.peer)
            msgs = [m for m in WORLD.history.get(key, []) if obj.offset_id == 0 or m.id < obj.offset_id]
            msgs = list(reversed(msgs))[: obj.limit]
            total = len(WORLD.history.get(key, []))
            return types.messages.MessagesSlice(count=total, messages=msgs, chats=WORLD.chats(), users=WORLD.users(),
                                                topics=[])
        if n == "ReadHistoryRequest":
            WORLD.unread.pop(peer_key(obj.peer), None)
            return types.messages.AffectedMessages(pts=1, pts_count=0)
        if n == "SendMessageRequest":
            key = peer_key(obj.peer)
            reply = getattr(obj.reply_to, "reply_to_msg_id", None)
            m = WORLD.add(key, ME, obj.message, int(time.time()), reply_to=reply)
            log("  message to", key, repr(obj.message), "answering %s" % reply if reply else "")
            if key[0] == "user" and key[1] != ME:
                # first "typing...", then an answer that quotes ours
                WORLD.pending.append((time.time() + 0.6, key, key[1], "typing"))
                WORLD.pending.append((time.time() + 3, key, key[1], ("You wrote: " + obj.message, m.id)))
                WORLD.pending.append((time.time() + 4, key, None, "read"))
            return types.UpdateShortSentMessage(id=m.id, pts=1, pts_count=1, date=m.date, out=True)
        if n == "ExportLoginTokenRequest":
            if getattr(self, "qr_scanned", False):
                log("  QR code scanned")
                return types.auth.LoginTokenSuccess(authorization=self.authorization())
            if not getattr(self, "qr_shown", False):
                # the phone scans the code a few seconds after it is shown
                self.qr_shown = True
                threading.Thread(target=self.scan_qr, daemon=True).start()
            return types.auth.LoginToken(expires=int(time.time()) + 30, token=os.urandom(32))
        if n == "SaveFilePartRequest":
            self.parts = getattr(self, "parts", {})
            self.parts.setdefault(obj.file_id, {})[obj.file_part] = obj.bytes
            return struct.pack("<I", 0x997275B5)  # boolTrue
        if n == "SendMediaRequest":
            key = peer_key(obj.peer)
            parts = self.parts.pop(obj.media.file.id, {})
            data = b"".join(parts[i] for i in sorted(parts))
            log("  file to", key, type(obj.media).__name__, obj.media.file.name, len(data), "bytes")
            # keep the file, so it shows (and downloads) like a real one
            fid = 700 + len(FILES)
            if isinstance(obj.media, types.InputMediaUploadedPhoto):
                from PIL import Image
                img = Image.open(io.BytesIO(data))
                small = io.BytesIO()
                img.convert("RGB").resize(small_w(*img.size)).save(small, "JPEG")
                FILES[fid] = (data, small.getvalue())
                w, h = img.size
                media = types.MessageMediaPhoto(photo=types.Photo(
                    id=fid, access_hash=fid * 3, file_reference=b"ref", date=0, dc_id=HOME_DC,
                    sizes=[types.PhotoSize("m", *small_w(w, h), len(small.getvalue())),
                           types.PhotoSize("y", w, h, len(data))]))
            else:
                media = document(fid, obj.media.file.name, obj.media.mime_type, data)
            reply = getattr(obj.reply_to, "reply_to_msg_id", None)
            m = WORLD.add(key, ME, obj.message, int(time.time()), media=media, reply_to=reply)
            return types.Updates(updates=[types.UpdateMessageID(id=m.id, random_id=obj.random_id),
                                          types.UpdateNewMessage(message=m, pts=1, pts_count=1)],
                                 users=WORLD.users(), chats=WORLD.chats(), date=m.date, seq=0)
        if n == "LogOutRequest":
            return types.auth.LoggedOut()
        if n == "GetFileRequest":
            loc = obj.location
            if loc.id not in FILES:
                raise RpcError(400, "FILE_ID_INVALID")
            if loc.id == 502 and self.dc != 4:
                raise RpcError(303, "FILE_MIGRATE_4")
            data, thumb = FILES[loc.id]
            if loc.thumb_size == "m":
                data = thumb
            log("  file", loc.id, loc.thumb_size or "whole", obj.offset, "of", len(data))
            time.sleep(0.2)
            return types.upload.File(type=types.storage.FileUnknown(), mtime=0,
                                     bytes=data[obj.offset:obj.offset + obj.limit])
        if n == "ExportAuthorizationRequest":
            return types.auth.ExportedAuthorization(id=ME, bytes=b"exported")
        if n == "ImportAuthorizationRequest":
            if obj.bytes != b"exported":
                raise RpcError(400, "AUTH_BYTES_INVALID")
            return self.authorization()
        if isinstance(obj, functions.messages.SearchRequest):
            key = peer_key(obj.peer)
            msgs = [m for m in reversed(WORLD.history.get(key, []))
                    if isinstance(obj.filter, types.InputMessagesFilterPinned) and m.pinned]
            return types.messages.Messages(messages=msgs[: obj.limit], chats=WORLD.chats(), users=WORLD.users(),
                                           topics=[])
        if isinstance(obj, (functions.messages.GetMessagesRequest, functions.channels.GetMessagesRequest)):
            found = []
            for i in obj.id:
                if isinstance(obj, functions.channels.GetMessagesRequest):
                    m = WORLD.find(peer_key(obj.channel), i.id)
                else:
                    m = WORLD.find_any(i.id)[1]
                found.append(m or types.MessageEmpty(id=i.id))
            return types.messages.Messages(messages=found, chats=WORLD.chats(), users=WORLD.users(), topics=[])
        if n == "EditMessageRequest":
            key = peer_key(obj.peer)
            m = WORLD.find(key, obj.id)
            if m is None:
                raise RpcError(400, "MESSAGE_ID_INVALID")
            if not m.out:
                raise RpcError(403, "MESSAGE_AUTHOR_REQUIRED")
            if m.message == obj.message:
                raise RpcError(400, "MESSAGE_NOT_MODIFIED")
            m.message, m.edit_date, m.entities = obj.message, int(time.time()), None
            log("  edited", obj.id, repr(obj.message))
            return types.Updates(updates=[types.UpdateEditMessage(message=m, pts=1, pts_count=1)],
                                 users=WORLD.users(), chats=WORLD.chats(), date=int(time.time()), seq=0)
        if isinstance(obj, (functions.messages.DeleteMessagesRequest, functions.channels.DeleteMessagesRequest)):
            for mid in obj.id:
                if isinstance(obj, functions.channels.DeleteMessagesRequest):
                    key = peer_key(obj.channel)
                else:
                    key = WORLD.find_any(mid)[0]
                if key:
                    WORLD.history[key] = [m for m in WORLD.history[key] if m.id != mid]
            log("  deleted", obj.id, "revoke" if getattr(obj, "revoke", True) else "just for me")
            return types.messages.AffectedMessages(pts=1, pts_count=len(obj.id))
        if n == "ForwardMessagesRequest":
            src, dst = peer_key(obj.from_peer), peer_key(obj.to_peer)
            ups = []
            for mid, rid in zip(obj.id, obj.random_id):
                m = WORLD.find(src, mid)
                if m is None:
                    raise RpcError(400, "MESSAGE_ID_INVALID")
                orig = m.from_id or (types.PeerUser(ME) if m.out else m.peer_id)
                new = WORLD.add(dst, ME, m.message, int(time.time()), media=m.media,
                                fwd_from=types.MessageFwdHeader(date=m.date, from_id=orig))
                ups += [types.UpdateMessageID(id=new.id, random_id=rid),
                        types.UpdateNewMessage(message=new, pts=1, pts_count=1)]
            log("  forwarded", obj.id, "from", src, "to", dst)
            return types.Updates(updates=ups, users=WORLD.users(), chats=WORLD.chats(), date=int(time.time()), seq=0)
        if n == "UpdatePinnedMessageRequest":
            key = peer_key(obj.peer)
            m = WORLD.find(key, obj.id)
            if m is None:
                raise RpcError(400, "MESSAGE_ID_INVALID")
            m.pinned = None if obj.unpin else True
            log("  unpinned" if obj.unpin else "  pinned", obj.id)
            peer = {"user": types.PeerUser, "chat": types.PeerChat, "channel": types.PeerChannel}[key[0]](key[1])
            return types.Updates(updates=[types.UpdatePinnedMessages(peer=peer, messages=[obj.id], pts=1, pts_count=1,
                                                                     pinned=None if obj.unpin else True)],
                                 users=WORLD.users(), chats=WORLD.chats(), date=int(time.time()), seq=0)
        if n == "SetTypingRequest":
            log("  typing in", peer_key(obj.peer), type(obj.action).__name__)
            return struct.pack("<I", 0x997275B5)  # boolTrue
        if n == "UpdateStatusRequest":
            log("  we are", "offline" if obj.offline else "online")
            return struct.pack("<I", 0x997275B5)
        if n == "GetBotCallbackAnswerRequest":
            log("  button pressed:", obj.data)
            if obj.data == b"yes":
                return types.messages.BotCallbackAnswer(cache_time=0, message="You said yes!")
            return types.messages.BotCallbackAnswer(cache_time=0, message="Maybe next time")
        if n == "SearchRequest":
            q = obj.q.lower().lstrip("@")
            found = [c for c in WORLD.chats() if q in c.title.lower() or q in (getattr(c, "username", "") or "")]
            users = [u for u in WORLD.users() if u.id != ME and (q in u.first_name.lower() or q in (u.username or ""))]
            return types.contacts.Found(
                my_results=[], results=[types.PeerChannel(c.id) if isinstance(c, types.Channel) else types.PeerChat(c.id)
                                        for c in found] + [types.PeerUser(u.id) for u in users],
                chats=WORLD.chats(), users=WORLD.users())
        if n == "ResolveUsernameRequest":
            name = obj.username.lower()
            for c in WORLD.chats():
                if getattr(c, "username", None) == name:
                    return types.contacts.ResolvedPeer(peer=types.PeerChannel(c.id), chats=WORLD.chats(),
                                                       users=WORLD.users())
            for u in WORLD.users():
                if u.username == name:
                    return types.contacts.ResolvedPeer(peer=types.PeerUser(u.id), chats=[], users=WORLD.users())
            raise RpcError(400, "USERNAME_NOT_OCCUPIED")
        if n in ("JoinChannelRequest", "LeaveChannelRequest"):
            cid = obj.channel.channel_id
            if n == "JoinChannelRequest":
                WORLD.joined.add(cid)
            else:
                WORLD.joined.discard(cid)
            log("  joined" if n == "JoinChannelRequest" else "  left", cid)
            return types.Updates(updates=[], users=[], chats=WORLD.chats(), date=int(time.time()), seq=0)
        if n == "GetFullChannelRequest":
            cid = obj.channel.channel_id
            about = {SPACE: "News about space, rockets and the Moon \U0001F319", CHANNEL: "What is new in RyzikOS"}
            full = types.ChannelFull(
                id=cid, about=about.get(cid, ""), participants_count=12345 if cid == SPACE else 42,
                read_inbox_max_id=0, read_outbox_max_id=0, unread_count=0, chat_photo=types.PhotoEmpty(0),
                notify_settings=types.PeerNotifySettings(), exported_invite=None, bot_info=[], pts=1)
            return types.messages.ChatFull(full_chat=full, chats=WORLD.chats(), users=[])
        if n == "GetFullUserRequest":
            uid = getattr(obj.id, "user_id", ME)
            full = types.UserFull(id=uid, settings=types.PeerSettings(), notify_settings=types.PeerNotifySettings(),
                                  common_chats_count=1, about="Hi, I am %s \U0001F44B" % USERS[uid][0])
            return types.users.UserFull(full_user=full, chats=[], users=WORLD.users())
        raise RpcError(400, "METHOD_NOT_IN_TEST_SERVER")

    def scan_qr(self):
        time.sleep(4)
        self.qr_scanned = True
        self.send(bytes(types.UpdateShort(update=types.UpdateLoginToken(), date=int(time.time()))))

    def authorization(self):
        self.signed_in = True
        me = [u for u in WORLD.users() if u.id == ME][0]
        return types.auth.Authorization(user=me)

    def dialogs(self):
        dialogs, tops = [], []
        order = sorted(WORLD.history.items(), key=lambda kv: -kv[1][-1].date)
        for key, msgs in order:
            if key == ("channel", SPACE) and SPACE not in WORLD.joined:
                continue
            peer = {"user": types.PeerUser, "chat": types.PeerChat, "channel": types.PeerChannel}[key[0]](key[1])
            dialogs.append(types.Dialog(peer=peer, top_message=msgs[-1].id, read_inbox_max_id=0,
                                        read_outbox_max_id=WORLD.read_out.get(key, 0), unread_count=WORLD.unread.get(key, 0),
                                        unread_mentions_count=0, unread_reactions_count=0, unread_poll_votes_count=0,
                                        notify_settings=types.PeerNotifySettings(), pinned=key == ("chat", GROUP)))
            tops.append(msgs[-1])
        return types.messages.Dialogs(dialogs=dialogs, messages=tops, chats=WORLD.chats(), users=WORLD.users())

    def pusher(self):
        """Answers that come later, pushed as updates."""
        while self.alive:
            time.sleep(0.3)
            now = time.time()
            due = [p for p in WORLD.pending if p[0] <= now]
            if not due or self.key is None or not self.main:
                continue
            for p in due:
                WORLD.pending.remove(p)
                _, key, from_id, text = p
                if text == "typing":
                    self.send(bytes(types.UpdateShort(update=types.UpdateUserTyping(
                        user_id=from_id, action=types.SendMessageTypingAction()), date=int(now))))
                    log("  pushed typing")
                    continue
                reply = None
                if isinstance(text, tuple):
                    text, reply = text
                if text == "read" and from_id is None:
                    # the other side read our messages
                    WORLD.read_out[key] = WORLD.history[key][-1].id
                    upd = types.UpdateShort(update=types.UpdateReadHistoryOutbox(
                        peer=types.PeerUser(key[1]), max_id=WORLD.read_out[key], pts=1, pts_count=1),
                        date=int(now))
                    self.send(bytes(upd))
                    continue
                markup = None
                if from_id == 1003:
                    markup = WORLD.bot_buttons() if random.random() < 0.5 else WORLD.bot_keyboard()
                m = WORLD.add(key, from_id, text, int(now), reply_to=reply, reply_markup=markup)
                if random.random() < 0.5 and markup is None:
                    upd = types.UpdateShortMessage(id=m.id, user_id=key[1], message=text, pts=1, pts_count=1,
                                                   date=m.date, reply_to=m.reply_to)
                else:
                    upd = types.Updates(updates=[types.UpdateNewMessage(message=m, pts=1, pts_count=1)],
                                        users=WORLD.users(), chats=[], date=m.date, seq=0)
                log("  pushed", type(upd).__name__)
                self.send(bytes(upd))


def srp_hash(salt1, salt2, password):
    h1 = sha256(salt1, password.encode(), salt1)
    h2 = sha256(salt2, h1, salt2)
    h3 = hashlib.pbkdf2_hmac("sha512", h2, salt1, 100000)
    return sha256(salt2, h3, salt2)


class Server(socketserver.ThreadingTCPServer):
    allow_reuse_address = True
    daemon_threads = True


if __name__ == "__main__":
    ap = argparse.ArgumentParser()
    ap.add_argument("--port", type=int, default=8443)
    ap.add_argument("--key", default="tg-test-server.key", help="where to keep the RSA key")
    ap.add_argument("--conf", help="also write a telegram.conf for RyzikOS here")
    ap.add_argument("--old-rsa-only", action="store_true",
                    help="answer RSA_PAD with -404, like a server that can't read it")
    args = ap.parse_args()
    OLD_RSA_ONLY = args.old_rsa_only
    RSA_N, RSA_D = rsa_key(args.key)
    Keys.load(args.key + ".sessions")
    FINGERPRINT = struct.unpack("<q", sha1(tl_bytes(ib(RSA_N)) + tl_bytes(b"\x01\x00\x01"))[-8:])[0]
    conf = "api_id=12345\r\napi_hash=0123456789abcdef0123456789abcdef\r\nserver=10.0.2.2:%d\r\nrsa=%x\r\n" % (
        args.port, RSA_N)
    if args.conf:
        open(args.conf, "w").write(conf)
    print(conf)
    log("listening on port", args.port)
    Server(("0.0.0.0", args.port), Handler).serve_forever()
