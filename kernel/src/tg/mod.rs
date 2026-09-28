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
    /// Load the picture of a message (a photo, or a file's preview) to
    /// show in the chat.
    Preview(Peer, i64),
    /// Download a message's photo or file to Downloads; `true` opens it
    /// when it is there.
    Download(Peer, i64, bool),
    /// Look for people, groups and channels on Telegram.
    Search(String),
    /// Show a chat found by the search (or already in the list).
    Show(Peer),
    /// Open @name, t.me/name or t.me/name/123.
    Resolve(String),
    Join(Peer),
    Leave(Peer),
    /// Load the details of a chat for its profile: about, members.
    Info(Peer),
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
    /// The public @name, without the @.
    pub username: String,
    /// A channel or group we are not in (found by the search or a link).
    pub left: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ChatKind {
    Private,
    Saved,
    Bot,
    Group,
    Channel,
}

#[derive(Clone, Debug, Default)]
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
    pub photo: Option<Photo>,
    pub file: Option<FileInfo>,
    /// Links in the text: (first char, end char, where to), in chars.
    pub links: Vec<(usize, usize, String)>,
    /// A link preview: the site and title.
    pub web: Option<(String, String)>,
}

/// Where Telegram keeps a file, and which data centre has it.
#[derive(Clone, Debug)]
pub struct FileRef {
    pub id: i64,
    pub access_hash: i64,
    pub file_reference: Vec<u8>,
    pub dc: i32,
    pub photo: bool,
}

#[derive(Clone, Debug)]
pub struct Photo {
    pub file: FileRef,
    pub w: i32,
    pub h: i32,
    /// The size to show in the chat ("m", "x", ...) and the biggest.
    pub small: String,
    pub big: String,
    pub big_size: i64,
}

#[derive(Clone, Debug)]
pub struct FileInfo {
    pub file: FileRef,
    pub name: String,
    pub mime: String,
    pub size: i64,
    /// "Video", "Music", "Voice message", "Sticker", "GIF" or "File".
    pub kind: String,
    /// The preview picture's size type, for videos, stickers and photos
    /// sent as files; its width and height.
    pub thumb: Option<(String, i32, i32)>,
}

/// A picture loaded for the chat, or on its way.
pub enum Preview {
    Loading,
    Ready(crate::web::image::Image),
    Failed,
}

impl core::fmt::Debug for Preview {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        f.write_str(match self {
            Preview::Loading => "Loading",
            Preview::Ready(_) => "Ready",
            Preview::Failed => "Failed",
        })
    }
}

/// A download to Downloads.
#[derive(Clone, Debug, Default)]
pub struct Download {
    pub done: i64,
    pub total: i64,
    /// Where it was saved, once it is all there.
    pub path: Option<String>,
    pub failed: Option<String>,
}

/// What the profile of a chat shows.
#[derive(Clone, Debug, Default)]
pub struct Info {
    pub about: String,
    /// Members or subscribers; 0 when not known.
    pub members: i64,
    pub phone: String,
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
    /// Pictures for the chat by (chat, message).
    pub previews: BTreeMap<(Peer, i64), Preview>,
    pub downloads: BTreeMap<(Peer, i64), Download>,
    /// What the search found on Telegram, for the query in the box.
    pub found: Option<(String, Vec<Chat>)>,
    /// A chat for the window to open (from a link or the search).
    pub goto: Option<Peer>,
    pub info: BTreeMap<Peer, Info>,
    /// Files to open, for the desktop.
    pub to_open: Vec<String>,
    /// A short note for the window to show once, like a link that led
    /// nowhere.
    pub notice: Option<String>,
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
            previews: BTreeMap::new(),
            downloads: BTreeMap::new(),
            found: None,
            goto: None,
            info: BTreeMap::new(),
            to_open: Vec::new(),
            notice: None,
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
