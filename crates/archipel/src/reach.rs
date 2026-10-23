//! The desktop app's ports: opened on the home router while a server runs, and
//! whether friends outside can reach it.
//!
//! A computer at home sits behind its router's NAT: the router must be told to
//! forward a server's ports to it. archipel asks, as each server starts, with
//! NAT-PMP (Apple routers and others) and then UPnP (most home routers), renews the
//! forwarding while the server runs and withdraws it when it stops. It then tells
//! which of three cases it is in: friends can join; only the home network can (the
//! router did not do it); or no one outside can, whatever the router does, because
//! the connection has no public address of its own (carrier-grade NAT, or a second
//! router in front). The server runs in every case.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, anyhow, bail};
use archipel_core::api::ReachState;
use archipel_core::game::Protocol;
use igd_next::PortMappingProtocol;
use igd_next::aio::Gateway;
use igd_next::aio::tokio::Tokio;
use parking_lot::Mutex;
use tracing::{debug, info};

/// Forwardings last this long on the router, renewed well before.
const LEASE: Duration = Duration::from_secs(3600);
const RENEW_EVERY: Duration = Duration::from_secs(20 * 60);
/// The public address is asked for again after this long.
const PUBLIC_FOR: Duration = Duration::from_secs(10 * 60);
/// Asked for the address the internet sees this computer at.
const WHAT_IS_MY_IP: &str = "https://api.ipify.org";

/// What archipel knows of a server's reachability.
#[derive(Debug, Clone, PartialEq)]
pub struct Status {
    pub state: ReachState,
    /// This computer's address on the home network.
    pub local_ip: Option<Ipv4Addr>,
    /// Where the internet reaches it, when it does.
    pub public_ip: Option<IpAddr>,
    pub detail: Option<String>,
}

/// How a server's ports were forwarded, to renew and withdraw them the same way.
#[derive(Clone)]
enum Via {
    NatPmp(Ipv4Addr),
    Upnp(Gateway<Tokio>),
}

struct Entry {
    status: Status,
    ports: Vec<u16>,
    via: Option<Via>,
    local_ip: Option<Ipv4Addr>,
}

pub struct Reacher {
    http: reqwest::Client,
    servers: Mutex<HashMap<u64, Entry>>,
    public: tokio::sync::Mutex<Option<(Instant, Option<IpAddr>)>>,
}

impl Reacher {
    pub fn new() -> anyhow::Result<Arc<Self>> {
        Ok(Arc::new(Reacher {
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(5))
                .build()?,
            servers: Default::default(),
            public: Default::default(),
        }))
    }

    pub fn status(&self, server: u64) -> Option<Status> {
        self.servers.lock().get(&server).map(|e| e.status.clone())
    }

    /// Opens a starting server's ports (TCP and UDP each: games use either or
    /// both) and works out who can reach it.
    pub async fn open(&self, server: u64, ports: Vec<u16>) {
        let local_ip = local_ip();
        self.servers.lock().insert(
            server,
            Entry {
                status: Status {
                    state: ReachState::Checking,
                    local_ip,
                    public_ip: None,
                    detail: None,
                },
                ports: ports.clone(),
                via: None,
                local_ip,
            },
        );
        let public_ip = self.public_ip().await;
        let (status, via) = match local_ip {
            None => (
                Status {
                    state: ReachState::LocalOnly,
                    local_ip,
                    public_ip,
                    detail: Some("this computer is not on a network".into()),
                },
                None,
            ),
            // Its own address is the public one (no router in between): nothing to do.
            Some(ip) if public_ip == Some(IpAddr::V4(ip)) => (
                Status {
                    state: ReachState::Internet,
                    local_ip,
                    public_ip,
                    detail: None,
                },
                None,
            ),
            Some(ip) => forward(ip, &ports, public_ip).await,
        };
        info!(server, ?status.state, "reachability");
        if let Some(entry) = self.servers.lock().get_mut(&server) {
            entry.status = status;
            entry.via = via;
        } else if let Some(via) = via {
            // Stopped meanwhile: take the forwarding back.
            tokio::spawn(async move { withdraw(&via, &ports).await });
        }
    }

    /// Takes a stopped server's ports back.
    pub async fn close(&self, server: u64) {
        let entry = self.servers.lock().remove(&server);
        if let Some(Entry {
            via: Some(via),
            ports,
            ..
        }) = entry
        {
            withdraw(&via, &ports).await;
        }
    }

    pub async fn close_all(&self) {
        let ids: Vec<u64> = self.servers.lock().keys().copied().collect();
        for id in ids {
            self.close(id).await;
        }
    }

    /// Renews forwardings before the router forgets them.
    pub async fn renew(self: Arc<Self>) {
        loop {
            tokio::time::sleep(RENEW_EVERY).await;
            let open: Vec<(Via, Vec<u16>, Ipv4Addr)> = self
                .servers
                .lock()
                .values()
                .filter_map(|e| Some((e.via.clone()?, e.ports.clone(), e.local_ip?)))
                .collect();
            for (via, ports, ip) in open {
                if let Err(e) = map_all(&via, ip, &ports).await {
                    debug!("renewing a forwarding: {e:#}");
                }
            }
        }
    }

    /// The address the internet sees this computer at, if the internet answers.
    async fn public_ip(&self) -> Option<IpAddr> {
        let mut cached = self.public.lock().await;
        if let Some((at, ip)) = *cached
            && at.elapsed() < PUBLIC_FOR
        {
            return ip;
        }
        let ip = async {
            let text = self.http.get(WHAT_IS_MY_IP).send().await?.text().await?;
            anyhow::Ok(text.trim().parse::<IpAddr>()?)
        }
        .await
        .map_err(|e| debug!("asking for the public address: {e:#}"))
        .ok();
        *cached = Some((Instant::now(), ip));
        ip
    }
}

