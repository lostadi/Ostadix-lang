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
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};

use anyhow::{bail, Context, Result};
use if_addrs::IfAddr;
use serde::{Deserialize, Serialize};
use socket2::SockRef;

pub const LAN_DISCOVERY_SCHEMA_V1: &str = "ostadix.lan-discovery/v1";
pub const LAN_BOOTSTRAP_SCHEMA_V1: &str = "ostadix.lan-bootstrap/v1";
pub const LAN_PEER_SCHEMA_V1: &str = "ostadix.lan-peer/v1";
pub const LAN_SECURITY_MODE: &str = "lan-open";
pub const PAIRING_REQUIRED_SECURITY_MODE: &str = "pairing-required";
pub const PAIRED_SECURITY_MODE: &str = "paired-public-key";
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

    /// Advertise a node whose connection credentials are available only after
    /// an explicit pairing exchange. The bootstrap port is the pairing
    /// endpoint for this security mode; it must never serve a LAN-open bundle.
    pub fn pairing_required(
        node_id: impl Into<String>,
        server_name: impl Into<String>,
        service_port: u16,
        pairing_port: u16,
        supports_v2: bool,
    ) -> Result<Self> {
        let value = Self {
            schema: LAN_DISCOVERY_SCHEMA_V1.to_owned(),
            node_id: node_id.into(),
            server_name: server_name.into(),
            service_port,
            bootstrap_port: pairing_port,
            supports_v2,
            security_mode: PAIRING_REQUIRED_SECURITY_MODE.to_owned(),
        };
        value.validate()?;
        Ok(value)
    }

    pub fn is_pairing_required(&self) -> bool {
        self.security_mode == PAIRING_REQUIRED_SECURITY_MODE
    }

    pub fn validate(&self) -> Result<()> {
        if self.schema != LAN_DISCOVERY_SCHEMA_V1 {
            bail!(
                "unsupported Ostadix LAN advertisement schema `{}`",
                self.schema
            );
        }
        validate_lan_identifier("node_id", &self.node_id)?;
        validate_server_name(&self.server_name)?;
        if self.service_port == 0 || self.bootstrap_port == 0 {
            bail!("LAN service and bootstrap ports must be nonzero");
        }
        if !matches!(
            self.security_mode.as_str(),
            LAN_SECURITY_MODE | PAIRING_REQUIRED_SECURITY_MODE
        ) {
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
            bail!(
                "unsupported LAN bootstrap security mode `{}`",
                self.security_mode
            );
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
        if !matches!(
            self.security_mode.as_str(),
            LAN_SECURITY_MODE | PAIRED_SECURITY_MODE
        ) {
            bail!(
                "unsupported stored LAN peer security mode `{}`",
                self.security_mode
            );
        }
        Ok(())
    }

    pub fn is_paired(&self) -> bool {
        self.security_mode == PAIRED_SECURITY_MODE
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
        Ok(Self::for_directory(root.join(node_id)))
    }

    fn for_directory(directory: PathBuf) -> Self {
        Self {
            metadata: directory.join("peer.json"),
            ca: directory.join("ca.pem"),
            client_cert: directory.join("client-cert.pem"),
            client_key: directory.join("client-key.pem"),
            node_receipt_public_key: directory.join("node-signing-public.v2"),
            directory,
        }
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
        format!("failed to bind Ostadix LAN bootstrap service on {bind_address}")
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
    let mut stream = TcpStream::connect_timeout(&address, timeout)
        .with_context(|| format!("failed to enroll with discovered Ostadix node at {address}"))?;
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
    let _store_lock = lock_peer_store(peers_root, &bundle.node_id)?;
    let paths = StoredLanPeerPathsV1::for_root(peers_root, &bundle.node_id)?;
    if fs::symlink_metadata(&paths.directory).is_ok() {
        ensure_private_directory(&paths.directory)?;
        let existing = read_stored_peer_metadata(&paths, &bundle.node_id)?;
        if existing.is_paired() {
            bail!(
                "refusing to replace paired identity for node `{}` with LAN-open material",
                bundle.node_id
            );
        }
    }
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

/// Store a passcode-paired peer without ever accepting private material from
/// that peer. The client certificate is issued by the destination for this
/// machine's locally generated client key; the matching private key therefore
/// remains local throughout pairing.
///
/// An exact paired record is idempotent. Any conflicting paired record fails
/// closed instead of rotating a trust pin based on discovery. A legacy
/// `lan-open` record may be upgraded through a fully staged directory swap.
#[allow(clippy::too_many_arguments)]
pub fn store_paired_lan_peer(
    peers_root: &Path,
    peer_address: SocketAddr,
    node_id: &str,
    server_name: &str,
    service_port: u16,
    supports_v2: bool,
    remote_server_ca_pem: &str,
    destination_issued_local_client_cert_pem: &str,
    locally_generated_private_client_key: &[u8],
    remote_node_receipt_public_key: Option<&str>,
) -> Result<(StoredLanPeerV1, StoredLanPeerPathsV1)> {
    write_paired_lan_peer(
        PairedPeerWriteMode::Store,
        peers_root,
        peer_address,
        node_id,
        server_name,
        service_port,
        supports_v2,
        remote_server_ca_pem,
        destination_issued_local_client_cert_pem,
        locally_generated_private_client_key,
        remote_node_receipt_public_key,
    )
}

/// Explicitly replace an existing passcode-paired peer identity.
///
/// This is a recovery and renewal primitive, not a discovery refresh path. It
/// requires an existing paired record, retains exact-material idempotence, and
/// installs different authenticated material through a staged directory swap.
/// Callers must expose a deliberate operator action before selecting this API.
#[allow(clippy::too_many_arguments)]
pub fn replace_paired_lan_peer(
    peers_root: &Path,
    peer_address: SocketAddr,
    node_id: &str,
    server_name: &str,
    service_port: u16,
    supports_v2: bool,
    remote_server_ca_pem: &str,
    destination_issued_local_client_cert_pem: &str,
    locally_generated_private_client_key: &[u8],
    remote_node_receipt_public_key: Option<&str>,
) -> Result<(StoredLanPeerV1, StoredLanPeerPathsV1)> {
    write_paired_lan_peer(
        PairedPeerWriteMode::Replace,
        peers_root,
        peer_address,
        node_id,
        server_name,
        service_port,
        supports_v2,
        remote_server_ca_pem,
        destination_issued_local_client_cert_pem,
        locally_generated_private_client_key,
        remote_node_receipt_public_key,
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PairedPeerWriteMode {
    Store,
    Replace,
}

#[allow(clippy::too_many_arguments)]
fn write_paired_lan_peer(
    write_mode: PairedPeerWriteMode,
    peers_root: &Path,
    peer_address: SocketAddr,
    node_id: &str,
    server_name: &str,
    service_port: u16,
    supports_v2: bool,
    remote_server_ca_pem: &str,
    destination_issued_local_client_cert_pem: &str,
    locally_generated_private_client_key: &[u8],
    remote_node_receipt_public_key: Option<&str>,
) -> Result<(StoredLanPeerV1, StoredLanPeerPathsV1)> {
    validate_lan_identifier("node_id", node_id)?;
    validate_server_name(server_name)?;
    if service_port == 0 || peer_address.port() != service_port {
        bail!(
            "paired peer address port {} must match nonzero service_port {service_port}",
            peer_address.port()
        );
    }
    reject_private_key_material("remote server CA", remote_server_ca_pem)?;
    require_pem("remote server CA", remote_server_ca_pem, "CERTIFICATE")?;
    reject_private_key_material(
        "destination-issued local client certificate",
        destination_issued_local_client_cert_pem,
    )?;
    require_pem(
        "destination-issued local client certificate",
        destination_issued_local_client_cert_pem,
        "CERTIFICATE",
    )?;
    let local_private_key_pem = std::str::from_utf8(locally_generated_private_client_key)
        .context("locally generated client private key is not UTF-8 PEM")?;
    require_private_key_pem("locally generated client key", local_private_key_pem)?;
    if supports_v2 != remote_node_receipt_public_key.is_some() {
        bail!("paired peer supports_v2 must match presence of its receipt public key");
    }
    if let Some(public_key) = remote_node_receipt_public_key {
        validate_lower_hex("remote node receipt public key", public_key, 64)?;
    }

    let metadata = StoredLanPeerV1 {
        schema: LAN_PEER_SCHEMA_V1.to_owned(),
        node_id: node_id.to_owned(),
        server_name: server_name.to_owned(),
        address: peer_address.to_string(),
        service_port,
        security_mode: PAIRED_SECURITY_MODE.to_owned(),
        supports_v2,
    };
    metadata.validate()?;

    ensure_private_directory(peers_root)?;
    let _store_lock = lock_peer_store(peers_root, node_id)?;
    let paths = StoredLanPeerPathsV1::for_root(peers_root, node_id)?;
    let existing = match fs::symlink_metadata(&paths.directory) {
        Ok(file_type) => {
            if file_type.file_type().is_symlink() || !file_type.is_dir() {
                bail!(
                    "stored LAN peer path `{}` must be a real directory",
                    paths.directory.display()
                );
            }
            Some(load_stored_lan_peer(peers_root, node_id)?.0)
        }
        Err(error) if error.kind() == ErrorKind::NotFound => None,
        Err(error) => {
            return Err(error).with_context(|| {
                format!(
                    "failed to inspect stored LAN peer directory `{}`",
                    paths.directory.display()
                )
            })
        }
    };

    if let Some(existing) = existing.as_ref().filter(|peer| peer.is_paired()) {
        if paired_material_matches(
            &paths,
            existing,
            &metadata,
            remote_server_ca_pem.as_bytes(),
            destination_issued_local_client_cert_pem.as_bytes(),
            locally_generated_private_client_key,
            remote_node_receipt_public_key,
        )? {
            return Ok((existing.clone(), paths));
        }
        if write_mode == PairedPeerWriteMode::Store {
            bail!("refusing to replace conflicting paired identity for node `{node_id}`");
        }
    }

    if write_mode == PairedPeerWriteMode::Replace
        && !existing.as_ref().is_some_and(StoredLanPeerV1::is_paired)
    {
        bail!(
            "explicit paired replacement requires an existing paired record for node `{node_id}`"
        );
    }

    let staging = create_private_peer_staging_directory(peers_root, node_id)?;
    let staged_paths = StoredLanPeerPathsV1::for_directory(staging.clone());
    if let Err(error) = write_paired_peer_directory(
        &staged_paths,
        &metadata,
        remote_server_ca_pem.as_bytes(),
        destination_issued_local_client_cert_pem.as_bytes(),
        locally_generated_private_client_key,
        remote_node_receipt_public_key,
    ) {
        let _ = fs::remove_dir_all(&staging);
        return Err(error);
    }

    let install = match (write_mode, existing) {
        (PairedPeerWriteMode::Store, Some(existing))
            if existing.security_mode == LAN_SECURITY_MODE =>
        {
            install_paired_upgrade(&staging, &paths.directory, peers_root, node_id)
        }
        (PairedPeerWriteMode::Store, Some(existing)) => {
            let _ = fs::remove_dir_all(&staging);
            bail!(
                "stored peer `{node_id}` has unsupported upgrade mode `{}`",
                existing.security_mode
            );
        }
        (PairedPeerWriteMode::Store, None) => {
            fs::rename(&staging, &paths.directory).with_context(|| {
                format!(
                    "failed to atomically install paired peer directory `{}`",
                    paths.directory.display()
                )
            })
        }
        (PairedPeerWriteMode::Replace, Some(existing)) if existing.is_paired() => {
            install_paired_replacement(&staging, &paths.directory, peers_root, node_id)
        }
        (PairedPeerWriteMode::Replace, _) => {
            let _ = fs::remove_dir_all(&staging);
            unreachable!("paired replacement precondition was checked before staging")
        }
    };
    if let Err(error) = install {
        let _ = fs::remove_dir_all(&staging);
        return Err(error);
    }
    Ok((metadata, paths))
}

fn paired_material_matches(
    paths: &StoredLanPeerPathsV1,
    existing: &StoredLanPeerV1,
    expected: &StoredLanPeerV1,
    remote_server_ca: &[u8],
    local_client_cert: &[u8],
    local_client_key: &[u8],
    remote_receipt_public_key: Option<&str>,
) -> Result<bool> {
    if existing != expected {
        return Ok(false);
    }
    if read_regular_file(&paths.ca, "paired peer server CA")? != remote_server_ca
        || read_regular_file(&paths.client_cert, "paired peer client certificate")?
            != local_client_cert
        || read_regular_file(&paths.client_key, "paired peer local client key")? != local_client_key
    {
        return Ok(false);
    }
    match remote_receipt_public_key {
        Some(public_key) => {
            let mut expected = public_key.as_bytes().to_vec();
            expected.push(b'\n');
            Ok(read_regular_file(
                &paths.node_receipt_public_key,
                "paired peer receipt public key",
            )? == expected)
        }
        None => match fs::symlink_metadata(&paths.node_receipt_public_key) {
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(true),
            Ok(_) => Ok(false),
            Err(error) => Err(error).with_context(|| {
                format!(
                    "failed to inspect paired peer receipt key `{}`",
                    paths.node_receipt_public_key.display()
                )
            }),
        },
    }
}

fn write_paired_peer_directory(
    paths: &StoredLanPeerPathsV1,
    metadata: &StoredLanPeerV1,
    remote_server_ca: &[u8],
    local_client_cert: &[u8],
    local_client_key: &[u8],
    remote_receipt_public_key: Option<&str>,
) -> Result<()> {
    ensure_private_directory(&paths.directory)?;
    write_atomic(&paths.ca, remote_server_ca, false)?;
    write_atomic(&paths.client_cert, local_client_cert, false)?;
    write_atomic(&paths.client_key, local_client_key, true)?;
    if let Some(public_key) = remote_receipt_public_key {
        let mut encoded = public_key.as_bytes().to_vec();
        encoded.push(b'\n');
        write_atomic(&paths.node_receipt_public_key, &encoded, false)?;
    }
    let mut encoded = serde_json::to_vec_pretty(metadata)?;
    encoded.push(b'\n');
    write_atomic(&paths.metadata, &encoded, false)
}

fn lock_peer_store(peers_root: &Path, node_id: &str) -> Result<fs::File> {
    let lock_path = peers_root.join(format!(".{node_id}.store.lock"));
    match fs::symlink_metadata(&lock_path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            bail!(
                "LAN peer store lock `{}` must be a regular, non-symlink file",
                lock_path.display()
            )
        }
        Ok(_) => {}
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error).with_context(|| {
                format!(
                    "failed to inspect LAN peer store lock `{}`",
                    lock_path.display()
                )
            })
        }
    }
    let mut options = fs::OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    options.mode(0o600);
    let lock = options.open(&lock_path).with_context(|| {
        format!(
            "failed to open LAN peer store lock `{}`",
            lock_path.display()
        )
    })?;
    #[cfg(unix)]
    fs::set_permissions(&lock_path, fs::Permissions::from_mode(0o600))?;
    fs2::FileExt::lock_exclusive(&lock)
        .with_context(|| format!("failed to lock LAN peer store `{}`", lock_path.display()))?;
    Ok(lock)
}

fn create_private_peer_staging_directory(peers_root: &Path, node_id: &str) -> Result<PathBuf> {
    for _ in 0..32 {
        let mut random = [0_u8; 8];
        getrandom::fill(&mut random).context("failed to obtain entropy for paired peer staging")?;
        let staging = peers_root.join(format!(".{node_id}.pairing-stage-{}", hex::encode(random)));
        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)]
        builder.mode(0o700);
        match builder.create(&staging) {
            Ok(()) => return Ok(staging),
            Err(error) if error.kind() == ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "failed to create paired peer staging directory `{}`",
                        staging.display()
                    )
                })
            }
        }
    }
    bail!("failed to allocate paired peer staging directory after 32 attempts")
}

