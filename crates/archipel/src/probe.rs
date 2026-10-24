//! Checking from the internet whether a desktop app's ports reach it (a hosted
//! archipel, `POST /api/probe`).
//!
//! A desktop behind a home router cannot tell by itself whether the forwarding the
//! router accepted works: it asks a hosted archipel, on the internet, to try. The
//! desktop listens with an echo responder on one port, TCP and UDP; the hosted
//! archipel connects to the caller's own address on that port, sends
//! `archipel-probe <nonce>` and expects it back. It never probes any other address
//! (the request comes over TCP: its source cannot be forged), only public IPv4 ones,
//! a few times per caller, a few at once.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use archipel_core::api::{HostCheck, ProbeOutcome, ProbeRequest, ProbeResponse};
use parking_lot::Mutex;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracing::info;

/// How long each protocol's probe and the hostname's lookup may take.
pub const TIMEOUT: Duration = Duration::from_secs(3);
/// How long a whole request may take.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
/// Requests a caller may make per window: a desktop makes one at launch, one per
/// renewal (20 min), and one per "Check again".
const PER_CALLER: u32 = 10;
const WINDOW: Duration = Duration::from_secs(10 * 60);
/// Probes running at once, whoever asked.
const AT_ONCE: usize = 16;

/// What the responder echoes, and the probe sends: one line.
pub fn payload(nonce: &str) -> String {
    format!("archipel-probe {nonce}\n")
}

/// Why a probe was not made.
#[derive(Debug, PartialEq)]
pub enum Refusal {
    Bad(String),
    TooMany,
    Busy,
}

/// The hosted side: limits, and the probes.
pub struct Prober {
    limiter: Limiter,
    running: Arc<tokio::sync::Semaphore>,
}

impl Default for Prober {
    fn default() -> Self {
        Prober {
            limiter: Limiter::new(PER_CALLER, WINDOW),
            running: Arc::new(tokio::sync::Semaphore::new(AT_ONCE)),
        }
    }
}

impl Prober {
    /// Probes `caller` (its address as the internet sees it) as it asks.
    pub async fn check(&self, caller: IpAddr, req: ProbeRequest) -> Result<ProbeResponse, Refusal> {
        if !self.limiter.allow(caller, Instant::now()) {
            return Err(Refusal::TooMany);
        }
        let host = check_request(&req).map_err(Refusal::Bad)?;
        let ip = match caller {
            IpAddr::V4(ip) => ip,
            IpAddr::V6(_) => {
                return Ok(ProbeResponse {
                    ip: caller.to_string(),
                    tcp: ProbeOutcome::Unsupported,
                    udp: ProbeOutcome::Unsupported,
                    detail: Some("IPv6 isn't checked yet".into()),
                    host: None,
                });
            }
        };
        if !is_public(ip) {
            return Err(Refusal::Bad(format!(
                "{ip} is not a public address: there is nothing to check from outside"
            )));
        }
        let Ok(_slot) = self.running.clone().try_acquire_owned() else {
            return Err(Refusal::Busy);
        };
        let addr = SocketAddr::new(caller, req.port);
        let probes = async {
            let ((tcp, udp), host) = tokio::join!(echo(addr, &req.nonce), async {
                match &host {
                    Some(name) => Some(resolve(name, caller).await),
                    None => None,
                }
            });
            (tcp, udp, host)
        };
        let (tcp, udp, host) = tokio::time::timeout(REQUEST_TIMEOUT, probes)
            .await
            .unwrap_or((ProbeOutcome::Silent, ProbeOutcome::Silent, None));
        info!(
            %caller,
            port = req.port,
            ?tcp,
            ?udp,
            host = ?host.as_ref().map(|h| (&h.name, h.matches)),
            "probed from outside"
        );
        Ok(ProbeResponse {
            ip: caller.to_string(),
            tcp,
            udp,
            detail: None,
            host,
        })
    }
}

/// The hostname to check, once the request is seen to be within bounds.
fn check_request(req: &ProbeRequest) -> Result<Option<String>, String> {
    if req.port < 1024 {
        return Err("the port must be between 1024 and 65535".into());
    }
    let n = &req.nonce;
    if !(16..=64).contains(&n.len()) || !n.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return Err("the nonce has 16 to 64 letters and digits".into());
    }
    match req.host.as_deref().map(str::trim) {
        None | Some("") => Ok(None),
        Some(host) => dns_name(host)
            .map(Some)
            .ok_or_else(|| format!("{host} is not a hostname")),
    }
}

