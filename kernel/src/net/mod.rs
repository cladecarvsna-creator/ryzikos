//! Networking: wired network card drivers (Intel e1000 and e1000e,
//! Realtek RTL8111/8168 and RTL8139) under the smoltcp TCP/IP stack, with
//! DHCP, DNS and blocking TCP streams for the browser.
//!
//! The stack lives in one global. The desktop's main loop polls it, and
//! blocking calls (connect, read, write) poll it while they wait.

pub mod e1000;
pub mod realtek;
pub mod tls;

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use smoltcp::iface::{Config, Interface, SocketHandle, SocketSet};
use smoltcp::socket::{dhcpv4, dns, tcp};
use smoltcp::time::Instant;
use smoltcp::wire::{
    DnsQueryType, EthernetAddress, HardwareAddress, IpAddress, IpCidr, Ipv4Address, Ipv4Cidr,
};

use crate::interrupts;
use crate::sync::IrqMutex;
use alloc::boxed::Box;
use smoltcp::phy::{self, Checksum, ChecksumCapabilities, DeviceCapabilities, Medium};

/// Seconds to wait for anything before giving up.
const TIMEOUT_SECS: u64 = 15;
/// If DHCP says nothing for this long, use QEMU's user network settings.
const DHCP_FALLBACK_SECS: u64 = 4;

/// What the stack needs from a network card driver.
pub trait Card: Send {
    fn name(&self) -> &'static str;
    fn mac(&self) -> [u8; 6];
    /// Whether a cable is plugged in and the link is up.
    fn link_up(&self) -> bool;
    /// The next received frame without its CRC; an empty one was dropped.
    fn receive(&mut self) -> Option<Vec<u8>>;
    fn can_send(&self) -> bool;
    /// Send one Ethernet frame (without CRC). Only after `can_send`.
    fn send(&mut self, frame: &[u8]);
}

/// Wait a few milliseconds, for cards that need time after a reset.
pub fn delay_ms(ms: u64) {
    let end = interrupts::ticks() + (ms * interrupts::TIMER_HZ).div_ceil(1000) + 1;
    while interrupts::ticks() < end {
        core::hint::spin_loop();
    }
}

/// Whether there is a driver for this card.
pub fn drives(vendor: u16, device: u16) -> bool {
    e1000::drives(vendor, device) || realtek::drives(vendor, device)
}

/// Start the first network card there is a driver for.
fn find_card() -> Option<Box<dyn Card>> {
    if let Some(c) = e1000::E1000::init() {
        return Some(Box::new(c));
    }
    if let Some(c) = realtek::Rtl8169::init() {
        return Some(Box::new(c));
    }
    if let Some(c) = realtek::Rtl8139::init() {
        return Some(Box::new(c));
    }
    None
}

/// The largest Ethernet frame, without CRC.
const MTU: usize = 1514;

struct Nic(Box<dyn Card>);

pub struct RxToken(Vec<u8>);
pub struct TxToken<'a>(&'a mut Nic);

impl phy::RxToken for RxToken {
    fn consume<R, F: FnOnce(&[u8]) -> R>(self, f: F) -> R {
        f(&self.0)
    }
}

impl phy::TxToken for TxToken<'_> {
    fn consume<R, F: FnOnce(&mut [u8]) -> R>(self, len: usize, f: F) -> R {
        let mut frame = vec![0u8; len];
        let result = f(&mut frame);
        self.0 .0.send(&frame);
        result
    }
}

impl phy::Device for Nic {
    type RxToken<'a> = RxToken;
    type TxToken<'a> = TxToken<'a>;

    fn receive(&mut self, _now: Instant) -> Option<(RxToken, TxToken<'_>)> {
        loop {
            let packet = self.0.receive()?;
            if !packet.is_empty() {
                return Some((RxToken(packet), TxToken(self)));
            }
        }
    }

    fn transmit(&mut self, _now: Instant) -> Option<TxToken<'_>> {
        self.0.can_send().then_some(TxToken(self))
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut caps = DeviceCapabilities::default();
        caps.medium = Medium::Ethernet;
        caps.max_transmission_unit = MTU;
        caps.max_burst_size = Some(16);
        let mut checksum = ChecksumCapabilities::default();
        checksum.ipv4 = Checksum::Both;
        caps.checksum = checksum;
        caps
    }
}