/// Asks the router to forward `ports` to `ip`: NAT-PMP, then UPnP.
async fn forward(ip: Ipv4Addr, ports: &[u16], public_ip: Option<IpAddr>) -> (Status, Option<Via>) {
    let mut problems = Vec::new();
    let mut router_ip = None;
    for attempt in [Attempt::NatPmp, Attempt::Upnp] {
        let tried = match attempt {
            Attempt::NatPmp => match gateway() {
                Some(gw) => natpmp_forward(gw, ip, ports).await,
                None => Err(anyhow!("no default route").into()),
            },
            Attempt::Upnp => upnp_forward(ip, ports).await,
        };
        match tried {
            Ok((via, external)) => return (judge(true, external, ip, public_ip, None), Some(via)),
            Err(Failure { external, error }) => {
                router_ip = router_ip.or(external);
                problems.push(format!("{}: {error:#}", attempt.name()));
            }
        }
    }
    let detail = if problems.iter().all(|p| p.contains("no answer")) {
        "the router answered neither UPnP nor NAT-PMP: UPnP is probably turned off".to_string()
    } else {
        problems.join("; ")
    };
    (judge(false, router_ip, ip, public_ip, Some(detail)), None)
}

/// The verdict, from what the router did and the addresses seen.
fn judge(
    forwarded: bool,
    router_ip: Option<IpAddr>,
    local_ip: Ipv4Addr,
    public_ip: Option<IpAddr>,
    detail: Option<String>,
) -> Status {
    let shared = |ip: &IpAddr| match ip {
        IpAddr::V4(v4) => is_shared(*v4),
        IpAddr::V6(_) => false,
    };
    // The router's own address is not the internet's: someone else's NAT is in
    // front, and forwarding on this router cannot help.
    let behind = match (router_ip, public_ip) {
        (Some(r), _) if shared(&r) => Some(format!(
            "the router's own address, {r}, is shared with other customers"
        )),
        (Some(r), Some(p)) if r != p => Some(format!(
            "the router's address is {r} but the internet sees {p}: another router or the \
             provider's sits in front"
        )),
        _ => None,
    };
    let base = |state, public_ip, detail| Status {
        state,
        local_ip: Some(local_ip),
        public_ip,
        detail,
    };
    match (forwarded, behind) {
        (_, Some(why)) => base(ReachState::NoPublicAddress, None, Some(why)),
        (true, None) => base(ReachState::Internet, public_ip.or(router_ip), None),
        (false, None) => base(ReachState::LocalOnly, None, detail),
    }
}

/// Carrier-grade NAT (100.64.0.0/10) and private ranges: not reachable from the
/// internet.
fn is_shared(ip: Ipv4Addr) -> bool {
    let [a, b, ..] = ip.octets();
    ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || (a == 100 && (64..128).contains(&b))
}

#[derive(Clone, Copy)]
enum Attempt {
    NatPmp,
    Upnp,
}