/// `name`, lowercased and without its final dot, if it is a hostname: labels of
/// letters, digits and dashes, at least two, the last not all digits (an address).
pub fn dns_name(name: &str) -> Option<String> {
    let name = name.strip_suffix('.').unwrap_or(name).to_ascii_lowercase();
    let labels: Vec<&str> = name.split('.').collect();
    let label_ok = |l: &&str| {
        (1..=63).contains(&l.len())
            && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            && !l.starts_with('-')
            && !l.ends_with('-')
    };
    let ok = name.len() <= 253
        && labels.len() >= 2
        && labels.iter().all(label_ok)
        && !labels.last()?.bytes().all(|b| b.is_ascii_digit());
    ok.then_some(name)
}

/// The caller's address as the internet sees it: the connection's peer, unless that
/// is facade's entry point on this machine (loopback, or the private gateway of the
/// app's Docker network), which always overwrites `X-Real-IP` with its own peer.
/// (`X-Forwarded-For` is never read: the entry point only appends to what the
/// client sent.)
pub fn caller_ip(peer: IpAddr, real_ip: Option<&str>) -> Option<IpAddr> {
    let proxied = match peer.to_canonical() {
        IpAddr::V4(ip) => ip.is_loopback() || ip.is_private(),
        IpAddr::V6(ip) => ip.is_loopback(),
    };
    if proxied {
        real_ip?
            .trim()
            .parse::<IpAddr>()
            .ok()
            .map(|ip| ip.to_canonical())
    } else {
        Some(peer.to_canonical())
    }
}

/// Global unicast IPv4: the only addresses probed, so that the probe never reaches
/// this machine's own networks.
pub fn is_public(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    !(a == 0
        || ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || (a == 100 && (64..128).contains(&b))
        || (a == 192 && b == 0 && c == 0)
        || (a == 198 && (18..20).contains(&b))
        || ip.is_documentation()
        || ip.is_multicast()
        || a >= 240)
}

/// Requests per caller in a window that starts with their first.
struct Limiter {
    max: u32,
    window: Duration,
    seen: Mutex<HashMap<IpAddr, (Instant, u32)>>,
}

impl Limiter {
    fn new(max: u32, window: Duration) -> Self {
        Limiter {
            max,
            window,
            seen: Default::default(),
        }
    }

    fn allow(&self, ip: IpAddr, now: Instant) -> bool {
        let mut seen = self.seen.lock();
        seen.retain(|_, (since, _)| now.duration_since(*since) < self.window);
        let (_, count) = seen.entry(ip).or_insert((now, 0));
        if *count >= self.max {
            return false;
        }
        *count += 1;
        true
    }
}

/// Sends `archipel-probe <nonce>` to `addr` over TCP and UDP at once, and says
/// what came back on each.
pub async fn echo(addr: SocketAddr, nonce: &str) -> (ProbeOutcome, ProbeOutcome) {
    let want = payload(nonce);
    tokio::join!(echo_tcp(addr, &want), echo_udp(addr, &want))
}

async fn echo_tcp(addr: SocketAddr, want: &str) -> ProbeOutcome {
    let until = tokio::time::Instant::now() + TIMEOUT;
    let mut stream =
        match tokio::time::timeout_at(until, tokio::net::TcpStream::connect(addr)).await {
            Ok(Ok(stream)) => stream,
            Ok(Err(e)) if e.kind() == std::io::ErrorKind::ConnectionRefused => {
                return ProbeOutcome::Refused;
            }
            Ok(Err(_)) | Err(_) => return ProbeOutcome::Silent,
        };
    // Connected: whatever does not echo is something other than archipel.
    let exchange = async {
        stream.write_all(want.as_bytes()).await?;
        let mut got = Vec::new();
        let mut buf = [0u8; 128];
        while got.len() < want.len() && !got.contains(&b'\n') {
            let n = stream.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            got.extend_from_slice(&buf[..n]);
        }
        std::io::Result::Ok(got)
    };
    match tokio::time::timeout_at(until, exchange).await {
        Ok(Ok(got)) if got == want.as_bytes() => ProbeOutcome::Reached,
        _ => ProbeOutcome::WrongAnswer,
    }
}

