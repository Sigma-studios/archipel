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
use archipel_core::api::{ReachState, Settings};
use archipel_core::game::Protocol;
use igd_next::PortMappingProtocol;
use igd_next::aio::Gateway;
use igd_next::aio::tokio::Tokio;
use parking_lot::Mutex;
use tracing::{debug, info};

use crate::store::Store;

/// Where UPnP devices listen for searches.
const SSDP: Ipv4Addr = Ipv4Addr::new(239, 255, 255, 250);

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
    /// The computer's, from the last check or server start.
    machine: Mutex<Status>,
    check_lock: tokio::sync::Mutex<()>,
    public: tokio::sync::Mutex<Option<(Instant, Option<IpAddr>)>>,
}

impl Reacher {
    pub fn new() -> anyhow::Result<Arc<Self>> {
        Ok(Arc::new(Reacher {
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(5))
                .build()?,
            servers: Default::default(),
            machine: Mutex::new(Status {
                state: ReachState::Checking,
                local_ip: local_ip(),
                public_ip: None,
                detail: None,
            }),
            check_lock: Default::default(),
            public: Default::default(),
        }))
    }

    pub fn status(&self, server: u64) -> Option<Status> {
        self.servers.lock().get(&server).map(|e| e.status.clone())
    }

    /// Whether this computer can receive games from the internet, as last seen.
    pub fn machine(&self) -> Status {
        self.machine.lock().clone()
    }

    /// Says a check is coming, before it starts in the background.
    pub fn checking(&self) {
        self.machine.lock().state = ReachState::Checking;
    }

    /// Finds out whether the internet can reach this computer: asks the router to
    /// forward `port` (one no server uses), then takes it back unless `keep`.
    pub async fn check(&self, port: u16, keep: bool) {
        let Ok(_one) = self.check_lock.try_lock() else {
            return;
        };
        *self.public.lock().await = None;
        let (status, via) = self.assess(&[port]).await;
        info!(?status.state, detail = ?status.detail, "this computer's reachability");
        if let (Some(via), false) = (&via, keep) {
            withdraw(via, &[port]).await;
        }
        *self.machine.lock() = status;
    }

    /// Forwards `ports` if the router will, and says who can reach them.
    async fn assess(&self, ports: &[u16]) -> (Status, Option<Via>) {
        let local_ip = local_ip();
        let public_ip = self.public_ip().await;
        match local_ip {
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
            Some(ip) => forward(&self.http, ip, ports, public_ip).await,
        }
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
        let (status, via) = self.assess(&ports).await;
        info!(server, ?status.state, "reachability");
        *self.machine.lock() = status.clone();
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
async fn forward(
    http: &reqwest::Client,
    ip: Ipv4Addr,
    ports: &[u16],
    public_ip: Option<IpAddr>,
) -> (Status, Option<Via>) {
    let mut problems = Vec::new();
    let mut router_ip = None;
    for attempt in [Attempt::NatPmp, Attempt::Upnp] {
        let tried = match attempt {
            Attempt::NatPmp => match gateway() {
                Some(gw) => natpmp_forward(gw, ip, ports).await,
                None => Err(anyhow!("no default route").into()),
            },
            Attempt::Upnp => upnp_forward(http, ip, ports).await,
        };
        match tried {
            Ok((via, external)) => return (judge(true, external, ip, public_ip, None), Some(via)),
            Err(Failure { external, error }) => {
                router_ip = router_ip.or(external);
                problems.push(format!("{}: {error:#}", attempt.name()));
            }
        }
    }
    let router = gateway().map_or("the router".to_string(), |gw| format!("the router ({gw})"));
    let detail = if problems.iter().all(|p| p.contains("no answer")) {
        let hidden = local_firewall()
            .map(|f| format!(", or {f} on this PC hides its answers"))
            .unwrap_or_default();
        format!(
            "{router} answered neither NAT-PMP nor UPnP, asked directly and on the whole \
             network: UPnP may be off in its settings{hidden}"
        )
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

async fn upnp_forward(http: &reqwest::Client, ip: Ipv4Addr, ports: &[u16]) -> Forwarded {
    // The router itself first: its answer to a search sent to everyone comes from
    // its own address, which a firewall here (NixOS's, by default) does not take
    // for an answer and drops; to a search sent to it, it is one.
    let found = match gateway() {
        Some(router) => upnp_ask(http, router)
            .await
            .map_err(|e| debug!("UPnP, asking the router {router}: {e:#}"))
            .ok(),
        None => None,
    };
    let gateway = match found {
        Some(gateway) => gateway,
        None => {
            let mut options = igd_next::SearchOptions::default();
            options.timeout = Some(Duration::from_secs(4));
            options.single_search_timeout = Some(Duration::from_secs(2));
            igd_next::aio::tokio::search_gateway(options)
                .await
                .map_err(|e| anyhow!("no answer ({e})"))?
        }
    };
    let external = gateway.get_external_ip().await.ok();
    let via = Via::Upnp(gateway);
    if let Err(error) = map_all(&via, ip, ports).await {
        return Err(Failure { external, error });
    }
    Ok((via, external))
}

/// What a UPnP search asks for: the router, or its WAN connection service.
const SEARCH_TARGETS: &[&str] = &[
    "urn:schemas-upnp-org:device:InternetGatewayDevice:1",
    "urn:schemas-upnp-org:service:WANIPConnection:1",
    "urn:schemas-upnp-org:service:WANPPPConnection:1",
];

/// UPnP's search, sent to the router alone, and the gateway it describes.
///
/// (igd-next's search, sent there, names the router in its `HOST` header, as the
/// specification has it for such a search; many routers only answer one naming
/// the multicast address, as a search sent to everyone does. Both are sent.)
async fn upnp_ask(http: &reqwest::Client, router: Ipv4Addr) -> anyhow::Result<Gateway<Tokio>> {
    let socket = tokio::net::UdpSocket::bind("0.0.0.0:0").await?;
    let to = SocketAddrV4::new(router, 1900);
    for host in [format!("{SSDP}:1900"), to.to_string()] {
        for target in SEARCH_TARGETS {
            let search = format!(
                "M-SEARCH * HTTP/1.1\r\nHOST: {host}\r\nMAN: \"ssdp:discover\"\r\nMX: 2\r\n\
                 ST: {target}\r\n\r\n"
            );
            socket.send_to(search.as_bytes(), to).await?;
        }
    }
    let until = tokio::time::Instant::now() + Duration::from_secs(3);
    let mut buf = [0u8; 2048];
    let mut tried = std::collections::HashSet::new();
    loop {
        let (n, from) = tokio::time::timeout_at(until, socket.recv_from(&mut buf))
            .await
            .map_err(|_| anyhow!("no answer"))??;
        if from.ip() != IpAddr::V4(router) {
            continue;
        }
        let Some(location) = search_location(&buf[..n]) else {
            continue;
        };
        if !tried.insert(location.clone()) {
            continue;
        }
        match describe_gateway(http, router, &location).await {
            Ok(gateway) => return Ok(gateway),
            Err(e) => debug!("UPnP, the router's description at {location}: {e:#}"),
        }
    }
}

/// The `LOCATION` of a search's answer: the device's description.
fn search_location(answer: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(answer).ok()?;
    if !text.starts_with("HTTP/1.1 200") {
        return None;
    }
    text.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.trim()
            .eq_ignore_ascii_case("location")
            .then(|| value.trim().to_string())
    })
}

/// The gateway the router describes at `location`: its WAN connection service and
/// the arguments of that service's actions, as igd-next's search would find them.
async fn describe_gateway(
    http: &reqwest::Client,
    router: Ipv4Addr,
    location: &str,
) -> anyhow::Result<Gateway<Tokio>> {
    let at_router = |url: &reqwest::Url| {
        url.scheme() == "http" && url.host_str() == Some(router.to_string().as_str())
    };
    let url = reqwest::Url::parse(location)?;
    if !at_router(&url) {
        bail!("not on the router");
    }
    let fetch = |url: reqwest::Url| async move {
        let text = http
            .get(url)
            .send()
            .await?
            .error_for_status()?
            .text()
            .await?;
        anyhow::Ok(xmltree::Element::parse(text.as_bytes())?)
    };
    let root = fetch(url.clone()).await?;
    let base = root
        .get_child("URLBase")
        .and_then(|b| b.get_text())
        .and_then(|b| reqwest::Url::parse(b.trim()).ok())
        .unwrap_or_else(|| url.clone());
    let (service_type, scpd, control) = wan_service(&root).context("no WAN connection service")?;
    let (scpd, control) = (base.join(&scpd)?, base.join(&control)?);
    if !at_router(&scpd) || !at_router(&control) {
        bail!("its service is not on the router");
    }
    let control_schema = action_arguments(&fetch(scpd.clone()).await?);
    if control_schema.is_empty() {
        bail!("its service lists no actions");
    }
    Ok(Gateway {
        addr: SocketAddr::new(
            IpAddr::V4(router),
            url.port_or_known_default().unwrap_or(80),
        ),
        root_url: url.path().to_string(),
        control_url: control.to_string(),
        control_schema_url: scpd.to_string(),
        control_schema,
        service_type,
        provider: Tokio,
    })
}

/// The first WAN connection service, anywhere in the device tree: its type, its
/// description's URL and its control URL.
fn wan_service(root: &xmltree::Element) -> Option<(String, String, String)> {
    let text = |e: &xmltree::Element, name: &str| {
        e.get_child(name)
            .and_then(|c| c.get_text())
            .map(|t| t.trim().to_string())
    };
    let elements = |e: &xmltree::Element, list: &str, item: &'static str| {
        e.get_child(list)
            .map(|l| {
                l.children
                    .iter()
                    .filter_map(|c| c.as_element())
                    .filter(|c| c.name == item)
                    .cloned()
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    };
    let mut devices: Vec<xmltree::Element> =
        root.get_child("device").cloned().into_iter().collect();
    while let Some(device) = devices.pop() {
        for service in elements(&device, "serviceList", "service") {
            let kind = text(&service, "serviceType").unwrap_or_default();
            if [
                "urn:schemas-upnp-org:service:WANIPConnection:1",
                "urn:schemas-upnp-org:service:WANIPConnection:2",
                "urn:schemas-upnp-org:service:WANPPPConnection:1",
            ]
            .contains(&kind.as_str())
                && let (Some(scpd), Some(control)) =
                    (text(&service, "SCPDURL"), text(&service, "controlURL"))
                && !control.is_empty()
            {
                return Some((kind, scpd, control));
            }
        }
        devices.extend(elements(&device, "deviceList", "device").into_iter().rev());
    }
    None
}

/// Each action of a service's description, with the names of its arguments in.
fn action_arguments(scpd: &xmltree::Element) -> HashMap<String, Vec<String>> {
    let children = |e: &xmltree::Element, name: &'static str| {
        e.children
            .iter()
            .filter_map(|c| c.as_element())
            .filter(move |c| c.name == name)
            .cloned()
            .collect::<Vec<_>>()
    };
    let text = |e: &xmltree::Element, name: &str| {
        e.get_child(name)
            .and_then(|c| c.get_text())
            .map(|t| t.trim().to_string())
    };
    let Some(list) = scpd.get_child("actionList") else {
        return HashMap::new();
    };
    children(list, "action")
        .iter()
        .filter_map(|action| {
            let arguments = action
                .get_child("argumentList")
                .map(|l| {
                    children(l, "argument")
                        .iter()
                        .filter(|a| text(a, "direction").as_deref() == Some("in"))
                        .filter_map(|a| text(a, "name"))
                        .collect()
                })
                .unwrap_or_default();
            Some((text(action, "name")?, arguments))
        })
        .collect()
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
/// A firewall on this computer that drops players' connections unless the ports
/// are opened in it, and how to open them: NixOS's (on by default), firewalld's
/// (Fedora's), ufw's (when turned on).
pub fn firewall_hint(first: u16, last: u16) -> Option<String> {
    let exists = |p: &str| std::path::Path::new(p).exists();
    let nixos = is_nixos();
    let nixos_units = ["firewall.service", "nftables.service"]
        .map(|u| std::path::Path::new("/etc/systemd/system").join(u));
    // Without its units in sight (in appimage-run's sandbox), it may well be on.
    let nixos_blocks = !exists("/etc/systemd/system")
        || (nixos_units.iter().any(|u| u.exists())
            && !nixos_units.iter().any(|u| opens(u, first, last)));
    if nixos && nixos_blocks {
        return Some(format!(
            "NixOS's firewall turns players away unless these ports are open in it: add \
             networking.firewall.allowedTCPPortRanges = [ {{ from = {first}; to = {last}; }} ]; \
             and the same allowedUDPPortRanges to your configuration, then run nixos-rebuild switch."
        ));
    }
    if exists("/run/firewalld") {
        return Some(format!(
            "firewalld turns players away unless these ports are open in it: run sudo \
             firewall-cmd --permanent --add-port={first}-{last}/tcp \
             --add-port={first}-{last}/udp, then sudo firewall-cmd --reload."
        ));
    }
    let ufw_on = std::fs::read_to_string("/etc/ufw/ufw.conf")
        .is_ok_and(|c| c.lines().any(|l| l.trim() == "ENABLED=yes"));
    if ufw_on {
        return Some(format!(
            "ufw turns players away unless these ports are open in it: run sudo ufw allow \
             {first}:{last}/tcp, then sudo ufw allow {first}:{last}/udp."
        ));
    }
    None
}

/// NixOS, seen from the host or from appimage-run's sandbox (which keeps /run).
fn is_nixos() -> bool {
    let exists = |p: &str| std::path::Path::new(p).exists();
    exists("/etc/NIXOS")
        || exists("/run/current-system/nixos-version")
        || std::fs::read_to_string("/etc/os-release")
            .is_ok_and(|r| r.lines().any(|l| l.trim() == "ID=nixos"))
}

/// A firewall on this computer, whatever ports it opens: it drops the answers
/// to a search sent to the whole network.
fn local_firewall() -> Option<&'static str> {
    let exists = |p: &str| std::path::Path::new(p).exists();
    let nixos_on = ["firewall.service", "nftables.service"]
        .iter()
        .any(|u| exists(&format!("/etc/systemd/system/{u}")))
        || !exists("/etc/systemd/system");
    if is_nixos() && nixos_on {
        Some("NixOS's firewall")
    } else if exists("/run/firewalld") {
        Some("firewalld")
    } else if std::fs::read_to_string("/etc/ufw/ufw.conf")
        .is_ok_and(|c| c.lines().any(|l| l.trim() == "ENABLED=yes"))
    {
        Some("ufw")
    } else {
        None
    }
}

/// Whether the NixOS firewall `unit` lets the pool in: its rules, in the scripts
/// and files of the Nix store it runs, accept the range (`30000:30019` for
/// iptables, `30000-30019` for nftables) for TCP and UDP.
fn opens(unit: &std::path::Path, first: u16, last: u16) -> bool {
    let ranges = [format!("{first}:{last}"), format!("{first}-{last}")];
    let mut seen = std::collections::HashSet::new();
    let mut files = vec![unit.to_path_buf()];
    let (mut tcp, mut udp) = (false, false);
    while let Some(file) = files.pop() {
        if seen.len() > 50 || !seen.insert(file.clone()) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        for line in text.lines() {
            if ranges.iter().any(|r| line.contains(r.as_str())) {
                tcp |= line.contains("tcp");
                udp |= line.contains("udp");
            }
        }
        // The files it refers to: /nix/store/<hash>-<name>[/…].
        for word in text.split(|c: char| c.is_whitespace() || "\"';=()".contains(c)) {
            if word.starts_with("/nix/store/") && seen.len() + files.len() < 50 {
                files.push(word.into());
            }
        }
    }
    tcp && udp
}

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

// ---------------------------------------------------------------- settings

/// The hosted archipel asked to check from outside, unless the settings name
/// another (or none).
pub const DEFAULT_PROBE_URL: &str = "https://games.sigma-dev.eu";
const PROBE_URL: &str = "probe_url";
const PUBLIC_HOST: &str = "public_host";

/// The desktop's settings: an unset URL is the default one, an empty one turns
/// checking off.
pub fn settings(store: &Store) -> anyhow::Result<Settings> {
    Ok(Settings {
        probe_url: store
            .setting(PROBE_URL)?
            .unwrap_or_else(|| DEFAULT_PROBE_URL.into()),
        default_probe_url: DEFAULT_PROBE_URL.into(),
        public_host: store.setting(PUBLIC_HOST)?.unwrap_or_default(),
    })
}

/// Settings as they are saved, once seen to be valid: `None` is unset (the
/// default URL stays "the default", to follow later versions').
#[derive(Debug, PartialEq)]
pub struct Saved {
    probe_url: Option<String>,
    public_host: Option<String>,
}

impl Saved {
    pub fn from(new: &Settings) -> Result<Saved, String> {
        let url = new.probe_url.trim().trim_end_matches('/');
        let probe_url = match url {
            "" => Some(String::new()),
            url if url == DEFAULT_PROBE_URL => None,
            url => {
                let parsed = reqwest::Url::parse(url).ok().filter(|u| {
                    let local = matches!(u.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
                    (u.scheme() == "https" || (u.scheme() == "http" && local))
                        && u.host_str().is_some()
                        && u.path() == "/"
                        && u.query().is_none()
                        && u.fragment().is_none()
                        && u.username().is_empty()
                });
                if parsed.is_none() {
                    return Err(format!(
                        "{url} is not the address of an archipel: give one like {DEFAULT_PROBE_URL}"
                    ));
                }
                Some(url.to_string())
            }
        };
        let public_host = match new.public_host.trim() {
            "" => None,
            host => Some(crate::probe::dns_name(host).ok_or_else(|| {
                format!("{host} is not a hostname: give one like yourname.ddns.net")
            })?),
        };
        Ok(Saved {
            probe_url,
            public_host,
        })
    }

    pub fn save(&self, store: &Store) -> anyhow::Result<()> {
        store.set_setting(PROBE_URL, self.probe_url.as_deref())?;
        store.set_setting(PUBLIC_HOST, self.public_host.as_deref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saves_only_valid_settings() {
        let saved = |url: &str, host: &str| {
            Saved::from(&Settings {
                probe_url: url.into(),
                default_probe_url: String::new(),
                public_host: host.into(),
            })
        };
        assert_eq!(
            saved(" https://games.sigma-dev.eu/ ", ""),
            Ok(Saved {
                probe_url: None,
                public_host: None
            })
        );
        assert_eq!(
            saved("", "Sigma-Dev.ddns.net"),
            Ok(Saved {
                probe_url: Some(String::new()),
                public_host: Some("sigma-dev.ddns.net".into())
            })
        );
        assert_eq!(
            saved("http://localhost:7810", "")
                .unwrap()
                .probe_url
                .as_deref(),
            Some("http://localhost:7810")
        );
        assert!(saved("https://games.example.com", "").is_ok());
        for bad in [
            "http://games.example.com",
            "games.example.com",
            "https://games.example.com/api",
            "https://games.example.com/?x=1",
            "ftp://games.example.com",
        ] {
            assert!(saved(bad, "").is_err(), "{bad}");
        }
        assert!(saved("", "not a host").is_err());
        assert!(saved("", "86.245.1.2").is_err());
    }

    #[test]
    fn reads_a_routers_description() {
        let answer = b"HTTP/1.1 200 OK\r\nCACHE-CONTROL: max-age=120\r\n\
            Location: http://192.168.1.1:5000/rootDesc.xml\r\nST: x\r\n\r\n";
        assert_eq!(
            search_location(answer).as_deref(),
            Some("http://192.168.1.1:5000/rootDesc.xml")
        );
        assert_eq!(
            search_location(b"NOTIFY * HTTP/1.1\r\nLOCATION: http://x/\r\n"),
            None
        );

        let root = xmltree::Element::parse(
            r#"<?xml version="1.0"?><root xmlns="urn:schemas-upnp-org:device-1-0"><device>
            <deviceType>urn:schemas-upnp-org:device:InternetGatewayDevice:1</deviceType>
            <deviceList><device><deviceType>urn:schemas-upnp-org:device:WANDevice:1</deviceType>
            <deviceList><device><serviceList><service>
            <serviceType>urn:schemas-upnp-org:service:WANIPConnection:1</serviceType>
            <controlURL>/ctl/IPConn</controlURL><SCPDURL>/WANIPCn.xml</SCPDURL>
            </service></serviceList></device></deviceList></device></deviceList></device></root>"#
                .as_bytes(),
        )
        .unwrap();
        assert_eq!(
            wan_service(&root),
            Some((
                "urn:schemas-upnp-org:service:WANIPConnection:1".into(),
                "/WANIPCn.xml".into(),
                "/ctl/IPConn".into()
            ))
        );

        let scpd = xmltree::Element::parse(
            r#"<scpd xmlns="urn:schemas-upnp-org:service-1-0"><actionList>
            <action><name>DeletePortMapping</name><argumentList>
            <argument><name>NewRemoteHost</name><direction>in</direction></argument>
            <argument><name>NewExternalPort</name><direction>in</direction></argument>
            <argument><name>NewProtocol</name><direction>in</direction></argument>
            </argumentList></action>
            <action><name>GetExternalIPAddress</name><argumentList>
            <argument><name>NewExternalIPAddress</name><direction>out</direction></argument>
            </argumentList></action></actionList></scpd>"#
                .as_bytes(),
        )
        .unwrap();
        let actions = action_arguments(&scpd);
        assert_eq!(
            actions["DeletePortMapping"],
            ["NewRemoteHost", "NewExternalPort", "NewProtocol"]
        );
        assert!(actions["GetExternalIPAddress"].is_empty());
    }

    #[test]
    fn reads_whether_the_firewall_opens_the_pool() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join(format!("../../target/firewall-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let unit = dir.join("firewall.service");
        std::fs::write(
            &unit,
            "ip46tables -A nixos-fw -p tcp --dport 30000:30019 -j nixos-fw-accept\n",
        )
        .unwrap();
        assert!(!opens(&unit, 30000, 30019), "UDP is still closed");
        std::fs::write(
            &unit,
            "ip46tables -A nixos-fw -p tcp --dport 30000:30019 -j nixos-fw-accept\n\
             ip46tables -A nixos-fw -p udp --dport 30000:30019 -j nixos-fw-accept\n",
        )
        .unwrap();
        assert!(opens(&unit, 30000, 30019));
        assert!(!opens(&unit, 31000, 31019));
        std::fs::write(
            &unit,
            "tcp dport { 30000-30019 } accept\nudp dport { 30000-30019 } accept\n",
        )
        .unwrap();
        assert!(opens(&unit, 30000, 30019), "nftables");
        std::fs::remove_dir_all(&dir).unwrap();
    }

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