fn install_paired_upgrade(
    staging: &Path,
    destination: &Path,
    peers_root: &Path,
    node_id: &str,
) -> Result<()> {
    let backup = unique_peer_backup_path(peers_root, node_id, "lan-open-backup")?;
    fs::rename(destination, &backup).with_context(|| {
        format!(
            "failed to preserve legacy LAN-open peer `{}` before pairing upgrade",
            destination.display()
        )
    })?;
    if let Err(install_error) = fs::rename(staging, destination) {
        if let Err(rollback_error) = fs::rename(&backup, destination) {
            bail!(
                "paired peer install failed ({install_error}); rollback also failed ({rollback_error})"
            );
        }
        return Err(install_error).with_context(|| {
            format!(
                "failed to atomically replace legacy LAN-open peer `{}`",
                destination.display()
            )
        });
    }

    // The legacy shared key is removed only after the paired directory is
    // installed. Removing it separately ensures a later cleanup failure cannot
    // leave usable private material in the retired directory.
    let legacy_shared_key = backup.join("client-key.pem");
    if let Err(remove_error) = fs::remove_file(&legacy_shared_key) {
        match fs::rename(destination, staging) {
            Ok(()) => match fs::rename(&backup, destination) {
                Ok(()) => {
                    let _ = fs::remove_dir_all(staging);
                    return Err(remove_error).with_context(|| {
                        format!(
                            "paired upgrade rolled back because legacy shared key `{}` could not be removed",
                            legacy_shared_key.display()
                        )
                    });
                }
                Err(rollback_error) => {
                    let reinstall_error = fs::rename(staging, destination).err();
                    bail!(
                        "paired peer install lost its legacy rollback after shared-key removal failed ({remove_error}); restoring legacy failed ({rollback_error}); restoring paired state returned {reinstall_error:?}"
                    );
                }
            },
            Err(move_new_error) => {
                bail!(
                    "paired peer was installed but legacy shared-key removal failed ({remove_error}) and the paired directory could not be moved for rollback ({move_new_error})"
                );
            }
        }
    }
    fs::remove_dir_all(&backup).with_context(|| {
        format!(
            "paired peer was installed but retired LAN-open directory `{}` could not be removed",
            backup.display()
        )
    })?;
    Ok(())
}

