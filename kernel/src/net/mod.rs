//! Networking: the e1000 driver under the smoltcp TCP/IP stack, with
//! DHCP, DNS and blocking TCP streams for the browser.
//!
//! The stack lives in one global. The desktop's main loop polls it, and
//! blocking calls (connect, read, write) poll it while they wait.

pub mod e1000;

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
use e1000::E1000;

/// Seconds to wait for anything before giving up.
const TIMEOUT_SECS: u64 = 15;
/// If DHCP says nothing for this long, use QEMU's user network settings.
const DHCP_FALLBACK_SECS: u64 = 4;

struct Stack {
    nic: E1000,
    iface: Interface,
    sockets: SocketSet<'static>,
    dhcp: SocketHandle,
    dns: SocketHandle,
    configured: bool,
    started: u64,
    next_port: u16,
}

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
        return Some(s.nic.mac);
    }
    let Some(mut nic) = E1000::init() else {
        crate::serial::write_str("\nnet: no network card\n");
        return None;
    };
    crate::serial::write_str("\nnet: e1000 found\n");
    let mac = nic.mac;
    let mut config = Config::new(HardwareAddress::Ethernet(EthernetAddress(mac)));
    config.random_seed = rdtsc();
    let iface = Interface::new(config, &mut nic, now());
    let mut sockets = SocketSet::new(Vec::new());
    let dhcp = sockets.add(dhcpv4::Socket::new());
    let dns = sockets.add(dns::Socket::new(&[], Vec::new()));
    *STACK.lock() = Some(Stack {
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

/// Whether the card has an IP address yet.
pub fn configured() -> bool {
    STACK.lock().as_ref().is_some_and(|s| s.configured)
}

/// Whether there is a network card, and whether its cable is plugged in.
pub fn link() -> Option<bool> {
    STACK.lock().as_ref().map(|s| s.nic.link_up())
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
                if !self.configured && waited > DHCP_FALLBACK_SECS * interrupts::TIMER_HZ {
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

fn parse_ipv4(s: &str) -> Option<Ipv4Address> {
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

/// A TCP connection. Reads and writes block (polling the stack) with a
/// timeout. Dropping it closes the connection.
pub struct TcpStream {
    handle: SocketHandle,
}

impl TcpStream {
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
        let stream = TcpStream { handle };
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

impl Drop for TcpStream {
    fn drop(&mut self) {
        let mut guard = STACK.lock();
        if let Some(s) = guard.as_mut() {
            s.sockets.get_mut::<tcp::Socket>(self.handle).abort();
            s.poll();
            s.sockets.remove(self.handle);
        }
    }
}

#[derive(Debug)]
pub struct IoError;

impl core::fmt::Display for IoError {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        f.write_str("network error")
    }
}

impl core::error::Error for IoError {}

impl embedded_io::Error for IoError {
    fn kind(&self) -> embedded_io::ErrorKind {
        embedded_io::ErrorKind::Other
    }
}

impl embedded_io::ErrorType for TcpStream {
    type Error = IoError;
}

impl embedded_io::Read for TcpStream {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, IoError> {
        TcpStream::read(self, buf).map_err(|_| IoError)
    }
}

impl embedded_io::Write for TcpStream {
    fn write(&mut self, buf: &[u8]) -> Result<usize, IoError> {
        self.write_all(buf).map_err(|_| IoError)?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> Result<(), IoError> {
        Ok(())
    }
}
