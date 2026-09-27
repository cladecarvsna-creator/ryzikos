//! A Telegram client: MTProto 2.0 over TCP (mtproto.rs), the TL format
//! read from the official schema (tl.rs), and the client logic (client.rs)
//! that signs in, keeps the chat list and the open chats, and sends and
//! receives messages. The window is gui/telegram.rs.
//!
//! The client runs in a fiber, so waiting for the network never stops
//! the desktop. It and the window share a [`Shared`]: the window puts
//! commands in, the client puts chats and messages in.
//!
//! A release build carries RyzikOS's own api_id and api_hash (the
//! RYZIKOS_TG_API_ID and RYZIKOS_TG_API_HASH secrets of the build); without
//! them every user brings their own from my.telegram.org. They are kept in
//! `/Users/<name>/AppData/telegram.conf` and the signed-in session in
//! `telegram.session` next to it.

pub mod client;
pub mod crypto;
pub mod mtproto;
pub mod tl;

use alloc::collections::{BTreeMap, VecDeque};
use alloc::string::String;
use alloc::vec::Vec;

/// A chat: a user, a basic group or a channel (supergroups are channels).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Peer {
    User(i64),
    Chat(i64),
    Channel(i64),
}

/// What the client is doing, and what the window should show.
#[derive(Clone, PartialEq, Debug)]
pub enum Stage {
    /// Reading settings or connecting.
    Starting,
    /// Needs the api_id and api_hash.
    Config,
    Phone,
    /// Log in by scanning a QR code with the phone: the tg://login link
    /// to show as the code.
    Qr(String),
    /// Waiting for the login code; says where it was sent.
    Code(String),
    /// Two-step verification; the password hint.
    Password(String),
    Ready,
}

#[derive(Clone, Debug)]
pub enum Cmd {
    Config {
        api_id: String,
        api_hash: String,
    },
    Phone(String),
    /// Switch to logging in with a QR code.
    UseQr,
    /// Switch back to logging in with the phone number.
    UsePhone,
    Code(String),
    Password(String),
    /// Show a chat: load its messages and mark them read.
    Open(Peer),
    /// Load older messages of a chat.
    Older(Peer),
    Send(Peer, String),
    /// Send a file from the disk.
    SendFile(Peer, String),
    Reload,
    /// The client's own timer, not the window's.
    Wake,
    LogOut,
}

#[derive(Clone, Debug)]
pub struct Chat {
    pub peer: Peer,
    pub title: String,
    /// The last message, as one line.
    pub last: String,
    pub last_out: bool,
    pub date: i64,
    pub unread: i64,
    pub pinned: bool,
    /// Our messages up to this one were read.
    pub read_out: i64,
    pub kind: ChatKind,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ChatKind {
    Private,
    Saved,
    Bot,
    Group,
    Channel,
}

#[derive(Clone, Debug)]
pub struct Message {
    /// 0 while it is being sent.
    pub id: i64,
    pub out: bool,
    /// Who wrote it, for groups.
    pub from: String,
    pub from_id: i64,
    pub text: String,
    /// "Photo", "Sticker", ... for messages with something attached.
    pub media: Option<String>,
    /// A service message ("joined the group"), drawn in the middle.
    pub service: bool,
    pub date: i64,
    pub edited: bool,
    /// Our random id while it is being sent.
    pub random_id: i64,
    pub failed: bool,
}

#[derive(Default, Debug)]
pub struct History {
    /// Oldest first.
    pub messages: Vec<Message>,
    /// Everything back to the first message is here.
    pub complete: bool,
    pub loading: bool,
}

#[derive(Debug)]
pub struct Shared {
    pub stage: Stage,
    /// Shown under the form or at the top of the chats.
    pub error: Option<String>,
    /// Waiting for the server on something the user asked for.
    pub busy: bool,
    /// Whether the client is connected.
    pub online: bool,
    pub chats: Vec<Chat>,
    pub history: BTreeMap<Peer, History>,
    /// Our own name.
    pub me: String,
    pub commands: VecDeque<Cmd>,
    /// Bumped on every change, so the window knows to draw again.
    pub version: u64,
    /// Seconds to add to a Telegram time to get the local time.
    pub tz: i64,
    /// The chat the window shows: new messages there are read at once.
    pub open: Option<Peer>,
}

impl Shared {
    pub fn new() -> Shared {
        Shared {
            stage: Stage::Starting,
            error: None,
            busy: false,
            online: false,
            chats: Vec::new(),
            history: BTreeMap::new(),
            me: String::new(),
            commands: VecDeque::new(),
            version: 1,
            tz: 0,
            open: None,
        }
    }

    pub fn changed(&mut self) {
        self.version += 1;
    }

    pub fn chat(&self, peer: Peer) -> Option<&Chat> {
        self.chats.iter().find(|c| c.peer == peer)
    }
}

impl Default for Shared {
    fn default() -> Self {
        Self::new()
    }
}

/// Check the cryptography against known answers, for the boot test and
/// the shell's `telegram selftest`. Returns what failed.
pub fn self_test() -> Result<(), &'static str> {
    // AES-256-IGE, the answer Telethon gives for the same input
    let key: [u8; 32] = core::array::from_fn(|i| i as u8);
    let iv: [u8; 32] = core::array::from_fn(|i| 32 + i as u8);
    let plain = [0x41u8; 32];
    let enc = crypto::ige_encrypt(&plain, &key, &iv);
    let expected = crypto::from_hex(concat!(
        "bf7297874c7c82d813bb4ea09d70cde4",
        "deb247ac0342ef61b416dbfbb3efd30f"
    ));
    if enc != expected || crypto::ige_decrypt(&enc, &key, &iv) != plain {
        return Err("aes-ige");
    }
    if crypto::factor(0x17ED48941A08F981) != Some((0x494C553B, 0x53911073)) {
        return Err("factoring pq");
    }
    // the fingerprints Telegram gives for its keys
    for i in 0..crypto::SERVER_KEY_COUNT {
        let (f, n) = crypto::server_key_at(i);
        if crypto::fingerprint(&n) != f {
            return Err("rsa key fingerprints");
        }
    }
    let s = tl::schema();
    let m = s.by_name("message").ok_or("schema: message")?;
    if m.id != 0x7600_b9d3 {
        return Err("schema: message id");
    }
    let o = tl::Obj::new(
        "inputPeerUser",
        &[("user_id", 777i64.into()), ("access_hash", (-5i64).into())],
    );
    let bytes = tl::encode(&o);
    if tl::Reader::new(&bytes).obj().ok() != Some(o) {
        return Err("tl round trip");
    }
    Ok(())
}