fn install_paired_replacement(
    staging: &Path,
    destination: &Path,
    peers_root: &Path,
    node_id: &str,
) -> Result<()> {
    let backup = unique_peer_backup_path(peers_root, node_id, "paired-replacement-backup")?;
    fs::rename(destination, &backup).with_context(|| {
        format!(
            "failed to archive paired peer `{}` before explicit replacement",
            destination.display()
        )
    })?;
    if let Err(install_error) = fs::rename(staging, destination) {
        if let Err(rollback_error) = fs::rename(&backup, destination) {
            bail!(
                "explicit paired replacement failed ({install_error}); rollback also failed ({rollback_error})"
            );
        }
        return Err(install_error).with_context(|| {
            format!(
                "failed to atomically replace paired peer `{}`; the original record was restored",
                destination.display()
            )
        });
    }

    // Retire the old locally held client key before deleting the remaining
    // archived public material. If that first unlink fails, the old directory
    // is still complete, so the directory swap can be rolled back safely.
    let retired_client_key = backup.join("client-key.pem");
    if let Err(remove_error) = fs::remove_file(&retired_client_key) {
        match fs::rename(destination, staging) {
            Ok(()) => match fs::rename(&backup, destination) {
                Ok(()) => {
                    let _ = fs::remove_dir_all(staging);
                    return Err(remove_error).with_context(|| {
                        format!(
                            "explicit paired replacement rolled back because retired client key `{}` could not be removed",
                            retired_client_key.display()
                        )
                    });
                }
                Err(rollback_error) => {
                    let reinstall_error = fs::rename(staging, destination).err();
                    bail!(
                        "explicit paired replacement lost its rollback after retired-key removal failed ({remove_error}); restoring the original failed ({rollback_error}); restoring replacement state returned {reinstall_error:?}"
                    );
                }
            },
            Err(move_new_error) => {
                bail!(
                    "paired peer was explicitly replaced but retired-key removal failed ({remove_error}) and replacement state could not be moved for rollback ({move_new_error})"
                );
            }
        }
    }

    fs::remove_dir_all(&backup).with_context(|| {
        format!(
            "paired peer was explicitly replaced and its retired private key removed, but archived public identity `{}` could not be removed",
            backup.display()
        )
    })?;
    Ok(())
}