struct Stack {
    nic: Nic,
    /// The link was up at the last look.
    link: bool,
    /// When to read the card's link state again, in timer ticks. Each
    /// read is a trip to the card (slow in an emulator), and polls come
    /// many times a second.
    next_link_check: u64,
    /// QEMU's network card (its MACs start 52:54:00), so QEMU's user
    /// network settings are a fair guess when DHCP is slow.
    emulated: bool,
    iface: Interface,
    sockets: SocketSet<'static>,
    dhcp: SocketHandle,
    dns: SocketHandle,
    configured: bool,
    started: u64,
    next_port: u16,
}

/// How often `poll` reads whether the cable is in: 4 times a second.
const LINK_CHECK_TICKS: u64 = interrupts::TIMER_HZ / 4;

static STACK: IrqMutex<Option<Stack>> = IrqMutex::new(None);

pub fn now() -> Instant {
    Instant::from_millis((interrupts::ticks() * 1000 / interrupts::TIMER_HZ) as i64)
}

fn rdtsc() -> u64 {
    unsafe { core::arch::x86_64::_rdtsc() }
}

/// Find the network card and start DHCP, once. Returns the MAC address.
pub fn init() -> Option<[u8; 6]> {
    if let Some(s) = STACK.lock().as_ref() {
        return Some(s.nic.0.mac());
    }
    let Some(card) = find_card() else {
        crate::serial::write_str("\nnet: no network card\n");
        return None;
    };
    let line = alloc::format!("\nnet: {} found\n", card.name());
    crate::serial::write_str(&line);
    let mac = card.mac();
    let mut nic = Nic(card);
    let mut config = Config::new(HardwareAddress::Ethernet(EthernetAddress(mac)));
    config.random_seed = rdtsc();
    let iface = Interface::new(config, &mut nic, now());
    let mut sockets = SocketSet::new(Vec::new());
    let dhcp = sockets.add(dhcpv4::Socket::new());
    let dns = sockets.add(dns::Socket::new(&[], Vec::new()));
    *STACK.lock() = Some(Stack {
        link: nic.0.link_up(),
        next_link_check: 0,
        emulated: mac[..3] == [0x52, 0x54, 0x00],
        nic,
        iface,
        sockets,
        dhcp,
        dns,
        configured: false,
        started: interrupts::ticks(),
        next_port: 49152 + (rdtsc() % 8192) as u16,
    });
    Some(mac)
}

/// The network card's name, like "Intel e1000e".
pub fn card_name() -> Option<&'static str> {
    STACK.lock().as_ref().map(|s| s.nic.0.name())
}

/// What to tell the user when there is no network card we can drive.
pub const NO_CARD: &str =
    "No supported wired network card. RyzikOS drives Intel e1000/e1000e and Realtek RTL8111/8168/8139.";

/// Whether the card has an IP address yet.
pub fn configured() -> bool {
    STACK.lock().as_ref().is_some_and(|s| s.configured)
}

/// Whether there is a network card, and whether its cable is plugged in.
pub fn link() -> Option<bool> {
    STACK.lock().as_ref().map(|s| s.link)
}

/// Our IP address as text, for the status bar.
pub fn address() -> Option<String> {
    let guard = STACK.lock();
    let s = guard.as_ref()?;
    s.iface
        .ipv4_addr()
        .map(|a| alloc::format!("{}", a))
        .filter(|_| s.configured)
}

/// Move packets between the card and the sockets. Cheap if idle.
pub fn poll() {
    let mut guard = STACK.lock();
    let Some(s) = guard.as_mut() else {
        return;
    };
    s.poll();
}

