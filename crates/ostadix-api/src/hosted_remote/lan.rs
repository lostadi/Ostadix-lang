//! Zero-configuration LAN discovery and enrollment for Ostadix hosted nodes.
//!
//! This module deliberately implements a usability-first trust model. A node
//! advertising `lan-open` mode makes a shared client identity available to any
//! peer that can reach its bootstrap port. The ordinary hosted transport still
//! uses TLS 1.3, but possession of LAN reachability is treated as authorization
//! to enroll. Operators who need explicit trust boundaries should use the
//! manual `o-node` / `octl` connection flags instead.

use std::collections::BTreeMap;
use std::fs;
use std::io::{ErrorKind, Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};

use anyhow::{bail, Context, Result};
use if_addrs::IfAddr;
use serde::{Deserialize, Serialize};
use socket2::SockRef;

pub const LAN_DISCOVERY_SCHEMA_V1: &str = "ostadix.lan-discovery/v1";
pub const LAN_BOOTSTRAP_SCHEMA_V1: &str = "ostadix.lan-bootstrap/v1";
pub const LAN_PEER_SCHEMA_V1: &str = "ostadix.lan-peer/v1";
pub const LAN_SECURITY_MODE: &str = "lan-open";
pub const DEFAULT_LAN_NODE_PORT: u16 = 7337;
pub const DEFAULT_LAN_BOOTSTRAP_PORT: u16 = 7338;
pub const DEFAULT_LAN_DISCOVERY_PORT: u16 = 7339;
pub const DEFAULT_LAN_DISCOVERY_MILLIS: u64 = 900;
const DISCOVERY_MULTICAST: Ipv4Addr = Ipv4Addr::new(239, 255, 73, 37);
const MAX_BOOTSTRAP_BYTES: usize = 4 * 1024 * 1024;
const MAX_DISCOVERY_DATAGRAMS_PER_SOCKET: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct LanIpv4Interface {
    address: Ipv4Addr,
    broadcast: Option<Ipv4Addr>,
}

struct LanDiscoverySocket {
    socket: UdpSocket,
    interface: Option<LanIpv4Interface>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LanDiscoveryRequestV1 {
    schema: String,
    request: String,
}

impl LanDiscoveryRequestV1 {
    fn discover() -> Self {
        Self {
            schema: LAN_DISCOVERY_SCHEMA_V1.to_owned(),
            request: "discover".to_owned(),
        }
    }