fn unique_peer_backup_path(peers_root: &Path, node_id: &str, label: &str) -> Result<PathBuf> {
    for _ in 0..32 {
        let mut random = [0_u8; 8];
        getrandom::fill(&mut random)
            .context("failed to obtain entropy for peer identity backup")?;
        let backup = peers_root.join(format!(".{node_id}.{label}-{}", hex::encode(random)));
        match fs::symlink_metadata(&backup) {
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(backup),
            Ok(_) => continue,
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "failed to inspect peer identity backup `{}`",
                        backup.display()
                    )
                })
            }
        }
    }
    bail!("failed to allocate peer identity backup path after 32 attempts")
}

pub fn load_stored_lan_peer(
    peers_root: &Path,
    node_id: &str,
) -> Result<(StoredLanPeerV1, StoredLanPeerPathsV1)> {
    let paths = StoredLanPeerPathsV1::for_root(peers_root, node_id)?;
    require_real_directory(&paths.directory, "stored LAN peer")?;
    let metadata = read_stored_peer_metadata(&paths, node_id)?;
    for required in [&paths.ca, &paths.client_cert, &paths.client_key] {
        read_regular_file(required, "stored LAN peer material").with_context(|| {
            format!(
                "stored LAN peer `{node_id}` is incomplete at `{}`",
                required.display()
            )
        })?;
    }
    Ok((metadata, paths))
}