impl Stack {
    fn poll(&mut self) {
        // a real card takes a few seconds to agree on a speed with the
        // switch; ask DHCP again as soon as the cable works
        if interrupts::ticks() >= self.next_link_check {
            self.next_link_check = interrupts::ticks() + LINK_CHECK_TICKS;
            let link = self.nic.0.link_up();
            if link && !self.link {
                self.sockets.get_mut::<dhcpv4::Socket>(self.dhcp).reset();
                self.started = interrupts::ticks();
            }
            self.link = link;
        }
        self.iface.poll(now(), &mut self.nic, &mut self.sockets);
        let event = self.sockets.get_mut::<dhcpv4::Socket>(self.dhcp).poll();
        match event {
            Some(dhcpv4::Event::Configured(config)) => {
                let dns: Vec<IpAddress> = config.dns_servers.iter().map(|&a| a.into()).collect();
                let router = config.router;
                let address = config.address;
                self.apply(address, router, &dns);
            }
            Some(dhcpv4::Event::Deconfigured) => {
                self.configured = false;
                self.iface.update_ip_addrs(|addrs| addrs.clear());
                self.iface.routes_mut().remove_default_ipv4_route();
            }
            None => {
                let waited = interrupts::ticks() - self.started;
                let slow = waited > DHCP_FALLBACK_SECS * interrupts::TIMER_HZ;
                if !self.configured && self.emulated && slow {
                    // QEMU's user network: 10.0.2.15, gateway .2, DNS .3
                    self.apply(
                        Ipv4Cidr::new(Ipv4Address::new(10, 0, 2, 15), 24),
                        Some(Ipv4Address::new(10, 0, 2, 2)),
                        &[Ipv4Address::new(10, 0, 2, 3).into()],
                    );
                }
            }
        }
    }

    fn apply(&mut self, address: Ipv4Cidr, router: Option<Ipv4Address>, dns: &[IpAddress]) {
        self.iface.update_ip_addrs(|addrs| {
            addrs.clear();
            let _ = addrs.push(IpCidr::Ipv4(address));
        });
        if let Some(router) = router {
            let _ = self.iface.routes_mut().add_default_ipv4_route(router);
        }
        self.sockets
            .get_mut::<dns::Socket>(self.dns)
            .update_servers(dns);
        if !self.configured {
            crate::serial::write_str("\nnet: configured ");
            let mut text = crate::StackString::<32>::new();
            let _ = core::fmt::write(&mut text, format_args!("{}", address));
            crate::serial::write_str(text.as_str());
            crate::serial::write_str("\n");
        }
        self.configured = true;
    }
}

/// Run `f` on the stack after polling it, until it returns Some, the
/// deadline passes, or the fiber doing this is cancelled.
fn wait<T>(
    what: &str,
    mut f: impl FnMut(&mut Stack) -> Option<Result<T, String>>,
) -> Result<T, String> {
    let deadline = interrupts::ticks() + TIMEOUT_SECS * interrupts::TIMER_HZ;
    loop {
        {
            let mut guard = STACK.lock();
            let s = guard.as_mut().ok_or("no network card")?;
            s.poll();
            if let Some(result) = f(s) {
                return result;
            }
        }
        if interrupts::ticks() > deadline {
            return Err(alloc::format!("timed out: {}", what));
        }
        if crate::fiber::cancelled() {
            return Err(String::from("cancelled"));
        }
        if crate::fiber::inside() {
            // a background download: let the desktop run meanwhile
            crate::fiber::pause();
        } else {
            // let the card and QEMU catch up; interrupts wake us
            for _ in 0..2000 {
                core::hint::spin_loop();
            }
        }
    }
}

/// Wait until DHCP (or the fallback) has given us an address.
pub fn wait_configured() -> Result<(), String> {
    wait("no IP address", |s| s.configured.then_some(Ok(())))
}

/// Look up the IPv4 address of `host`.
pub fn resolve(host: &str) -> Result<Ipv4Address, String> {
    if let Some(ip) = parse_ipv4(host) {
        return Ok(ip);
    }
    let now = interrupts::ticks();
    if let Some(&(ip, until)) = DNS_CACHE.lock().get(host) {
        if now < until {
            return Ok(ip);
        }
    }
    let ip = lookup(host)?;
    let mut cache = DNS_CACHE.lock();
    if cache.len() > 256 {
        cache.clear();
    }
    cache.insert(
        String::from(host),
        (ip, now + DNS_CACHE_SECS * interrupts::TIMER_HZ),
    );
    Ok(ip)
}