    fn validate(&self) -> Result<()> {
        if self.schema != LAN_DISCOVERY_SCHEMA_V1 || self.request != "discover" {
            bail!("unsupported Ostadix LAN discovery request");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LanNodeAdvertisementV1 {
    pub schema: String,
    pub node_id: String,
    pub server_name: String,
    pub service_port: u16,
    pub bootstrap_port: u16,
    pub supports_v2: bool,
    pub security_mode: String,
}

impl LanNodeAdvertisementV1 {
    pub fn new(
        node_id: impl Into<String>,
        server_name: impl Into<String>,
        service_port: u16,
        bootstrap_port: u16,
        supports_v2: bool,
    ) -> Result<Self> {
        let value = Self {
            schema: LAN_DISCOVERY_SCHEMA_V1.to_owned(),
            node_id: node_id.into(),
            server_name: server_name.into(),
            service_port,
            bootstrap_port,
            supports_v2,
            security_mode: LAN_SECURITY_MODE.to_owned(),
        };
        value.validate()?;
        Ok(value)
    }

    pub fn validate(&self) -> Result<()> {
        if self.schema != LAN_DISCOVERY_SCHEMA_V1 {
            bail!("unsupported Ostadix LAN advertisement schema `{}`", self.schema);
        }
        validate_lan_identifier("node_id", &self.node_id)?;
        validate_server_name(&self.server_name)?;
        if self.service_port == 0 || self.bootstrap_port == 0 {
            bail!("LAN service and bootstrap ports must be nonzero");
        }
        if self.security_mode != LAN_SECURITY_MODE {
            bail!("unsupported LAN security mode `{}`", self.security_mode);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredLanNodeV1 {
    pub advertisement: LanNodeAdvertisementV1,
    pub source_ip: IpAddr,
}

impl DiscoveredLanNodeV1 {
    pub fn service_address(&self) -> SocketAddr {
        SocketAddr::new(self.source_ip, self.advertisement.service_port)
    }

    pub fn bootstrap_address(&self) -> SocketAddr {
        SocketAddr::new(self.source_ip, self.advertisement.bootstrap_port)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LanBootstrapBundleV1 {
    pub schema: String,
    pub node_id: String,
    pub server_name: String,
    pub service_port: u16,
    pub security_mode: String,
    pub ca_pem: String,
    pub client_cert_pem: String,
    pub client_key_pem: String,
    pub node_receipt_public_key: Option<String>,
}

impl LanBootstrapBundleV1 {
    pub fn validate(&self) -> Result<()> {
        if self.schema != LAN_BOOTSTRAP_SCHEMA_V1 {
            bail!("unsupported Ostadix LAN bootstrap schema `{}`", self.schema);
        }
        validate_lan_identifier("node_id", &self.node_id)?;
        validate_server_name(&self.server_name)?;
        if self.service_port == 0 {
            bail!("LAN bootstrap service port must be nonzero");
        }
        if self.security_mode != LAN_SECURITY_MODE {
            bail!("unsupported LAN bootstrap security mode `{}`", self.security_mode);
        }
        require_pem("CA", &self.ca_pem, "CERTIFICATE")?;
        require_pem("client certificate", &self.client_cert_pem, "CERTIFICATE")?;
        require_private_key_pem("client key", &self.client_key_pem)?;
        if let Some(key) = &self.node_receipt_public_key {
            validate_lower_hex("node receipt public key", key, 64)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredLanPeerV1 {
    pub schema: String,
    pub node_id: String,
    pub server_name: String,
    pub address: String,
    pub service_port: u16,
    pub security_mode: String,
    pub supports_v2: bool,
}

impl StoredLanPeerV1 {
    pub fn validate(&self) -> Result<()> {
        if self.schema != LAN_PEER_SCHEMA_V1 {
            bail!("unsupported stored LAN peer schema `{}`", self.schema);
        }
        validate_lan_identifier("node_id", &self.node_id)?;
        validate_server_name(&self.server_name)?;
        self.address
            .parse::<SocketAddr>()
            .with_context(|| format!("stored peer address `{}` is invalid", self.address))?;
        if self.service_port == 0 {
            bail!("stored LAN peer service port must be nonzero");
        }
        if self.security_mode != LAN_SECURITY_MODE {
            bail!("unsupported stored LAN peer security mode `{}`", self.security_mode);
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct StoredLanPeerPathsV1 {
    pub directory: PathBuf,
    pub metadata: PathBuf,
    pub ca: PathBuf,
    pub client_cert: PathBuf,
    pub client_key: PathBuf,
    pub node_receipt_public_key: PathBuf,
}

impl StoredLanPeerPathsV1 {
    pub fn for_root(root: &Path, node_id: &str) -> Result<Self> {
        validate_lan_identifier("node_id", node_id)?;
        let directory = root.join(node_id);
        Ok(Self {
            metadata: directory.join("peer.json"),
            ca: directory.join("ca.pem"),
            client_cert: directory.join("client-cert.pem"),
            client_key: directory.join("client-key.pem"),
            node_receipt_public_key: directory.join("node-signing-public.v2"),
            directory,
        })
    }
}

pub fn spawn_lan_discovery_responder(
    advertisement: LanNodeAdvertisementV1,
    discovery_port: u16,
) -> Result<thread::JoinHandle<()>> {
    advertisement.validate()?;
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, discovery_port)).with_context(|| {
        format!("failed to bind Ostadix LAN discovery UDP port {discovery_port}")
    })?;
    join_discovery_multicast_interfaces(&socket)?;
    socket
        .set_read_timeout(Some(Duration::from_secs(1)))
        .context("failed to configure Ostadix LAN discovery socket")?;
    let encoded = Arc::new(serde_json::to_vec(&advertisement)?);
    thread::Builder::new()
        .name("ostadix-lan-discovery".to_owned())
        .spawn(move || {
            let mut buffer = [0_u8; 2048];
            loop {
                match socket.recv_from(&mut buffer) {
                    Ok((length, source)) => {
                        let request = serde_json::from_slice::<LanDiscoveryRequestV1>(
                            &buffer[..length],
                        );
                        if request.as_ref().is_ok_and(|value| value.validate().is_ok()) {
                            if let Err(error) = socket.send_to(encoded.as_slice(), source) {
                                eprintln!(
                                    "o-node: failed to answer LAN discovery request from {source}: {error}"
                                );
                            }
                        }
                    }
                    Err(error)
                        if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
                    Err(error) => {
                        eprintln!("o-node: LAN discovery responder stopped: {error}");
                        break;
                    }
                }
            }
        })
        .context("failed to spawn Ostadix LAN discovery responder")
}

pub fn discover_lan_nodes(timeout: Duration) -> Result<Vec<DiscoveredLanNodeV1>> {
    if timeout.is_zero() {
        bail!("LAN discovery timeout must be positive");
    }
    let sockets = open_lan_discovery_sockets()?;
    let request = serde_json::to_vec(&LanDiscoveryRequestV1::discover())?;
    let port = DEFAULT_LAN_DISCOVERY_PORT;
    let mut sent = false;
    for discovery_socket in &sockets {
        for destination in discovery_destinations(discovery_socket.interface, port) {
            match discovery_socket.socket.send_to(&request, destination) {
                Ok(_) => sent = true,
                Err(error) => {
                    eprintln!(
                        "octl: LAN discovery probe to {destination} could not be sent: {error}"
                    );
                }
            }
        }
    }
    if !sent {
        bail!("failed to send any Ostadix LAN discovery probe");
    }

    let deadline = Instant::now() + timeout;
    let mut discovered = BTreeMap::new();
    let mut buffer = [0_u8; 4096];
    while Instant::now() < deadline {
        let mut received = false;
        for discovery_socket in &sockets {
            for _ in 0..MAX_DISCOVERY_DATAGRAMS_PER_SOCKET {
                if Instant::now() >= deadline {
                    break;
                }
                match discovery_socket.socket.recv_from(&mut buffer) {
                    Ok((length, source)) => {
                        received = true;
                        record_discovered_node(&mut discovered, &buffer[..length], source);
                    }
                    Err(error) if error.kind() == ErrorKind::WouldBlock => break,
                    Err(error) => {
                        return Err(error).context("Ostadix LAN discovery receive failed");
                    }
                }
            }
        }
        if !received {
            let remaining = deadline.saturating_duration_since(Instant::now());
            thread::sleep(remaining.min(Duration::from_millis(10)));
        }
    }
    Ok(discovered.into_values().collect())
}

fn active_lan_ipv4_interfaces() -> Result<Vec<LanIpv4Interface>> {
    let mut interfaces = BTreeMap::<Ipv4Addr, Option<Ipv4Addr>>::new();
    for interface in if_addrs::get_if_addrs().context("failed to enumerate IPv4 interfaces")? {
        if !interface.is_oper_up() {
            continue;
        }
        let IfAddr::V4(address) = interface.addr else {
            continue;
        };
        if address.ip.is_unspecified()
            || address.ip.is_multicast()
            || address.ip == Ipv4Addr::BROADCAST
        {
            continue;
        }
        interfaces
            .entry(address.ip)
            .and_modify(|broadcast| {
                if broadcast.is_none() {
                    *broadcast = address.broadcast;
                }
            })
            .or_insert(address.broadcast);
    }
    if interfaces.is_empty() {
        bail!("no active IPv4 interfaces are available for Ostadix LAN discovery");
    }
    Ok(interfaces
        .into_iter()
        .map(|(address, broadcast)| LanIpv4Interface { address, broadcast })
        .collect())
}

fn join_discovery_multicast_interfaces(socket: &UdpSocket) -> Result<()> {
    let interfaces = match active_lan_ipv4_interfaces() {
        Ok(interfaces) => interfaces,
        Err(error) => {
            eprintln!(
                "o-node: IPv4 interface enumeration failed; using the default multicast interface: {error:#}"
            );
            Vec::new()
        }
    };
    let mut joined = false;
    for interface in interfaces {
        match socket.join_multicast_v4(&DISCOVERY_MULTICAST, &interface.address) {
            Ok(()) => joined = true,
            Err(error) => eprintln!(
                "o-node: could not join LAN discovery multicast on {}: {error}",
                interface.address
            ),
        }
    }
    if !joined {
        socket
            .join_multicast_v4(&DISCOVERY_MULTICAST, &Ipv4Addr::UNSPECIFIED)
            .context("failed to join Ostadix LAN discovery multicast group")?;
    }
    Ok(())
}

fn open_lan_discovery_sockets() -> Result<Vec<LanDiscoverySocket>> {
    let interfaces = match active_lan_ipv4_interfaces() {
        Ok(interfaces) => interfaces,
        Err(error) => {
            eprintln!(
                "octl: IPv4 interface enumeration failed; using the default route for LAN discovery: {error:#}"
            );
            Vec::new()
        }
    };
    let mut sockets = Vec::new();
    for interface in interfaces {
        let socket = match UdpSocket::bind((interface.address, 0)) {
            Ok(socket) => socket,
            Err(error) => {
                eprintln!(
                    "octl: could not bind a LAN discovery probe on {}: {error}",
                    interface.address
                );
                continue;
            }
        };
        configure_lan_discovery_socket(&socket)?;
        if !interface.address.is_loopback() {
            if let Err(error) = SockRef::from(&socket).set_multicast_if_v4(&interface.address) {
                eprintln!(
                    "octl: could not select {} for multicast discovery: {error}",
                    interface.address
                );
            }
        }
        sockets.push(LanDiscoverySocket {
            socket,
            interface: Some(interface),
        });
    }
    if sockets.is_empty() {
        let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))
            .context("failed to bind Ostadix LAN discovery client socket")?;
        configure_lan_discovery_socket(&socket)?;
        sockets.push(LanDiscoverySocket {
            socket,
            interface: None,
        });
    }
    Ok(sockets)
}

fn configure_lan_discovery_socket(socket: &UdpSocket) -> Result<()> {
    socket
        .set_broadcast(true)
        .context("failed to enable Ostadix LAN broadcast discovery")?;
    socket
        .set_nonblocking(true)
        .context("failed to configure Ostadix LAN discovery socket")
}

fn discovery_destinations(interface: Option<LanIpv4Interface>, port: u16) -> Vec<SocketAddr> {
    let addresses = match interface {
        Some(interface) if interface.address.is_loopback() => vec![Ipv4Addr::LOCALHOST],
        Some(interface) => {
            let mut addresses = Vec::with_capacity(3);
            if let Some(broadcast) = interface.broadcast {
                addresses.push(broadcast);
            }
            addresses.push(Ipv4Addr::BROADCAST);
            addresses.push(DISCOVERY_MULTICAST);
            addresses
        }
        None => vec![
            Ipv4Addr::BROADCAST,
            DISCOVERY_MULTICAST,
            Ipv4Addr::LOCALHOST,
        ],
    };
    let mut destinations = Vec::with_capacity(addresses.len());
    for address in addresses {
        let destination = SocketAddr::from((address, port));
        if !destinations.contains(&destination) {
            destinations.push(destination);
        }
    }
    destinations
}

fn record_discovered_node(
    discovered: &mut BTreeMap<String, DiscoveredLanNodeV1>,
    payload: &[u8],
    source: SocketAddr,
) {
    let Ok(advertisement) = serde_json::from_slice::<LanNodeAdvertisementV1>(payload) else {
        return;
    };
    if advertisement.validate().is_err() {
        return;
    }
    let key = format!("{}@{}", advertisement.node_id, source.ip());
    discovered.insert(
        key,
        DiscoveredLanNodeV1 {
            advertisement,
            source_ip: source.ip(),
        },
    );
}

pub fn spawn_lan_bootstrap_server(
    bind_address: SocketAddr,
    bundle: LanBootstrapBundleV1,
) -> Result<thread::JoinHandle<()>> {
    bundle.validate()?;
    let listener = TcpListener::bind(bind_address).with_context(|| {
        format!(
            "failed to bind Ostadix LAN bootstrap service on {bind_address}"
        )
    })?;
    let payload = serde_json::to_vec(&bundle)?;
    if payload.len() > MAX_BOOTSTRAP_BYTES {
        bail!("Ostadix LAN bootstrap bundle exceeds {MAX_BOOTSTRAP_BYTES} bytes");
    }
    let payload = Arc::new(payload);
    thread::Builder::new()
        .name("ostadix-lan-bootstrap".to_owned())
        .spawn(move || {
            for incoming in listener.incoming() {
                let payload = Arc::clone(&payload);
                match incoming {
                    Ok(mut stream) => {
                        let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                        let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
                        thread::spawn(move || {
                            let length = (payload.len() as u32).to_be_bytes();
                            if let Err(error) = stream
                                .write_all(&length)
                                .and_then(|_| stream.write_all(payload.as_slice()))
                                .and_then(|_| stream.flush())
                            {
                                eprintln!("o-node: LAN enrollment delivery failed: {error}");
                            }
                        });
                    }
                    Err(error) => {
                        eprintln!("o-node: LAN bootstrap accept failed: {error}");
                    }
                }
            }
        })
        .context("failed to spawn Ostadix LAN bootstrap service")
}

pub fn fetch_lan_bootstrap(
    node: &DiscoveredLanNodeV1,
    timeout: Duration,
) -> Result<LanBootstrapBundleV1> {
    node.advertisement.validate()?;
    let address = node.bootstrap_address();
    let mut stream = TcpStream::connect_timeout(&address, timeout).with_context(|| {
        format!("failed to enroll with discovered Ostadix node at {address}")
    })?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    let mut length = [0_u8; 4];
    stream
        .read_exact(&mut length)
        .context("LAN bootstrap closed before its length prefix")?;
    let length = u32::from_be_bytes(length) as usize;
    if length == 0 || length > MAX_BOOTSTRAP_BYTES {
        bail!("LAN bootstrap announced invalid payload length {length}");
    }
    let mut payload = vec![0_u8; length];
    stream
        .read_exact(&mut payload)
        .context("LAN bootstrap closed before the complete enrollment bundle")?;
    let bundle: LanBootstrapBundleV1 =
        serde_json::from_slice(&payload).context("failed to decode LAN enrollment bundle")?;
    bundle.validate()?;
    if bundle.node_id != node.advertisement.node_id
        || bundle.server_name != node.advertisement.server_name
        || bundle.service_port != node.advertisement.service_port
    {
        bail!("LAN bootstrap identity differs from the discovery advertisement");
    }
    Ok(bundle)
}

pub fn store_lan_peer(
    peers_root: &Path,
    node: &DiscoveredLanNodeV1,
    bundle: &LanBootstrapBundleV1,
) -> Result<(StoredLanPeerV1, StoredLanPeerPathsV1)> {
    bundle.validate()?;
    if bundle.node_id != node.advertisement.node_id {
        bail!("cannot store a LAN enrollment bundle for a different node");
    }
    ensure_private_directory(peers_root)?;
    let paths = StoredLanPeerPathsV1::for_root(peers_root, &bundle.node_id)?;
    ensure_private_directory(&paths.directory)?;
    let address = node.service_address().to_string();
    let metadata = StoredLanPeerV1 {
        schema: LAN_PEER_SCHEMA_V1.to_owned(),
        node_id: bundle.node_id.clone(),
        server_name: bundle.server_name.clone(),
        address,
        service_port: bundle.service_port,
        security_mode: bundle.security_mode.clone(),
        supports_v2: bundle.node_receipt_public_key.is_some(),
    };
    metadata.validate()?;

    write_atomic(&paths.ca, bundle.ca_pem.as_bytes(), false)?;
    write_atomic(&paths.client_cert, bundle.client_cert_pem.as_bytes(), false)?;
    write_atomic(&paths.client_key, bundle.client_key_pem.as_bytes(), true)?;
    if let Some(public_key) = &bundle.node_receipt_public_key {
        let mut bytes = public_key.as_bytes().to_vec();
        bytes.push(b'\n');
        write_atomic(&paths.node_receipt_public_key, &bytes, false)?;
    } else if paths.node_receipt_public_key.exists() {
        fs::remove_file(&paths.node_receipt_public_key)?;
    }
    let mut encoded = serde_json::to_vec_pretty(&metadata)?;
    encoded.push(b'\n');
    write_atomic(&paths.metadata, &encoded, false)?;
    Ok((metadata, paths))
}

pub fn load_stored_lan_peer(
    peers_root: &Path,
    node_id: &str,
) -> Result<(StoredLanPeerV1, StoredLanPeerPathsV1)> {
    let paths = StoredLanPeerPathsV1::for_root(peers_root, node_id)?;
    let metadata: StoredLanPeerV1 = serde_json::from_slice(
        &fs::read(&paths.metadata).with_context(|| {
            format!("failed to read stored LAN peer `{node_id}` metadata")
        })?,
    )
    .with_context(|| format!("failed to decode stored LAN peer `{node_id}` metadata"))?;
    metadata.validate()?;
    for required in [&paths.ca, &paths.client_cert, &paths.client_key] {
        if !required.is_file() {
            bail!(
                "stored LAN peer `{node_id}` is incomplete: missing `{}`",
                required.display()
            );
        }
    }
    Ok((metadata, paths))
}

pub fn list_stored_lan_peers(
    peers_root: &Path,
) -> Result<Vec<(StoredLanPeerV1, StoredLanPeerPathsV1)>> {
    if !peers_root.exists() {
        return Ok(Vec::new());
    }
    let mut peers = Vec::new();
    for entry in fs::read_dir(peers_root)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let Some(node_id) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if let Ok(peer) = load_stored_lan_peer(peers_root, &node_id) {
            peers.push(peer);
        }
    }
    peers.sort_by(|left, right| left.0.node_id.cmp(&right.0.node_id));
    Ok(peers)
}

fn validate_lan_identifier(field: &str, value: &str) -> Result<()> {
    if value.is_empty() || value.len() > 128 {
        bail!("{field} must contain between 1 and 128 bytes");
    }
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
    {
        bail!("{field} contains characters outside [A-Za-z0-9._:-]");
    }
    Ok(())
}

fn validate_server_name(value: &str) -> Result<()> {
    if value.is_empty() || value.len() > 253 || !value.is_ascii() {
        bail!("server_name must be a non-empty ASCII DNS name or IP address");
    }
    Ok(())
}

fn validate_lower_hex(field: &str, value: &str, length: usize) -> Result<()> {
    if value.len() != length
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        bail!("{field} must be lowercase hexadecimal with exactly {length} characters");
    }
    Ok(())
}

fn require_pem(label: &str, value: &str, kind: &str) -> Result<()> {
    if !value.contains(&format!("-----BEGIN {kind}"))
        || !value.contains(&format!("-----END {kind}"))
    {
        bail!("LAN bootstrap {label} is not a PEM {kind}");
    }
    Ok(())
}

fn require_private_key_pem(label: &str, value: &str) -> Result<()> {
    const KEY_KINDS: [&str; 3] = ["PRIVATE KEY", "RSA PRIVATE KEY", "EC PRIVATE KEY"];
    if KEY_KINDS.iter().any(|kind| {
        value.contains(&format!("-----BEGIN {kind}-----"))
            && value.contains(&format!("-----END {kind}-----"))
    }) {
        return Ok(());
    }
    bail!("LAN bootstrap {label} is not a supported PEM private key")
}

fn ensure_private_directory(path: &Path) -> Result<()> {
    if path.exists() {
        if !path.is_dir() {
            bail!("`{}` must be a directory", path.display());
        }
        #[cfg(unix)]
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
        return Ok(());
    }
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    builder.mode(0o700);
    builder
        .create(path)
        .with_context(|| format!("failed to create `{}`", path.display()))
}

fn write_atomic(path: &Path, bytes: &[u8], private: bool) -> Result<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    ensure_private_directory(parent)?;
    let mut random = [0_u8; 8];
    getrandom::fill(&mut random).context("failed to obtain entropy for atomic LAN peer write")?;
    let temporary = parent.join(format!(
        ".{}.{}",
        path.file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("peer"),
        hex::encode(random)
    ));
    fs::write(&temporary, bytes)
        .with_context(|| format!("failed to write `{}`", temporary.display()))?;
    #[cfg(unix)]
    fs::set_permissions(
        &temporary,
        fs::Permissions::from_mode(if private { 0o600 } else { 0o644 }),
    )?;
    fs::rename(&temporary, path).with_context(|| {
        format!(
            "failed to atomically install LAN peer material `{}`",
            path.display()
        )
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_bundle() -> LanBootstrapBundleV1 {
        LanBootstrapBundleV1 {
            schema: LAN_BOOTSTRAP_SCHEMA_V1.to_owned(),
            node_id: "ostadix-test-node".to_owned(),
            server_name: "test-node.local".to_owned(),
            service_port: DEFAULT_LAN_NODE_PORT,
            security_mode: LAN_SECURITY_MODE.to_owned(),
            ca_pem: "-----BEGIN CERTIFICATE-----\na\n-----END CERTIFICATE-----\n".to_owned(),
            client_cert_pem:
                "-----BEGIN CERTIFICATE-----\nb\n-----END CERTIFICATE-----\n".to_owned(),
            client_key_pem:
                "-----BEGIN PRIVATE KEY-----\nc\n-----END PRIVATE KEY-----\n".to_owned(),
            node_receipt_public_key: Some("a".repeat(64)),
        }
    }

    #[test]
    fn bootstrap_bundle_validates_the_declared_lan_open_contract() {
        sample_bundle().validate().unwrap();
        let mut invalid = sample_bundle();
        invalid.security_mode = "manual".to_owned();
        assert!(invalid.validate().is_err());
    }

    #[test]
    fn stored_peer_round_trip_keeps_private_key_private() {
        let root = tempfile::tempdir().unwrap();
        let advertisement = LanNodeAdvertisementV1::new(
            "ostadix-test-node",
            "test-node.local",
            DEFAULT_LAN_NODE_PORT,
            DEFAULT_LAN_BOOTSTRAP_PORT,
            true,
        )
        .unwrap();
        let node = DiscoveredLanNodeV1 {
            advertisement,
            source_ip: "192.0.2.8".parse().unwrap(),
        };
        let (metadata, paths) = store_lan_peer(root.path(), &node, &sample_bundle()).unwrap();
        assert_eq!(metadata.address, "192.0.2.8:7337");
        let (loaded, _) = load_stored_lan_peer(root.path(), "ostadix-test-node").unwrap();
        assert_eq!(loaded, metadata);
        #[cfg(unix)]
        assert_eq!(
            fs::metadata(paths.client_key).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn physical_interfaces_probe_their_subnet_and_multicast_group() {
        let interface = LanIpv4Interface {
            address: "192.168.50.12".parse().unwrap(),
            broadcast: Some("192.168.50.255".parse().unwrap()),
        };
        let destinations = discovery_destinations(Some(interface), 7339);
        assert_eq!(
            destinations,
            vec![
                "192.168.50.255:7339".parse().unwrap(),
                "255.255.255.255:7339".parse().unwrap(),
                "239.255.73.37:7339".parse().unwrap(),
            ]
        );
    }

    #[test]
    fn loopback_discovery_stays_a_separate_compatibility_probe() {
        let interface = LanIpv4Interface {
            address: Ipv4Addr::LOCALHOST,
            broadcast: None,
        };
        assert_eq!(
            discovery_destinations(Some(interface), 7339),
            vec!["127.0.0.1:7339".parse().unwrap()]
        );
    }
}