impl Attempt {
    fn name(self) -> &'static str {
        match self {
            Attempt::NatPmp => "NAT-PMP",
            Attempt::Upnp => "UPnP",
        }
    }
}

struct Failure {
    /// The router's external address, if it told it before failing.
    external: Option<IpAddr>,
    error: anyhow::Error,
}

impl From<anyhow::Error> for Failure {
    fn from(error: anyhow::Error) -> Self {
        Failure {
            external: None,
            error,
        }
    }
}

type Forwarded = Result<(Via, Option<IpAddr>), Failure>;

async fn natpmp_forward(gateway: Ipv4Addr, ip: Ipv4Addr, ports: &[u16]) -> Forwarded {
    let external = natpmp_external(gateway).await?;
    let via = Via::NatPmp(gateway);
    if let Err(error) = map_all(&via, ip, ports).await {
        return Err(Failure {
            external: Some(IpAddr::V4(external)),
            error,
        });
    }
    Ok((via, Some(IpAddr::V4(external))))
}

async fn upnp_forward(ip: Ipv4Addr, ports: &[u16]) -> Forwarded {
    let mut options = igd_next::SearchOptions::default();
    options.timeout = Some(Duration::from_secs(4));
    options.single_search_timeout = Some(Duration::from_secs(2));
    let gateway = igd_next::aio::tokio::search_gateway(options)
        .await
        .map_err(|e| anyhow!("no answer ({e})"))?;
    let external = gateway.get_external_ip().await.ok();
    let via = Via::Upnp(gateway);
    if let Err(error) = map_all(&via, ip, ports).await {
        return Err(Failure { external, error });
    }
    Ok((via, external))
}

/// Forwards every port, TCP and UDP, to `ip`.
async fn map_all(via: &Via, ip: Ipv4Addr, ports: &[u16]) -> anyhow::Result<()> {
    for &port in ports {
        for protocol in [Protocol::Udp, Protocol::Tcp] {
            map(via, ip, port, protocol, LEASE).await.with_context(|| {
                format!(
                    "the router refused to forward {} {port}",
                    proto_name(protocol)
                )
            })?;
        }
    }
    Ok(())
}

async fn withdraw(via: &Via, ports: &[u16]) {
    for &port in ports {
        for protocol in [Protocol::Udp, Protocol::Tcp] {
            let done = match via {
                // A lifetime of 0 deletes the mapping.
                Via::NatPmp(_) => {
                    map(via, Ipv4Addr::UNSPECIFIED, port, protocol, Duration::ZERO).await
                }
                Via::Upnp(gw) => gw
                    .remove_port(upnp_proto(protocol), port)
                    .await
                    .map_err(anyhow::Error::from),
            };
            if let Err(e) = done {
                debug!("withdrawing {} {port}: {e:#}", proto_name(protocol));
            }
        }
    }
}

async fn map(
    via: &Via,
    ip: Ipv4Addr,
    port: u16,
    protocol: Protocol,
    lease: Duration,
) -> anyhow::Result<()> {
    match via {
        Via::NatPmp(gw) => natpmp_map(*gw, port, protocol, lease).await,
        Via::Upnp(gw) => gw
            .add_port(
                upnp_proto(protocol),
                port,
                SocketAddr::V4(SocketAddrV4::new(ip, port)),
                lease.as_secs() as u32,
                "archipel",
            )
            .await
            .map_err(anyhow::Error::from),
    }
}

fn upnp_proto(protocol: Protocol) -> PortMappingProtocol {
    match protocol {
        Protocol::Tcp => PortMappingProtocol::TCP,
        Protocol::Udp => PortMappingProtocol::UDP,
    }
}

fn proto_name(protocol: Protocol) -> &'static str {
    match protocol {
        Protocol::Tcp => "TCP",
        Protocol::Udp => "UDP",
    }
}

// ---------------------------------------------------------------- NAT-PMP (RFC 6886)

const NATPMP_PORT: u16 = 5351;