/// Addresses looked up recently, with when to forget them.
static DNS_CACHE: IrqMutex<BTreeMap<String, (Ipv4Address, u64)>> = IrqMutex::new(BTreeMap::new());
const DNS_CACHE_SECS: u64 = 300;

fn lookup(host: &str) -> Result<Ipv4Address, String> {
    wait_configured()?;
    let query = {
        let mut guard = STACK.lock();
        let s = guard.as_mut().ok_or("no network card")?;
        let cx = s.iface.context();
        s.sockets
            .get_mut::<dns::Socket>(s.dns)
            .start_query(cx, host, DnsQueryType::A)
            .map_err(|e| alloc::format!("bad host name: {:?}", e))?
    };
    wait("DNS lookup", |s| {
        match s
            .sockets
            .get_mut::<dns::Socket>(s.dns)
            .get_query_result(query)
        {
            Ok(addrs) => Some(
                addrs
                    .iter()
                    .find_map(|a| match a {
                        IpAddress::Ipv4(v4) => Some(*v4),
                        #[allow(unreachable_patterns)]
                        _ => None,
                    })
                    .ok_or_else(|| alloc::format!("{}: no IPv4 address", host)),
            ),
            Err(dns::GetQueryResultError::Pending) => None,
            Err(_) => Some(Err(alloc::format!("{}: host not found", host))),
        }
    })
}

pub fn parse_ipv4(s: &str) -> Option<Ipv4Address> {
    let mut parts = [0u8; 4];
    let mut n = 0;
    for part in s.split('.') {
        if n == 4 {
            return None;
        }
        parts[n] = part.parse().ok()?;
        n += 1;
    }
    (n == 4).then(|| Ipv4Address::new(parts[0], parts[1], parts[2], parts[3]))
}

/// A TCP connection on the network card itself. Reads and writes block
/// (polling the stack) with a timeout. Dropping it closes the connection.
/// Programs use [`TcpStream`], which goes through the VPN when it is on.
pub struct Socket {
    handle: SocketHandle,
}

impl Socket {
    pub fn connect(ip: Ipv4Address, port: u16) -> Result<Self, String> {
        wait_configured()?;
        let handle = {
            let mut guard = STACK.lock();
            let s = guard.as_mut().ok_or("no network card")?;
            let socket = tcp::Socket::new(
                tcp::SocketBuffer::new(vec![0; 256 * 1024]),
                tcp::SocketBuffer::new(vec![0; 32 * 1024]),
            );
            let handle = s.sockets.add(socket);
            let local = s.next_port;
            s.next_port = if s.next_port >= 65000 {
                49152
            } else {
                s.next_port + 1
            };
            let cx = s.iface.context();
            let socket = s.sockets.get_mut::<tcp::Socket>(handle);
            socket.set_nagle_enabled(false);
            if let Err(e) = socket.connect(cx, (IpAddress::Ipv4(ip), port), local) {
                s.sockets.remove(handle);
                return Err(alloc::format!("connect: {:?}", e));
            }
            handle
        };
        let stream = Socket { handle };
        wait("connecting", |s| {
            let socket = s.sockets.get_mut::<tcp::Socket>(handle);
            if socket.may_send() {
                Some(Ok(()))
            } else if !socket.is_open() {
                Some(Err(String::from("connection refused")))
            } else {
                None
            }
        })?;
        Ok(stream)
    }

    /// Read some bytes; 0 means the other side closed the connection.
    pub fn read(&mut self, buf: &mut [u8]) -> Result<usize, String> {
        let handle = self.handle;
        wait("reading", |s| {
            let socket = s.sockets.get_mut::<tcp::Socket>(handle);
            if socket.can_recv() {
                Some(
                    socket
                        .recv_slice(buf)
                        .map_err(|e| alloc::format!("{:?}", e)),
                )
            } else if !socket.may_recv() {
                Some(Ok(0))
            } else {
                None
            }
        })
    }

