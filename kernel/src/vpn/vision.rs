//! XTLS Vision, the "flow" most VLESS REALITY servers require. While the
//! program inside the tunnel is doing its own TLS handshake, both sides
//! wrap what they send in blocks padded to random lengths, so the sizes
//! of the inner handshake don't show. A block is
//! `[uuid on the first] command, content length, padding length (2 bytes
//! each), content, padding`. Command 0 means more blocks follow, 1 means
//! the padding is over, and 2 means the server stops using the outer TLS
//! as well and sends the inner TLS as it is.

use alloc::vec::Vec;

use crate::tg::crypto::random;

const CONTINUE: u8 = 0;
const END: u8 = 1;
const DIRECT: u8 = 2;

fn rand_below(n: u32) -> usize {
    let mut b = [0u8; 4];
    random(&mut b);
    (u32::from_le_bytes(b) % n) as usize
}

/// Wrap `content` in one block.
fn block(out: &mut Vec<u8>, uuid: Option<&[u8; 16]>, cmd: u8, content: &[u8], long: bool) {
    let padding = if long && content.len() < 900 {
        rand_below(500) + 900 - content.len()
    } else {
        rand_below(256)
    };
    if let Some(u) = uuid {
        out.extend_from_slice(u);
    }
    out.push(cmd);
    out.extend_from_slice(&(content.len() as u16).to_be_bytes());
    out.extend_from_slice(&(padding as u16).to_be_bytes());
    out.extend_from_slice(content);
    let start = out.len();
    out.resize(start + padding, 0);
    random(&mut out[start..]);
}

pub struct Writer {
    uuid: [u8; 16],
    first: bool,
    padding: bool,
    /// The program inside started a TLS handshake.
    inner_tls: bool,
}

impl Writer {
    pub fn new(uuid: [u8; 16]) -> Writer {
        Writer {
            uuid,
            first: true,
            padding: true,
            inner_tls: false,
        }
    }

    /// What to send for `data`.
    pub fn wrap(&mut self, data: &[u8]) -> Vec<u8> {
        if !self.padding {
            return data.to_vec();
        }
        if self.first {
            self.inner_tls = data.len() >= 6 && data[0] == 0x16 && data[1] == 3;
        }
        // the inner TLS's first application data ends the padding, and
        // so does anything that isn't TLS at all
        let app_data = data.len() >= 6 && data[..3] == [0x17, 3, 3];
        let last = !self.inner_tls || app_data;
        let mut out = Vec::with_capacity(data.len() + 1024);
        let chunks: Vec<&[u8]> = if data.is_empty() {
            alloc::vec![&[][..]]
        } else {
            data.chunks(4096).collect()
        };
        let n = chunks.len();
        for (i, chunk) in chunks.into_iter().enumerate() {
            let cmd = if last && i == n - 1 { END } else { CONTINUE };
            let uuid = if self.first { Some(&self.uuid) } else { None };
            self.first = false;
            block(&mut out, uuid, cmd, chunk, self.inner_tls && !app_data);
        }
        if last {
            self.padding = false;
        }
        out
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    /// Before the first bytes: padded or not?
    Start,
    /// Reading a block's 5-byte header.
    Header,
    Content,
    Padding,
    /// No more padding.
    Plain,
}

pub struct Reader {
    uuid: [u8; 16],
    state: State,
    head: Vec<u8>,
    cmd: u8,
    content_left: usize,
    padding_left: usize,
    /// The server said it sends the rest without the outer TLS.
    pub direct: bool,
}

impl Reader {
    pub fn new(uuid: [u8; 16]) -> Reader {
        Reader {
            uuid,
            state: State::Start,
            head: Vec::new(),
            cmd: 0,
            content_left: 0,
            padding_left: 0,
            direct: false,
        }
    }

    /// Take bytes from the server and return the program's data in them.
    /// After this sets `direct`, the caller reads the socket without TLS.
    pub fn unwrap(&mut self, mut data: &[u8], out: &mut Vec<u8>) {
        while !data.is_empty() {
            match self.state {
                State::Start => {
                    if data.len() >= 21 && data[..16] == self.uuid {
                        data = &data[16..];
                        self.state = State::Header;
                    } else {
                        self.state = State::Plain;
                    }
                }
                State::Header => {
                    let take = (5 - self.head.len()).min(data.len());
                    self.head.extend_from_slice(&data[..take]);
                    data = &data[take..];
                    if self.head.len() == 5 {
                        self.cmd = self.head[0];
                        self.content_left = u16::from_be_bytes([self.head[1], self.head[2]]) as usize;
                        self.padding_left = u16::from_be_bytes([self.head[3], self.head[4]]) as usize;
                        self.head.clear();
                        self.state = State::Content;
                        self.after_block_part();
                    }
                }
                State::Content => {
                    let take = self.content_left.min(data.len());
                    out.extend_from_slice(&data[..take]);
                    data = &data[take..];
                    self.content_left -= take;
                    self.after_block_part();
                }
                State::Padding => {
                    let take = self.padding_left.min(data.len());
                    data = &data[take..];
                    self.padding_left -= take;
                    self.after_block_part();
                }
                State::Plain => {
                    out.extend_from_slice(data);
                    return;
                }
            }
        }
    }

    fn after_block_part(&mut self) {
        if self.state == State::Content && self.content_left == 0 {
            self.state = State::Padding;
        }
        if self.state == State::Padding && self.padding_left == 0 {
            self.state = match self.cmd {
                CONTINUE => State::Header,
                DIRECT => {
                    self.direct = true;
                    State::Plain
                }
                _ => State::Plain,
            };
        }
    }
}