/// One request to the router, retried as the RFC says (250 ms, doubling).
async fn natpmp_ask(gateway: Ipv4Addr, request: &[u8], opcode: u8) -> anyhow::Result<Vec<u8>> {
    let socket = tokio::net::UdpSocket::bind("0.0.0.0:0").await?;
    socket.connect((gateway, NATPMP_PORT)).await?;
    let mut wait = Duration::from_millis(250);
    let mut buf = [0u8; 16];
    for _ in 0..4 {
        match socket.send(request).await {
            Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => bail!("no answer"),
            sent => sent?,
        };
        if let Ok(got) = tokio::time::timeout(wait, socket.recv(&mut buf)).await {
            let n = match got {
                // Nothing listens there: the router does not speak NAT-PMP.
                Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => {
                    bail!("no answer")
                }
                got => got?,
            };
            if n >= 4 && buf[0] == 0 && buf[1] == 128 + opcode {
                let code = u16::from_be_bytes([buf[2], buf[3]]);
                if code != 0 {
                    bail!("{}", natpmp_error(code));
                }
                return Ok(buf[..n].to_vec());
            }
        }
        wait *= 2;
    }
    bail!("no answer")
}

fn natpmp_error(code: u16) -> &'static str {
    match code {
        1 => "unsupported version",
        2 => "not allowed (turned off in the router's settings)",
        3 => "the router has no internet connection",
        4 => "the router has no room left",
        5 => "unsupported request",
        _ => "refused",
    }
}

async fn natpmp_external(gateway: Ipv4Addr) -> anyhow::Result<Ipv4Addr> {
    let answer = natpmp_ask(gateway, &[0, 0], 0).await?;
    if answer.len() < 12 {
        bail!("a short answer");
    }
    Ok(Ipv4Addr::new(answer[8], answer[9], answer[10], answer[11]))
}

async fn natpmp_map(
    gateway: Ipv4Addr,
    port: u16,
    protocol: Protocol,
    lease: Duration,
) -> anyhow::Result<()> {
    let opcode = match protocol {
        Protocol::Udp => 1,
        Protocol::Tcp => 2,
    };
    let mut request = vec![0, opcode, 0, 0];
    request.extend_from_slice(&port.to_be_bytes());
    // The same port outside: games announce the port they listen on.
    request.extend_from_slice(&port.to_be_bytes());
    request.extend_from_slice(&(lease.as_secs() as u32).to_be_bytes());
    let answer = natpmp_ask(gateway, &request, opcode).await?;
    if answer.len() < 16 {
        bail!("a short answer");
    }
    let external = u16::from_be_bytes([answer[10], answer[11]]);
    if !lease.is_zero() && external != port {
        bail!("the router gave port {external} instead of {port}, which is taken");
    }
    Ok(())
}

// ---------------------------------------------------------------- this computer

/// The address this computer uses on the home network: the one a packet to the
/// internet would leave from (nothing is sent).
pub fn local_ip() -> Option<Ipv4Addr> {
    let socket = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("1.1.1.1:80").ok()?;
    match socket.local_addr().ok()?.ip() {
        IpAddr::V4(ip) if !ip.is_unspecified() => Some(ip),
        _ => None,
    }
}

/// The router: the default route's gateway.
fn gateway() -> Option<Ipv4Addr> {
    let routes = std::fs::read_to_string("/proc/net/route").ok()?;
    routes.lines().skip(1).find_map(|line| {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.get(1) != Some(&"00000000") {
            return None;
        }
        let hex = u32::from_str_radix(fields.get(2)?, 16).ok()?;
        // Little-endian in that file.
        Some(Ipv4Addr::from(hex.to_le_bytes()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tells_who_can_reach() {
        let home = Ipv4Addr::new(192, 168, 1, 20);
        let public: IpAddr = "86.245.1.2".parse().unwrap();
        // Forwarded, the router's address is the internet's.
        let s = judge(true, Some(public), home, Some(public), None);
        assert_eq!((s.state, s.public_ip), (ReachState::Internet, Some(public)));
        // Forwarded, but the router itself sits behind the provider's NAT.
        let s = judge(
            true,
            Some("100.72.3.4".parse().unwrap()),
            home,
            Some(public),
            None,
        );
        assert_eq!(s.state, ReachState::NoPublicAddress);
        // Another router in front.
        let s = judge(
            true,
            Some("10.0.0.2".parse().unwrap()),
            home,
            Some(public),
            None,
        );
        assert_eq!(s.state, ReachState::NoPublicAddress);
        // Not forwarded.
        let s = judge(false, None, home, Some(public), Some("no answer".into()));
        assert_eq!(s.state, ReachState::LocalOnly);
        assert!(is_shared(Ipv4Addr::new(100, 64, 0, 1)));
        assert!(!is_shared(Ipv4Addr::new(100, 128, 0, 1)));
        assert!(!is_shared(Ipv4Addr::new(86, 245, 1, 2)));
    }
}