fn read_stored_peer_metadata(
    paths: &StoredLanPeerPathsV1,
    node_id: &str,
) -> Result<StoredLanPeerV1> {
    let metadata: StoredLanPeerV1 = serde_json::from_slice(&read_regular_file(
        &paths.metadata,
        "stored LAN peer metadata",
    )?)
    .with_context(|| format!("failed to decode stored LAN peer `{node_id}` metadata"))?;
    metadata.validate()?;
    Ok(metadata)
}

pub fn list_stored_lan_peers(
    peers_root: &Path,
) -> Result<Vec<(StoredLanPeerV1, StoredLanPeerPathsV1)>> {
    match fs::symlink_metadata(peers_root) {
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            bail!(
                "LAN peer root `{}` must be a real directory",
                peers_root.display()
            )
        }
        Ok(_) => {}
        Err(error) => {
            return Err(error).with_context(|| {
                format!("failed to inspect LAN peer root `{}`", peers_root.display())
            })
        }
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

fn reject_private_key_material(label: &str, value: &str) -> Result<()> {
    if value.lines().any(|line| {
        let line = line.trim();
        line.starts_with("-----BEGIN ") && line.ends_with("-----") && line.contains("PRIVATE KEY")
    }) {
        bail!("paired {label} must not contain private-key material");
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

fn require_real_directory(path: &Path, label: &str) -> Result<()> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("failed to inspect {label} `{}`", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!("{label} `{}` must be a real directory", path.display());
    }
    Ok(())
}

fn read_regular_file(path: &Path, label: &str) -> Result<Vec<u8>> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("failed to inspect {label} `{}`", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!(
            "{label} `{}` must be a regular, non-symlink file",
            path.display()
        );
    }
    fs::read(path).with_context(|| format!("failed to read {label} `{}`", path.display()))
}

fn ensure_private_directory(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                bail!("`{}` must be a real directory", path.display());
            }
            #[cfg(unix)]
            fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
            return Ok(());
        }
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error).with_context(|| format!("failed to inspect `{}`", path.display()))
        }
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

    const SAMPLE_REMOTE_CA: &str =
        "-----BEGIN CERTIFICATE-----\nremote-ca\n-----END CERTIFICATE-----\n";
    const SAMPLE_LOCAL_CLIENT_CERT: &str =
        "-----BEGIN CERTIFICATE-----\nlocal-client\n-----END CERTIFICATE-----\n";
    const SAMPLE_LOCAL_CLIENT_KEY: &[u8] =
        b"-----BEGIN PRIVATE KEY-----\nlocal-only\n-----END PRIVATE KEY-----\n";
    const REPLACEMENT_REMOTE_CA: &str =
        "-----BEGIN CERTIFICATE-----\nreplacement-ca\n-----END CERTIFICATE-----\n";
    const REPLACEMENT_LOCAL_CLIENT_CERT: &str =
        "-----BEGIN CERTIFICATE-----\nreplacement-client\n-----END CERTIFICATE-----\n";
    const REPLACEMENT_LOCAL_CLIENT_KEY: &[u8] =
        b"-----BEGIN PRIVATE KEY-----\nreplacement-local-only\n-----END PRIVATE KEY-----\n";

    fn sample_bundle() -> LanBootstrapBundleV1 {
        LanBootstrapBundleV1 {
            schema: LAN_BOOTSTRAP_SCHEMA_V1.to_owned(),
            node_id: "ostadix-test-node".to_owned(),
            server_name: "test-node.local".to_owned(),
            service_port: DEFAULT_LAN_NODE_PORT,
            security_mode: LAN_SECURITY_MODE.to_owned(),
            ca_pem: "-----BEGIN CERTIFICATE-----\na\n-----END CERTIFICATE-----\n".to_owned(),
            client_cert_pem: "-----BEGIN CERTIFICATE-----\nb\n-----END CERTIFICATE-----\n"
                .to_owned(),
            client_key_pem: "-----BEGIN PRIVATE KEY-----\nc\n-----END PRIVATE KEY-----\n"
                .to_owned(),
            node_receipt_public_key: Some("a".repeat(64)),
        }
    }

    fn store_sample_paired(root: &Path) -> Result<(StoredLanPeerV1, StoredLanPeerPathsV1)> {
        store_paired_lan_peer(
            root,
            "192.0.2.8:7337".parse().unwrap(),
            "ostadix-test-node",
            "test-node.local",
            DEFAULT_LAN_NODE_PORT,
            true,
            SAMPLE_REMOTE_CA,
            SAMPLE_LOCAL_CLIENT_CERT,
            SAMPLE_LOCAL_CLIENT_KEY,
            Some(&"a".repeat(64)),
        )
    }

    #[test]
    fn advertisement_and_stored_peer_security_modes_are_distinct() {
        let legacy = LanNodeAdvertisementV1::new(
            "ostadix-test-node",
            "test-node.local",
            DEFAULT_LAN_NODE_PORT,
            DEFAULT_LAN_BOOTSTRAP_PORT,
            true,
        )
        .unwrap();
        assert_eq!(legacy.security_mode, LAN_SECURITY_MODE);
        assert!(!legacy.is_pairing_required());

        let pairing = LanNodeAdvertisementV1::pairing_required(
            "ostadix-test-node",
            "test-node.local",
            DEFAULT_LAN_NODE_PORT,
            DEFAULT_LAN_BOOTSTRAP_PORT,
            true,
        )
        .unwrap();
        assert!(pairing.is_pairing_required());
        let mut invalid_advertisement = pairing;
        invalid_advertisement.security_mode = PAIRED_SECURITY_MODE.to_owned();
        assert!(invalid_advertisement.validate().is_err());

        let mut stored = StoredLanPeerV1 {
            schema: LAN_PEER_SCHEMA_V1.to_owned(),
            node_id: "ostadix-test-node".to_owned(),
            server_name: "test-node.local".to_owned(),
            address: "192.0.2.8:7337".to_owned(),
            service_port: DEFAULT_LAN_NODE_PORT,
            security_mode: LAN_SECURITY_MODE.to_owned(),
            supports_v2: true,
        };
        stored.validate().unwrap();
        assert!(!stored.is_paired());
        stored.security_mode = PAIRED_SECURITY_MODE.to_owned();
        stored.validate().unwrap();
        assert!(stored.is_paired());
        stored.security_mode = PAIRING_REQUIRED_SECURITY_MODE.to_owned();
        assert!(stored.validate().is_err());
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
    fn paired_peer_storage_is_private_and_exactly_idempotent() {
        let root = tempfile::tempdir().unwrap();
        let (metadata, paths) = store_sample_paired(root.path()).unwrap();
        assert!(metadata.is_paired());
        assert_eq!(metadata.address, "192.0.2.8:7337");
        assert_eq!(
            fs::read(&paths.client_key).unwrap(),
            SAMPLE_LOCAL_CLIENT_KEY
        );

        let (again, again_paths) = store_sample_paired(root.path()).unwrap();
        assert_eq!(again, metadata);
        assert_eq!(again_paths.directory, paths.directory);

        for public_path in [
            &paths.metadata,
            &paths.ca,
            &paths.client_cert,
            &paths.node_receipt_public_key,
        ] {
            let public = fs::read_to_string(public_path).unwrap();
            assert!(!public.contains("PRIVATE KEY"), "{}", public_path.display());
        }
        #[cfg(unix)]
        {
            assert_eq!(
                fs::metadata(root.path()).unwrap().permissions().mode() & 0o777,
                0o700
            );
            assert_eq!(
                fs::metadata(&paths.directory).unwrap().permissions().mode() & 0o777,
                0o700
            );
            assert_eq!(
                fs::metadata(&paths.client_key)
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn ordinary_conflict_rejects_but_explicit_paired_replacement_succeeds() {
        let root = tempfile::tempdir().unwrap();
        let (_, paths) = store_sample_paired(root.path()).unwrap();
        let original_ca = fs::read(&paths.ca).unwrap();
        let error = store_paired_lan_peer(
            root.path(),
            "192.0.2.8:7337".parse().unwrap(),
            "ostadix-test-node",
            "test-node.local",
            DEFAULT_LAN_NODE_PORT,
            true,
            REPLACEMENT_REMOTE_CA,
            REPLACEMENT_LOCAL_CLIENT_CERT,
            REPLACEMENT_LOCAL_CLIENT_KEY,
            Some(&"b".repeat(64)),
        )
        .unwrap_err();
        assert!(error
            .to_string()
            .contains("refusing to replace conflicting"));
        assert_eq!(fs::read(&paths.ca).unwrap(), original_ca);

        let (replaced, replaced_paths) = replace_paired_lan_peer(
            root.path(),
            "192.0.2.9:7337".parse().unwrap(),
            "ostadix-test-node",
            "replacement-node.local",
            DEFAULT_LAN_NODE_PORT,
            true,
            REPLACEMENT_REMOTE_CA,
            REPLACEMENT_LOCAL_CLIENT_CERT,
            REPLACEMENT_LOCAL_CLIENT_KEY,
            Some(&"b".repeat(64)),
        )
        .unwrap();
        assert_eq!(replaced.address, "192.0.2.9:7337");
        assert_eq!(replaced.server_name, "replacement-node.local");
        assert_eq!(
            fs::read(&replaced_paths.ca).unwrap(),
            REPLACEMENT_REMOTE_CA.as_bytes()
        );
        assert_eq!(
            fs::read(&replaced_paths.client_cert).unwrap(),
            REPLACEMENT_LOCAL_CLIENT_CERT.as_bytes()
        );
        assert_eq!(
            fs::read(&replaced_paths.client_key).unwrap(),
            REPLACEMENT_LOCAL_CLIENT_KEY
        );
        assert_eq!(
            fs::read_to_string(&replaced_paths.node_receipt_public_key).unwrap(),
            format!("{}\n", "b".repeat(64))
        );
        for public_path in [
            &replaced_paths.metadata,
            &replaced_paths.ca,
            &replaced_paths.client_cert,
            &replaced_paths.node_receipt_public_key,
        ] {
            assert!(!fs::read_to_string(public_path)
                .unwrap()
                .contains("PRIVATE KEY"));
        }
        #[cfg(unix)]
        {
            assert_eq!(
                fs::metadata(&replaced_paths.directory)
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
            assert_eq!(
                fs::metadata(&replaced_paths.client_key)
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }

        let (again, again_paths) = replace_paired_lan_peer(
            root.path(),
            "192.0.2.9:7337".parse().unwrap(),
            "ostadix-test-node",
            "replacement-node.local",
            DEFAULT_LAN_NODE_PORT,
            true,
            REPLACEMENT_REMOTE_CA,
            REPLACEMENT_LOCAL_CLIENT_CERT,
            REPLACEMENT_LOCAL_CLIENT_KEY,
            Some(&"b".repeat(64)),
        )
        .unwrap();
        assert_eq!(again, replaced);
        assert_eq!(again_paths.directory, replaced_paths.directory);

        let hidden_leftovers = fs::read_dir(root.path())
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter_map(|entry| entry.file_name().to_str().map(str::to_owned))
            .filter(|name| {
                name.contains("pairing-stage") || name.contains("paired-replacement-backup")
            })
            .collect::<Vec<_>>();
        assert!(hidden_leftovers.is_empty(), "{hidden_leftovers:?}");
    }

    #[test]
    fn explicit_paired_replacement_requires_an_existing_paired_record() {
        let empty_root = tempfile::tempdir().unwrap();
        let missing_error = replace_paired_lan_peer(
            empty_root.path(),
            "192.0.2.9:7337".parse().unwrap(),
            "ostadix-test-node",
            "replacement-node.local",
            DEFAULT_LAN_NODE_PORT,
            true,
            REPLACEMENT_REMOTE_CA,
            REPLACEMENT_LOCAL_CLIENT_CERT,
            REPLACEMENT_LOCAL_CLIENT_KEY,
            Some(&"b".repeat(64)),
        )
        .unwrap_err();
        assert!(missing_error
            .to_string()
            .contains("requires an existing paired record"));
        assert!(!empty_root.path().join("ostadix-test-node").exists());

        let legacy_root = tempfile::tempdir().unwrap();
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
        let (_, legacy_paths) =
            store_lan_peer(legacy_root.path(), &node, &sample_bundle()).unwrap();
        let legacy_key = fs::read(&legacy_paths.client_key).unwrap();
        let legacy_error = replace_paired_lan_peer(
            legacy_root.path(),
            "192.0.2.9:7337".parse().unwrap(),
            "ostadix-test-node",
            "replacement-node.local",
            DEFAULT_LAN_NODE_PORT,
            true,
            REPLACEMENT_REMOTE_CA,
            REPLACEMENT_LOCAL_CLIENT_CERT,
            REPLACEMENT_LOCAL_CLIENT_KEY,
            Some(&"b".repeat(64)),
        )
        .unwrap_err();
        assert!(legacy_error
            .to_string()
            .contains("requires an existing paired record"));
        assert_eq!(fs::read(legacy_paths.client_key).unwrap(), legacy_key);
    }

    #[test]
    fn explicit_paired_replacement_restores_the_original_on_install_failure() {
        let root = tempfile::tempdir().unwrap();
        let (original, paths) = store_sample_paired(root.path()).unwrap();
        let original_key = fs::read(&paths.client_key).unwrap();
        let missing_stage = root.path().join("missing-pairing-stage");

        let error = install_paired_replacement(
            &missing_stage,
            &paths.directory,
            root.path(),
            "ostadix-test-node",
        )
        .unwrap_err();
        assert!(error.to_string().contains("original record was restored"));
        let (restored, restored_paths) =
            load_stored_lan_peer(root.path(), "ostadix-test-node").unwrap();
        assert_eq!(restored, original);
        assert_eq!(fs::read(restored_paths.client_key).unwrap(), original_key);
        let hidden_leftovers = fs::read_dir(root.path())
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter_map(|entry| entry.file_name().to_str().map(str::to_owned))
            .filter(|name| name.contains("paired-replacement-backup"))
            .collect::<Vec<_>>();
        assert!(hidden_leftovers.is_empty(), "{hidden_leftovers:?}");
    }

    #[test]
    fn legacy_bootstrap_cannot_downgrade_a_paired_peer() {
        let root = tempfile::tempdir().unwrap();
        let (_, paths) = store_sample_paired(root.path()).unwrap();
        let paired_key = fs::read(&paths.client_key).unwrap();
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
        let error = store_lan_peer(root.path(), &node, &sample_bundle()).unwrap_err();
        assert!(error.to_string().contains("refusing to replace paired"));
        assert_eq!(fs::read(paths.client_key).unwrap(), paired_key);
    }

    #[test]
    fn paired_public_inputs_reject_private_key_material() {
        let root = tempfile::tempdir().unwrap();
        let public_with_private = format!(
            "{SAMPLE_REMOTE_CA}{}",
            String::from_utf8_lossy(SAMPLE_LOCAL_CLIENT_KEY)
        );
        let error = store_paired_lan_peer(
            root.path(),
            "192.0.2.8:7337".parse().unwrap(),
            "ostadix-test-node",
            "test-node.local",
            DEFAULT_LAN_NODE_PORT,
            false,
            &public_with_private,
            SAMPLE_LOCAL_CLIENT_CERT,
            SAMPLE_LOCAL_CLIENT_KEY,
            None,
        )
        .unwrap_err();
        assert!(error
            .to_string()
            .contains("must not contain private-key material"));
        assert!(!root.path().join("ostadix-test-node").exists());
    }

    #[test]
    fn lan_open_peer_upgrades_only_after_a_complete_paired_stage_exists() {
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
        let (_, legacy_paths) = store_lan_peer(root.path(), &node, &sample_bundle()).unwrap();
        assert_ne!(
            fs::read(&legacy_paths.client_key).unwrap(),
            SAMPLE_LOCAL_CLIENT_KEY
        );

        let (paired, paired_paths) = store_sample_paired(root.path()).unwrap();
        assert!(paired.is_paired());
        assert_eq!(
            fs::read(&paired_paths.client_key).unwrap(),
            SAMPLE_LOCAL_CLIENT_KEY
        );
        let hidden_leftovers = fs::read_dir(root.path())
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter_map(|entry| entry.file_name().to_str().map(str::to_owned))
            .filter(|name| name.contains("pairing-stage") || name.contains("lan-open-backup"))
            .collect::<Vec<_>>();
        assert!(hidden_leftovers.is_empty(), "{hidden_leftovers:?}");
    }

    #[cfg(unix)]
    #[test]
    fn paired_storage_rejects_a_symlinked_private_root() {
        use std::os::unix::fs::symlink;

        let temporary = tempfile::tempdir().unwrap();
        let actual = temporary.path().join("actual");
        fs::create_dir(&actual).unwrap();
        let linked = temporary.path().join("linked");
        symlink(&actual, &linked).unwrap();
        assert!(store_sample_paired(&linked).is_err());
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