async fn echo_udp(addr: SocketAddr, want: &str) -> ProbeOutcome {
    let refused = |e: &std::io::Error| e.kind() == std::io::ErrorKind::ConnectionRefused;
    let any: SocketAddr = match addr {
        SocketAddr::V4(_) => (Ipv4Addr::UNSPECIFIED, 0).into(),
        SocketAddr::V6(_) => (std::net::Ipv6Addr::UNSPECIFIED, 0).into(),
    };
    let Ok(socket) = tokio::net::UdpSocket::bind(any).await else {
        return ProbeOutcome::Silent;
    };
    if socket.connect(addr).await.is_err() {
        return ProbeOutcome::Silent;
    }
    let mut buf = [0u8; 128];
    let mut other = false;
    // Sent twice: a datagram may be lost.
    for _ in 0..2 {
        match socket.send(want.as_bytes()).await {
            Err(e) if refused(&e) => return ProbeOutcome::Refused,
            _ => {}
        }
        let until = tokio::time::Instant::now() + TIMEOUT / 2;
        loop {
            match tokio::time::timeout_at(until, socket.recv(&mut buf)).await {
                Err(_) => break,
                Ok(Err(e)) if refused(&e) => return ProbeOutcome::Refused,
                Ok(Err(_)) => break,
                Ok(Ok(n)) if &buf[..n] == want.as_bytes() => return ProbeOutcome::Reached,
                Ok(Ok(_)) => other = true,
            }
        }
    }
    if other {
        ProbeOutcome::WrongAnswer
    } else {
        ProbeOutcome::Silent
    }
}