    /// Read what has arrived without waiting: `Ok(None)` if nothing has
    /// yet, `Ok(Some(0))` if the other side closed the connection.
    pub fn try_read(&mut self, buf: &mut [u8]) -> Result<Option<usize>, String> {
        let mut guard = STACK.lock();
        let s = guard.as_mut().ok_or("no network card")?;
        s.poll();
        let socket = s.sockets.get_mut::<tcp::Socket>(self.handle);
        if socket.can_recv() {
            socket
                .recv_slice(buf)
                .map(Some)
                .map_err(|e| alloc::format!("{:?}", e))
        } else if !socket.may_recv() {
            Ok(Some(0))
        } else {
            Ok(None)
        }
    }

    pub fn write_all(&mut self, mut data: &[u8]) -> Result<(), String> {
        let handle = self.handle;
        while !data.is_empty() {
            let n = wait("sending", |s| {
                let socket = s.sockets.get_mut::<tcp::Socket>(handle);
                if !socket.may_send() {
                    Some(Err(String::from("connection closed")))
                } else if socket.can_send() {
                    Some(
                        socket
                            .send_slice(data)
                            .map_err(|e| alloc::format!("{:?}", e)),
                    )
                } else {
                    None
                }
            })?;
            data = &data[n..];
        }
        Ok(())
    }
}

impl Drop for Socket {
    fn drop(&mut self) {
        let mut guard = STACK.lock();
        if let Some(s) = guard.as_mut() {
            s.sockets.get_mut::<tcp::Socket>(self.handle).abort();
            s.poll();
            s.sockets.remove(self.handle);
        }
    }
}

/// Where a connection goes: an address, or a name the VPN server looks up.
#[derive(Clone)]
pub enum Host {
    Ip(Ipv4Address),
    Name(String),
}

/// A TCP connection for programs: straight from the network card, or
/// through the VPN server while the VPN is on. Dropping it closes it.
pub struct TcpStream(Conn);

enum Conn {
    Direct(Socket),
    Vpn(Box<crate::vpn::Tunnel>),
}

impl TcpStream {
    pub fn connect(ip: Ipv4Address, port: u16) -> Result<Self, String> {
        Self::open(Host::Ip(ip), port)
    }

    /// Connect to a host by name. With the VPN on, the VPN server looks
    /// the name up, so nothing about it goes out on this network.
    pub fn connect_host(host: &str, port: u16) -> Result<Self, String> {
        match parse_ipv4(host) {
            Some(ip) => Self::open(Host::Ip(ip), port),
            None => Self::open(Host::Name(String::from(host)), port),
        }
    }

    fn open(host: Host, port: u16) -> Result<Self, String> {
        if let Some(server) = crate::vpn::route() {
            let tunnel = crate::vpn::Tunnel::open(&server, &host, port)?;
            return Ok(TcpStream(Conn::Vpn(Box::new(tunnel))));
        }
        let ip = match host {
            Host::Ip(ip) => ip,
            Host::Name(name) => resolve(&name)?,
        };
        Ok(TcpStream(Conn::Direct(Socket::connect(ip, port)?)))
    }

    /// Read some bytes; 0 means the other side closed the connection.
    pub fn read(&mut self, buf: &mut [u8]) -> Result<usize, String> {
        match &mut self.0 {
            Conn::Direct(s) => s.read(buf),
            Conn::Vpn(t) => t.read(buf),
        }
    }

    /// Read what has arrived without waiting: `Ok(None)` if nothing has
    /// yet, `Ok(Some(0))` if the other side closed the connection.
    pub fn try_read(&mut self, buf: &mut [u8]) -> Result<Option<usize>, String> {
        match &mut self.0 {
            Conn::Direct(s) => s.try_read(buf),
            Conn::Vpn(t) => t.try_read(buf),
        }
    }

    pub fn write_all(&mut self, data: &[u8]) -> Result<(), String> {
        match &mut self.0 {
            Conn::Direct(s) => s.write_all(data),
            Conn::Vpn(t) => t.write_all(data),
        }
    }
}