/// Where `name` leads, as this machine's resolver (the internet's DNS) says.
async fn resolve(name: &str, caller: IpAddr) -> HostCheck {
    let found = tokio::time::timeout(TIMEOUT, tokio::net::lookup_host((name, 0))).await;
    let mut addresses: Vec<IpAddr> = match found {
        Ok(Ok(addrs)) => addrs.map(|a| a.ip()).filter(IpAddr::is_ipv4).collect(),
        _ => Vec::new(),
    };
    addresses.sort();
    addresses.dedup();
    HostCheck {
        name: name.to_string(),
        matches: addresses.contains(&caller),
        addresses: addresses.iter().map(IpAddr::to_string).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A responder like the desktop's on 127.0.0.1: echoes `nonce`'s line on TCP
    /// (unless `tcp` is off) and UDP (unless `udp` is off), says `other` otherwise.
    async fn responder(nonce: &str, tcp: bool, udp: bool) -> SocketAddr {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let socket = tokio::net::UdpSocket::bind(addr).await.unwrap();
        let want = payload(nonce);
        let line = want.clone();
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let mut buf = [0u8; 128];
                let n = stream.read(&mut buf).await.unwrap_or(0);
                let answer = if tcp && &buf[..n] == line.as_bytes() {
                    line.clone()
                } else {
                    "other\n".into()
                };
                let _ = stream.write_all(answer.as_bytes()).await;
            }
        });
        tokio::spawn(async move {
            let mut buf = [0u8; 128];
            while let Ok((n, from)) = socket.recv_from(&mut buf).await {
                if udp && &buf[..n] == want.as_bytes() {
                    let _ = socket.send_to(&buf[..n], from).await;
                }
            }
        });
        addr
    }

    #[tokio::test]
    async fn echoes_the_nonce_and_nothing_else() {
        let nonce = "0123456789abcdef";
        let addr = responder(nonce, true, true).await;
        assert_eq!(
            echo(addr, nonce).await,
            (ProbeOutcome::Reached, ProbeOutcome::Reached)
        );
        // Another nonce: the TCP side answers something else, UDP nothing.
        assert_eq!(
            echo(addr, "fedcba9876543210").await,
            (ProbeOutcome::WrongAnswer, ProbeOutcome::Silent)
        );
        let addr = responder(nonce, true, false).await;
        assert_eq!(
            echo(addr, nonce).await,
            (ProbeOutcome::Reached, ProbeOutcome::Silent)
        );
    }

    #[tokio::test]
    async fn tells_a_closed_port() {
        // Bound, then closed: nothing listens there any more.
        let addr = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap();
        let (tcp, udp) = echo(addr, "0123456789abcdef").await;
        assert_eq!(tcp, ProbeOutcome::Refused);
        // Loopback answers ICMP "port unreachable", which only Linux hands back.
        assert!(matches!(udp, ProbeOutcome::Refused | ProbeOutcome::Silent));
    }

    #[test]
    fn probes_only_public_addresses() {
        let public = |s: &str| is_public(s.parse().unwrap());
        assert!(public("86.245.1.2"));
        assert!(public("100.128.0.1"));
        for private in [
            "127.0.0.1",
            "10.1.2.3",
            "172.17.0.1",
            "192.168.1.20",
            "100.64.0.1",
            "169.254.1.1",
            "0.1.2.3",
            "224.0.0.1",
            "255.255.255.255",
            "240.0.0.1",
            "192.0.2.1",
            "198.18.0.1",
            "203.0.113.5",
        ] {
            assert!(!public(private), "{private}");
        }
    }

    #[test]
    fn trusts_the_real_ip_header_only_from_facade() {
        let forged = Some("1.2.3.4");
        let ip = |s: &str| s.parse::<IpAddr>().unwrap();
        // Through facade's entry point: loopback, or the Docker network's gateway.
        assert_eq!(caller_ip(ip("127.0.0.1"), forged), Some(ip("1.2.3.4")));
        assert_eq!(caller_ip(ip("172.18.0.1"), forged), Some(ip("1.2.3.4")));
        assert_eq!(
            caller_ip(ip("::ffff:172.18.0.1"), forged),
            Some(ip("1.2.3.4"))
        );
        assert_eq!(caller_ip(ip("127.0.0.1"), None), None);
        // Straight from the internet: the header is the caller's to forge.
        assert_eq!(caller_ip(ip("86.245.1.2"), forged), Some(ip("86.245.1.2")));
        assert_eq!(
            caller_ip(ip("::ffff:86.245.1.2"), None),
            Some(ip("86.245.1.2"))
        );
    }

    #[test]
    fn limits_each_caller() {
        let limiter = Limiter::new(3, Duration::from_secs(600));
        let (a, b) = (IpAddr::from([86, 245, 1, 2]), IpAddr::from([86, 245, 1, 3]));
        let start = Instant::now();
        for _ in 0..3 {
            assert!(limiter.allow(a, start));
        }
        assert!(!limiter.allow(a, start + Duration::from_secs(1)));
        assert!(limiter.allow(b, start + Duration::from_secs(1)));
        assert!(limiter.allow(a, start + Duration::from_secs(601)));
    }

    #[test]
    fn keeps_requests_within_bounds() {
        let req = |port, nonce: &str, host: Option<&str>| ProbeRequest {
            port,
            nonce: nonce.into(),
            host: host.map(Into::into),
        };
        let nonce = "0123456789abcdef";
        assert_eq!(check_request(&req(30000, nonce, None)), Ok(None));
        assert_eq!(
            check_request(&req(30000, nonce, Some("Sigma-Dev.ddns.net."))),
            Ok(Some("sigma-dev.ddns.net".into()))
        );
        assert!(check_request(&req(80, nonce, None)).is_err());
        assert!(check_request(&req(30000, "short", None)).is_err());
        assert!(check_request(&req(30000, "0123456789abcdef\n", None)).is_err());
        assert!(check_request(&req(30000, &"a".repeat(65), None)).is_err());
        for bad in [
            "localhost",
            "1.2.3.4",
            "-a.example.com",
            "a..b",
            "a b.com",
            "x.com/y",
        ] {
            assert!(
                check_request(&req(30000, nonce, Some(bad))).is_err(),
                "{bad}"
            );
        }
    }

    #[tokio::test]
    async fn refuses_private_callers_and_says_ipv6_is_not_checked() {
        let prober = Prober::default();
        let req = ProbeRequest {
            port: 30000,
            nonce: "0123456789abcdef".into(),
            host: None,
        };
        assert!(matches!(
            prober
                .check("192.168.1.20".parse().unwrap(), req.clone())
                .await,
            Err(Refusal::Bad(_))
        ));
        let v6 = prober
            .check("2a01:e0a::1".parse().unwrap(), req)
            .await
            .unwrap();
        assert_eq!(
            (v6.tcp, v6.udp),
            (ProbeOutcome::Unsupported, ProbeOutcome::Unsupported)
        );
    }
}
