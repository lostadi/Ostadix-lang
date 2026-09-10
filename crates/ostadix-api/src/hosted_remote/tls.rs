//! TLS 1.3-only mutual-authentication helpers for hosted placement.

use std::fs::File;
use std::io::{BufReader, ErrorKind};
use std::net::{Shutdown, SocketAddr, TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc,
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName};
use rustls::server::WebPkiClientVerifier;
use rustls::{
    ClientConfig, ClientConnection, RootCertStore, ServerConfig, ServerConnection, StreamOwned,
};
use sha2::{Digest, Sha256};

pub const HOSTED_TLS_ALPN_V1: &[u8] = b"ostadix-hosted/1";
pub const HOSTED_TLS_ALPN_V2: &[u8] = b"ostadix-hosted/2";
pub const HOSTED_TLS_ALPN_MESH_V1: &[u8] = b"ostadix-mesh/1";
pub const EXECUTION_FABRIC_TLS_ALPN_V1: &[u8] = b"ostadix-execution-fabric/1";
pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
pub const DEFAULT_IO_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientTlsIdentity {
    pub ca_path: PathBuf,
    pub cert_path: PathBuf,
    pub key_path: PathBuf,
    pub server_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerTlsIdentity {
    pub client_ca_path: PathBuf,
    pub cert_path: PathBuf,
    pub key_path: PathBuf,
}

#[cfg(test)]
pub(crate) fn test_server_tls_identity() -> Result<(tempfile::TempDir, ServerTlsIdentity)> {
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("test-assets/hosted_tls");
    let directory = tempfile::tempdir()?;
    let key_path = directory.path().join("server-key.pem");
    let key_body = include_str!("../../test-assets/hosted_tls/server-key.pkcs8.b64");
    let key_label = "PRIVATE KEY";
    std::fs::write(
        &key_path,
        format!(
            "-----BEGIN {key_label}-----\n{}\n-----END {key_label}-----\n",
            key_body.trim()
        ),
    )?;
    let identity = ServerTlsIdentity {
        client_ca_path: fixture.join("ca.pem"),
        cert_path: fixture.join("server-cert.pem"),
        key_path,
    };
    Ok((directory, identity))
}

/// Test-only server configuration used to isolate client-side transport and
/// pinning behavior. The production builders below always require client
/// certificates; this fixture deliberately does not test that independent
/// server-side invariant.
#[cfg(test)]
pub(crate) fn test_server_config_without_client_auth_execution_fabric_v1(
    identity: &ServerTlsIdentity,
) -> Result<Arc<ServerConfig>> {
    let certificates = load_certificates(&identity.cert_path)?;
    let key = load_private_key(&identity.key_path)?;
    let provider = rustls::crypto::ring::default_provider();
    let mut config = ServerConfig::builder_with_provider(provider.into())
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .with_no_client_auth()
        .with_single_cert(certificates, key)?;
    config.max_early_data_size = 0;
    config.alpn_protocols = vec![EXECUTION_FABRIC_TLS_ALPN_V1.to_vec()];
    Ok(Arc::new(config))
}

pub type HostedClientStream = StreamOwned<ClientConnection, TcpStream>;
pub type HostedServerStream = StreamOwned<ServerConnection, TcpStream>;

/// Immutable client-side TLS identity for one Fabric configuration.
/// Certificate/key paths are resolved once so a Submit and its later Queries
/// cannot silently authenticate as different coordinator principals.
#[derive(Clone)]
pub(crate) struct ExecutionFabricClientTlsV1 {
    config: Arc<ClientConfig>,
    server_name: ServerName<'static>,
}

impl std::fmt::Debug for ExecutionFabricClientTlsV1 {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ExecutionFabricClientTlsV1")
            .field("server_name", &self.server_name)
            .field("credentials", &"[frozen]")
            .finish()
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum ExecutionFabricTlsConnectFailureV1 {
    #[error("execution-Fabric transport deadline expired")]
    Deadline,
    #[error("execution-Fabric TCP connection failed: {0:#}")]
    Connection(anyhow::Error),
    #[error("execution-Fabric local TLS/socket setup failed: {0:#}")]
    LocalSetup(anyhow::Error),
    #[error("execution-Fabric TLS peer authentication or ALPN failed: {0:#}")]
    PeerAuthentication(anyhow::Error),
    #[error("execution-Fabric protocol negotiation failed: {0:#}")]
    ProtocolNegotiation(anyhow::Error),
}

/// Owns the hard monotonic deadline for one execution-Fabric exchange.
///
/// Socket read/write timeouts are inactivity bounds and can be extended by a
/// drip-feeding peer. This guard independently shuts down the connection at
/// the absolute deadline, which interrupts handshake and frame I/O even when
/// individual operations continue to make partial progress.
#[must_use = "dropping the guard disables the execution-Fabric hard deadline"]
pub(crate) struct FabricTransportDeadlineGuardV1 {
    cancel: Option<mpsc::SyncSender<()>>,
    watchdog: Option<JoinHandle<()>>,
    expired: Arc<AtomicBool>,
}

impl FabricTransportDeadlineGuardV1 {
    fn arm(socket: &TcpStream, deadline: Instant) -> Result<Self> {
        let watched = socket
            .try_clone()
            .context("failed to clone execution-Fabric socket for deadline enforcement")?;
        let (cancel, cancelled) = mpsc::sync_channel(1);
        let expired = Arc::new(AtomicBool::new(false));
        let watchdog_expired = Arc::clone(&expired);
        let watchdog = thread::Builder::new()
            .name("ostadix-fabric-deadline-v1".to_string())
            .spawn(move || {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if matches!(
                    cancelled.recv_timeout(remaining),
                    Err(mpsc::RecvTimeoutError::Timeout)
                ) {
                    watchdog_expired.store(true, Ordering::SeqCst);
                    let _ = watched.shutdown(Shutdown::Both);
                }
            })
            .context("failed to create execution-Fabric deadline watchdog")?;
        Ok(Self {
            cancel: Some(cancel),
            watchdog: Some(watchdog),
            expired,
        })
    }

    pub(crate) fn expired(&self) -> bool {
        self.expired.load(Ordering::SeqCst)
    }
}

impl Drop for FabricTransportDeadlineGuardV1 {
    fn drop(&mut self) {
        if let Some(cancel) = self.cancel.take() {
            let _ = cancel.try_send(());
        }
        if let Some(watchdog) = self.watchdog.take() {
            let _ = watchdog.join();
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostedTlsProtocol {
    V1,
    V2,
    MeshV1,
}

/// Opt-in shared-listener route. Existing Hosted/Mesh acceptors retain their
/// frozen `HostedTlsProtocol` result and reject Fabric instead of falling back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostedTlsRouteV1 {
    Hosted(HostedTlsProtocol),
    ExecutionFabricV1,
}

pub fn build_client_config(identity: &ClientTlsIdentity) -> Result<Arc<ClientConfig>> {
    build_client_config_for_alpn(identity, HOSTED_TLS_ALPN_V1)
}

pub fn build_client_config_v2(identity: &ClientTlsIdentity) -> Result<Arc<ClientConfig>> {
    build_client_config_for_alpn(identity, HOSTED_TLS_ALPN_V2)
}

pub fn build_client_config_mesh_v1(identity: &ClientTlsIdentity) -> Result<Arc<ClientConfig>> {
    build_client_config_for_alpn(identity, HOSTED_TLS_ALPN_MESH_V1)
}

/// Build a client that advertises only the independently versioned execution
/// Fabric ALPN. Hosted and Mesh are never offered as fallback protocols.
pub fn build_client_config_execution_fabric_v1(
    identity: &ClientTlsIdentity,
) -> Result<Arc<ClientConfig>> {
    build_client_config_for_alpn(identity, EXECUTION_FABRIC_TLS_ALPN_V1)
}

pub(crate) fn prepare_execution_fabric_client_tls_v1(
    identity: &ClientTlsIdentity,
) -> Result<ExecutionFabricClientTlsV1> {
    let config = build_client_config_execution_fabric_v1(identity)?;
    let server_name = ServerName::try_from(identity.server_name.clone()).with_context(|| {
        format!(
            "invalid TLS server name `{}` (use a DNS name or IP SAN from the node certificate)",
            identity.server_name
        )
    })?;
    Ok(ExecutionFabricClientTlsV1 {
        config,
        server_name,
    })
}

fn build_client_config_for_alpn(
    identity: &ClientTlsIdentity,
    alpn: &[u8],
) -> Result<Arc<ClientConfig>> {
    let roots = load_roots(&identity.ca_path)
        .with_context(|| format!("failed to load server CA {}", identity.ca_path.display()))?;
    let certs = load_certificates(&identity.cert_path).with_context(|| {
        format!(
            "failed to load client certificate {}",
            identity.cert_path.display()
        )
    })?;
    let key = load_private_key(&identity.key_path).with_context(|| {
        format!(
            "failed to load client private key {}",
            identity.key_path.display()
        )
    })?;

    let provider = rustls::crypto::ring::default_provider();
    let mut config = ClientConfig::builder_with_provider(provider.into())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .context("failed to enable the TLS 1.3 protocol suite")?
        .with_root_certificates(roots)
        .with_client_auth_cert(certs, key)
        .context("client certificate and key do not form a usable TLS identity")?;
    config.enable_early_data = false;
    config.alpn_protocols = vec![alpn.to_vec()];
    Ok(Arc::new(config))
}

pub fn build_server_config(identity: &ServerTlsIdentity) -> Result<Arc<ServerConfig>> {
    build_server_config_for_alpns(identity, vec![HOSTED_TLS_ALPN_V1.to_vec()])
}

/// Opt in to Fabric beside frozen Hosted V1 without advertising V2 or Mesh.
pub fn build_server_config_with_execution_fabric_v1(
    identity: &ServerTlsIdentity,
) -> Result<Arc<ServerConfig>> {
    build_server_config_for_alpns(
        identity,
        vec![
            EXECUTION_FABRIC_TLS_ALPN_V1.to_vec(),
            HOSTED_TLS_ALPN_V1.to_vec(),
        ],
    )
}

pub fn build_dual_server_config(identity: &ServerTlsIdentity) -> Result<Arc<ServerConfig>> {
    build_server_config_for_alpns(
        identity,
        vec![HOSTED_TLS_ALPN_V2.to_vec(), HOSTED_TLS_ALPN_V1.to_vec()],
    )
}

/// Opt in to Fabric beside frozen Hosted V1/V2 without advertising Mesh.
pub fn build_dual_server_config_with_execution_fabric_v1(
    identity: &ServerTlsIdentity,
) -> Result<Arc<ServerConfig>> {
    build_server_config_for_alpns(
        identity,
        vec![
            EXECUTION_FABRIC_TLS_ALPN_V1.to_vec(),
            HOSTED_TLS_ALPN_V2.to_vec(),
            HOSTED_TLS_ALPN_V1.to_vec(),
        ],
    )
}

/// Build the existing V1/V2 listener configuration with the independently
/// versioned project-mesh data plane enabled on the same TLS port.
pub fn build_dual_server_config_with_mesh(
    identity: &ServerTlsIdentity,
) -> Result<Arc<ServerConfig>> {
    build_server_config_for_alpns(
        identity,
        vec![
            HOSTED_TLS_ALPN_MESH_V1.to_vec(),
            HOSTED_TLS_ALPN_V2.to_vec(),
            HOSTED_TLS_ALPN_V1.to_vec(),
        ],
    )
}

/// Opt in to Fabric on the shared TLS listener. Existing server builders do
/// not advertise this ALPN, so enabling a Fabric provider remains explicit.
pub fn build_dual_server_config_with_mesh_and_execution_fabric_v1(
    identity: &ServerTlsIdentity,
) -> Result<Arc<ServerConfig>> {
    build_server_config_for_alpns(
        identity,
        vec![
            EXECUTION_FABRIC_TLS_ALPN_V1.to_vec(),
            HOSTED_TLS_ALPN_MESH_V1.to_vec(),
            HOSTED_TLS_ALPN_V2.to_vec(),
            HOSTED_TLS_ALPN_V1.to_vec(),
        ],
    )
}

fn build_server_config_for_alpns(
    identity: &ServerTlsIdentity,
    alpns: Vec<Vec<u8>>,
) -> Result<Arc<ServerConfig>> {
    let client_roots = load_roots(&identity.client_ca_path).with_context(|| {
        format!(
            "failed to load client CA {}",
            identity.client_ca_path.display()
        )
    })?;
    let certs = load_certificates(&identity.cert_path).with_context(|| {
        format!(
            "failed to load server certificate {}",
            identity.cert_path.display()
        )
    })?;
    let key = load_private_key(&identity.key_path).with_context(|| {
        format!(
            "failed to load server private key {}",
            identity.key_path.display()
        )
    })?;

    let provider: Arc<rustls::crypto::CryptoProvider> =
        rustls::crypto::ring::default_provider().into();
    // No `allow_unauthenticated`: a valid client certificate is mandatory.
    let client_verifier =
        WebPkiClientVerifier::builder_with_provider(client_roots.into(), provider.clone())
            .build()
            .context("failed to construct mandatory client-certificate verifier")?;
    let mut config = ServerConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])
        .context("failed to enable the TLS 1.3 protocol suite")?
        .with_client_cert_verifier(client_verifier)
        .with_single_cert(certs, key)
        .context("server certificate and key do not form a usable TLS identity")?;
    config.max_early_data_size = 0;
    config.alpn_protocols = alpns;
    Ok(Arc::new(config))
}

pub fn connect_mutual_tls(
    address: &str,
    identity: &ClientTlsIdentity,
    connect_timeout: Duration,
    io_timeout: Duration,
) -> Result<HostedClientStream> {
    connect_mutual_tls_for_protocol(
        address,
        identity,
        connect_timeout,
        io_timeout,
        HostedTlsProtocol::V1,
    )
}

pub fn connect_mutual_tls_v2(
    address: &str,
    identity: &ClientTlsIdentity,
    connect_timeout: Duration,
    io_timeout: Duration,
) -> Result<HostedClientStream> {
    connect_mutual_tls_for_protocol(
        address,
        identity,
        connect_timeout,
        io_timeout,
        HostedTlsProtocol::V2,
    )
}

pub fn connect_mutual_tls_mesh_v1(
    address: &str,
    identity: &ClientTlsIdentity,
    connect_timeout: Duration,
    io_timeout: Duration,
) -> Result<HostedClientStream> {
    connect_mutual_tls_for_protocol(
        address,
        identity,
        connect_timeout,
        io_timeout,
        HostedTlsProtocol::MeshV1,
    )
}

/// Connect with the execution-Fabric ALPN as the sole application protocol.
/// A peer that selects no ALPN or any Hosted/Mesh ALPN is rejected; there is
/// no protocol sniffing or fallback after the TLS handshake.
///
/// Establish one exact-address Fabric connection under the caller's absolute
/// monotonic deadline. Unlike Hosted/Mesh helpers, this path performs no DNS
/// resolution and never tries a second address with a refreshed timeout.
pub(crate) fn connect_mutual_tls_execution_fabric_v1_until(
    address: SocketAddr,
    tls: &ExecutionFabricClientTlsV1,
    connect_timeout: Duration,
    io_timeout: Duration,
    deadline: Instant,
) -> std::result::Result<
    (HostedClientStream, FabricTransportDeadlineGuardV1),
    ExecutionFabricTlsConnectFailureV1,
> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err(ExecutionFabricTlsConnectFailureV1::Deadline);
    }
    let connect_deadline_limited = remaining <= connect_timeout;
    let connect_budget = connect_timeout.min(remaining);
    let mut tcp = match TcpStream::connect_timeout(&address, connect_budget) {
        Ok(tcp) => tcp,
        Err(error)
            if matches!(error.kind(), ErrorKind::TimedOut | ErrorKind::WouldBlock)
                && connect_deadline_limited
                && Instant::now() >= deadline =>
        {
            return Err(ExecutionFabricTlsConnectFailureV1::Deadline)
        }
        Err(error) => {
            return Err(ExecutionFabricTlsConnectFailureV1::Connection(
                anyhow::Error::new(error).context(format!(
                    "failed to connect to execution-Fabric node `{address}`"
                )),
            ))
        }
    };
    tcp.set_nodelay(true)
        .context("failed to enable TCP_NODELAY for execution-Fabric client")
        .map_err(ExecutionFabricTlsConnectFailureV1::LocalSetup)?;
    let deadline_guard = FabricTransportDeadlineGuardV1::arm(&tcp, deadline)
        .map_err(ExecutionFabricTlsConnectFailureV1::LocalSetup)?;
    let mut connection = ClientConnection::new(tls.config.clone(), tls.server_name.clone())
        .context("failed to initialize execution-Fabric TLS client")
        .map_err(ExecutionFabricTlsConnectFailureV1::LocalSetup)?;
    complete_client_handshake_until(
        &mut connection,
        &mut tcp,
        EXECUTION_FABRIC_TLS_ALPN_V1,
        "node did not negotiate the execution-Fabric V1 ALPN",
        deadline,
        connect_timeout,
        &deadline_guard,
    )?;
    let io_budget = remaining_timeout(deadline, io_timeout)
        .map_err(|_| ExecutionFabricTlsConnectFailureV1::Deadline)?;
    set_timeouts(&tcp, io_budget).map_err(ExecutionFabricTlsConnectFailureV1::LocalSetup)?;
    Ok((StreamOwned::new(connection, tcp), deadline_guard))
}

fn connect_mutual_tls_for_protocol(
    address: &str,
    identity: &ClientTlsIdentity,
    connect_timeout: Duration,
    io_timeout: Duration,
    protocol: HostedTlsProtocol,
) -> Result<HostedClientStream> {
    let (config, expected_alpn) = match protocol {
        HostedTlsProtocol::V1 => (build_client_config(identity)?, HOSTED_TLS_ALPN_V1),
        HostedTlsProtocol::V2 => (build_client_config_v2(identity)?, HOSTED_TLS_ALPN_V2),
        HostedTlsProtocol::MeshV1 => (
            build_client_config_mesh_v1(identity)?,
            HOSTED_TLS_ALPN_MESH_V1,
        ),
    };
    connect_mutual_tls_for_alpn(
        address,
        identity,
        connect_timeout,
        io_timeout,
        config,
        expected_alpn,
        "node did not negotiate the hosted-transport ALPN",
    )
}

fn connect_mutual_tls_for_alpn(
    address: &str,
    identity: &ClientTlsIdentity,
    connect_timeout: Duration,
    io_timeout: Duration,
    config: Arc<ClientConfig>,
    expected_alpn: &'static [u8],
    alpn_failure: &'static str,
) -> Result<HostedClientStream> {
    let server_name = ServerName::try_from(identity.server_name.clone()).with_context(|| {
        format!(
            "invalid TLS server name `{}` (use a DNS name or IP SAN from the node certificate)",
            identity.server_name
        )
    })?;
    let addresses = address
        .to_socket_addrs()
        .with_context(|| format!("failed to resolve node address `{address}`"))?
        .collect::<Vec<_>>();
    if addresses.is_empty() {
        bail!("node address `{address}` resolved to no socket addresses");
    }

    let mut failures = Vec::new();
    for resolved in addresses {
        let mut tcp = match TcpStream::connect_timeout(&resolved, connect_timeout) {
            Ok(stream) => stream,
            Err(error) => {
                failures.push(format!("{resolved}: {error}"));
                continue;
            }
        };
        tcp.set_nodelay(true)
            .context("failed to enable TCP_NODELAY for hosted client")?;
        set_timeouts(&tcp, connect_timeout)?;
        let mut connection = ClientConnection::new(config.clone(), server_name.clone())
            .context("failed to initialize hosted TLS client")?;
        if let Err(error) =
            complete_client_handshake(&mut connection, &mut tcp, expected_alpn, alpn_failure)
        {
            failures.push(format!("{resolved}: {error:#}"));
            continue;
        }
        set_timeouts(&tcp, io_timeout)?;
        return Ok(StreamOwned::new(connection, tcp));
    }
    bail!(
        "failed to establish mutually authenticated TLS with `{address}`: {}",
        failures.join("; ")
    )
}

pub fn accept_mutual_tls(
    tcp: TcpStream,
    config: Arc<ServerConfig>,
    handshake_timeout: Duration,
    io_timeout: Duration,
) -> Result<HostedServerStream> {
    let (stream, protocol) =
        accept_mutual_tls_versioned(tcp, config, handshake_timeout, io_timeout)?;
    if protocol != HostedTlsProtocol::V1 {
        bail!("client did not negotiate the hosted-transport V1 ALPN");
    }
    Ok(stream)
}

pub fn accept_mutual_tls_versioned(
    mut tcp: TcpStream,
    config: Arc<ServerConfig>,
    handshake_timeout: Duration,
    io_timeout: Duration,
) -> Result<(HostedServerStream, HostedTlsProtocol)> {
    tcp.set_nodelay(true)
        .context("failed to enable TCP_NODELAY for hosted node")?;
    set_timeouts(&tcp, handshake_timeout)?;
    let mut connection =
        ServerConnection::new(config).context("failed to initialize hosted TLS server")?;
    while connection.is_handshaking() {
        connection
            .complete_io(&mut tcp)
            .context("mutual TLS handshake failed")?;
    }
    let protocol = classify_hosted_tls_alpn(connection.alpn_protocol())?;
    set_timeouts(&tcp, io_timeout)?;
    Ok((StreamOwned::new(connection, tcp), protocol))
}

/// Accept a connection on a listener whose configuration explicitly opted in
/// to the Fabric ALPN, then return an exact route selected only from the TLS
/// negotiation result. No application bytes are consumed for protocol
/// detection and an absent or unknown ALPN is rejected.
pub fn accept_mutual_tls_with_execution_fabric_v1(
    mut tcp: TcpStream,
    config: Arc<ServerConfig>,
    handshake_timeout: Duration,
    io_timeout: Duration,
) -> Result<(HostedServerStream, HostedTlsRouteV1)> {
    tcp.set_nodelay(true)
        .context("failed to enable TCP_NODELAY for hosted node")?;
    set_timeouts(&tcp, handshake_timeout)?;
    let mut connection =
        ServerConnection::new(config).context("failed to initialize hosted TLS server")?;
    while connection.is_handshaking() {
        connection
            .complete_io(&mut tcp)
            .context("mutual TLS handshake failed")?;
    }
    let route = classify_hosted_or_fabric_tls_alpn_v1(connection.alpn_protocol())?;
    set_timeouts(&tcp, io_timeout)?;
    Ok((StreamOwned::new(connection, tcp), route))
}

fn classify_hosted_tls_alpn(protocol: Option<&[u8]>) -> Result<HostedTlsProtocol> {
    match protocol {
        Some(protocol) if protocol == HOSTED_TLS_ALPN_V1 => Ok(HostedTlsProtocol::V1),
        Some(protocol) if protocol == HOSTED_TLS_ALPN_V2 => Ok(HostedTlsProtocol::V2),
        Some(protocol) if protocol == HOSTED_TLS_ALPN_MESH_V1 => Ok(HostedTlsProtocol::MeshV1),
        _ => bail!("client did not negotiate a supported hosted-transport ALPN"),
    }
}

fn classify_hosted_or_fabric_tls_alpn_v1(protocol: Option<&[u8]>) -> Result<HostedTlsRouteV1> {
    match protocol {
        Some(protocol) if protocol == EXECUTION_FABRIC_TLS_ALPN_V1 => {
            Ok(HostedTlsRouteV1::ExecutionFabricV1)
        }
        protocol => classify_hosted_tls_alpn(protocol).map(HostedTlsRouteV1::Hosted),
    }
}

pub fn peer_principal_sha256(stream: &HostedServerStream) -> Result<String> {
    authenticated_peer_leaf_sha256(stream.conn.peer_certificates())
}

/// Return the authenticated server leaf-certificate fingerprint from a
/// completed mutually authenticated client connection.
///
/// This is sampled from Rustls handshake state, never from Fabric payload
/// metadata, so callers can compare it with an independently pinned provider
/// principal before accepting a candidate.
pub fn peer_server_principal_sha256(stream: &HostedClientStream) -> Result<String> {
    authenticated_peer_leaf_sha256(stream.conn.peer_certificates())
}

fn authenticated_peer_leaf_sha256(certificates: Option<&[CertificateDer<'_>]>) -> Result<String> {
    let certificate = certificates
        .and_then(|certificates| certificates.first())
        .context("mutually authenticated TLS stream has no peer leaf certificate")?;
    Ok(hex::encode(Sha256::digest(certificate.as_ref())))
}

pub fn certificate_leaf_sha256(path: impl AsRef<Path>) -> Result<String> {
    let certificates = load_certificates(path.as_ref()).with_context(|| {
        format!(
            "failed to load certificate fingerprint source {}",
            path.as_ref().display()
        )
    })?;
    Ok(hex::encode(Sha256::digest(certificates[0].as_ref())))
}

fn complete_client_handshake(
    connection: &mut ClientConnection,
    tcp: &mut TcpStream,
    expected_alpn: &[u8],
    alpn_failure: &'static str,
) -> Result<()> {
    while connection.is_handshaking() {
        connection
            .complete_io(tcp)
            .context("mutual TLS handshake failed")?;
    }
    if connection.alpn_protocol() != Some(expected_alpn) {
        bail!(alpn_failure);
    }
    Ok(())
}

fn complete_client_handshake_until(
    connection: &mut ClientConnection,
    tcp: &mut TcpStream,
    expected_alpn: &[u8],
    alpn_failure: &'static str,
    deadline: Instant,
    operation_timeout: Duration,
    deadline_guard: &FabricTransportDeadlineGuardV1,
) -> std::result::Result<(), ExecutionFabricTlsConnectFailureV1> {
    while connection.is_handshaking() {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(ExecutionFabricTlsConnectFailureV1::Deadline);
        }
        let deadline_limited = remaining <= operation_timeout;
        set_timeouts(tcp, operation_timeout.min(remaining))
            .map_err(ExecutionFabricTlsConnectFailureV1::LocalSetup)?;
        if let Err(error) = connection.complete_io(tcp) {
            let evidence = classify_client_handshake_io_error(&error);
            let timed_transport =
                matches!(error.kind(), ErrorKind::TimedOut | ErrorKind::WouldBlock);
            let error = anyhow::Error::new(error).context("mutual TLS handshake failed");
            return match evidence {
                ClientHandshakeFailureEvidenceV1::ProtocolNegotiation => Err(
                    ExecutionFabricTlsConnectFailureV1::ProtocolNegotiation(error),
                ),
                ClientHandshakeFailureEvidenceV1::PeerAuthentication => Err(
                    ExecutionFabricTlsConnectFailureV1::PeerAuthentication(error),
                ),
                ClientHandshakeFailureEvidenceV1::Transport
                    if deadline_guard.expired()
                        || (timed_transport && deadline_limited && Instant::now() >= deadline) =>
                {
                    Err(ExecutionFabricTlsConnectFailureV1::Deadline)
                }
                ClientHandshakeFailureEvidenceV1::Transport => {
                    Err(ExecutionFabricTlsConnectFailureV1::Connection(error))
                }
            };
        }
    }
    if connection.alpn_protocol() != Some(expected_alpn) {
        return Err(ExecutionFabricTlsConnectFailureV1::ProtocolNegotiation(
            anyhow::anyhow!(alpn_failure),
        ));
    }
    if Instant::now() >= deadline {
        return Err(ExecutionFabricTlsConnectFailureV1::Deadline);
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ClientHandshakeFailureEvidenceV1 {
    ProtocolNegotiation,
    PeerAuthentication,
    Transport,
}

fn classify_client_handshake_io_error(error: &std::io::Error) -> ClientHandshakeFailureEvidenceV1 {
    match error
        .get_ref()
        .and_then(|source| source.downcast_ref::<rustls::Error>())
    {
        Some(rustls::Error::PeerMisbehaved(
            rustls::PeerMisbehaved::SelectedUnofferedApplicationProtocol,
        )) => ClientHandshakeFailureEvidenceV1::ProtocolNegotiation,
        Some(_) => ClientHandshakeFailureEvidenceV1::PeerAuthentication,
        None if error.kind() == ErrorKind::InvalidData => {
            ClientHandshakeFailureEvidenceV1::PeerAuthentication
        }
        None => ClientHandshakeFailureEvidenceV1::Transport,
    }
}

fn remaining_timeout(deadline: Instant, configured: Duration) -> Result<Duration> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        bail!("execution-Fabric transport deadline expired");
    }
    Ok(configured.min(remaining))
}

fn set_timeouts(stream: &TcpStream, timeout: Duration) -> Result<()> {
    stream
        .set_read_timeout(Some(timeout))
        .context("failed to set hosted transport read timeout")?;
    stream
        .set_write_timeout(Some(timeout))
        .context("failed to set hosted transport write timeout")?;
    Ok(())
}

fn load_certificates(path: &Path) -> Result<Vec<CertificateDer<'static>>> {
    let file = File::open(path)?;
    let mut reader = BufReader::new(file);
    let certificates = rustls_pemfile::certs(&mut reader)
        .collect::<std::result::Result<Vec<_>, _>>()
        .context("certificate PEM is malformed")?;
    if certificates.is_empty() {
        bail!("certificate PEM contains no certificates");
    }
    Ok(certificates)
}

fn load_private_key(path: &Path) -> Result<PrivateKeyDer<'static>> {
    let file = File::open(path)?;
    let mut reader = BufReader::new(file);
    rustls_pemfile::private_key(&mut reader)
        .context("private-key PEM is malformed")?
        .context("private-key PEM contains no supported private key")
}

fn load_roots(path: &Path) -> Result<RootCertStore> {
    let mut roots = RootCertStore::empty();
    for certificate in load_certificates(path)? {
        roots
            .add(certificate)
            .context("CA PEM contains a certificate unusable as a trust anchor")?;
    }
    if roots.is_empty() {
        bail!("CA PEM contains no usable trust anchors");
    }
    Ok(roots)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_client_identity(server: &ServerTlsIdentity) -> ClientTlsIdentity {
        ClientTlsIdentity {
            ca_path: server.client_ca_path.clone(),
            cert_path: server.cert_path.clone(),
            key_path: server.key_path.clone(),
            server_name: "localhost".to_string(),
        }
    }

    #[test]
    fn fabric_client_advertises_only_the_exact_fabric_alpn() {
        let (_directory, server) = test_server_tls_identity().unwrap();
        let client = test_client_identity(&server);
        let config = build_client_config_execution_fabric_v1(&client).unwrap();
        assert_eq!(
            config.alpn_protocols,
            vec![EXECUTION_FABRIC_TLS_ALPN_V1.to_vec()]
        );
    }

    #[test]
    fn fabric_absolute_deadline_interrupts_a_stalled_tls_handshake() {
        let (_directory, server) = test_server_tls_identity().unwrap();
        let client = test_client_identity(&server);
        let tls = prepare_execution_fabric_client_tls_v1(&client).unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let stalled_peer = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let mut bytes = [0_u8; 4096];
            loop {
                match std::io::Read::read(&mut socket, &mut bytes) {
                    Ok(0) | Err(_) => return,
                    Ok(_) => {}
                }
            }
        });
        let started = Instant::now();
        let deadline = started.checked_add(Duration::from_millis(50)).unwrap();

        let result = connect_mutual_tls_execution_fabric_v1_until(
            address,
            &tls,
            Duration::from_secs(5),
            Duration::from_secs(5),
            deadline,
        );

        assert!(matches!(
            result,
            Err(ExecutionFabricTlsConnectFailureV1::Deadline)
        ));
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "deadline watchdog did not interrupt the stalled handshake promptly"
        );
        stalled_peer.join().unwrap();
    }

    #[test]
    fn concrete_tls_evidence_precedes_transport_deadline_classification() {
        let selected_unoffered = std::io::Error::new(
            ErrorKind::InvalidData,
            rustls::Error::PeerMisbehaved(
                rustls::PeerMisbehaved::SelectedUnofferedApplicationProtocol,
            ),
        );
        assert_eq!(
            classify_client_handshake_io_error(&selected_unoffered),
            ClientHandshakeFailureEvidenceV1::ProtocolNegotiation
        );

        let peer_authentication = std::io::Error::new(
            ErrorKind::InvalidData,
            rustls::Error::General("fixture certificate rejection".to_string()),
        );
        assert_eq!(
            classify_client_handshake_io_error(&peer_authentication),
            ClientHandshakeFailureEvidenceV1::PeerAuthentication
        );

        let raw_eof = std::io::Error::new(ErrorKind::UnexpectedEof, "watchdog socket shutdown");
        assert_eq!(
            classify_client_handshake_io_error(&raw_eof),
            ClientHandshakeFailureEvidenceV1::Transport
        );
    }

    #[test]
    fn existing_server_builders_do_not_opt_in_to_fabric() {
        let (_directory, identity) = test_server_tls_identity().unwrap();
        for config in [
            build_server_config(&identity).unwrap(),
            build_dual_server_config(&identity).unwrap(),
            build_dual_server_config_with_mesh(&identity).unwrap(),
        ] {
            assert!(!config
                .alpn_protocols
                .iter()
                .any(|alpn| alpn == EXECUTION_FABRIC_TLS_ALPN_V1));
        }
    }

    #[test]
    fn shared_server_builder_explicitly_offers_exact_fabric_alpn() {
        let (_directory, identity) = test_server_tls_identity().unwrap();
        let config = build_dual_server_config_with_mesh_and_execution_fabric_v1(&identity).unwrap();
        assert_eq!(
            config.alpn_protocols,
            vec![
                EXECUTION_FABRIC_TLS_ALPN_V1.to_vec(),
                HOSTED_TLS_ALPN_MESH_V1.to_vec(),
                HOSTED_TLS_ALPN_V2.to_vec(),
                HOSTED_TLS_ALPN_V1.to_vec(),
            ]
        );
    }

    #[test]
    fn shared_route_is_exact_and_existing_classifier_has_no_fabric_fallback() {
        assert!(classify_hosted_tls_alpn(Some(EXECUTION_FABRIC_TLS_ALPN_V1)).is_err());
        assert!(classify_hosted_tls_alpn(None).is_err());
        assert!(classify_hosted_tls_alpn(Some(b"ostadix-execution-fabric/2")).is_err());

        assert_eq!(
            classify_hosted_or_fabric_tls_alpn_v1(Some(EXECUTION_FABRIC_TLS_ALPN_V1)).unwrap(),
            HostedTlsRouteV1::ExecutionFabricV1
        );
        assert_eq!(
            classify_hosted_or_fabric_tls_alpn_v1(Some(HOSTED_TLS_ALPN_V1)).unwrap(),
            HostedTlsRouteV1::Hosted(HostedTlsProtocol::V1)
        );
        assert_eq!(
            classify_hosted_or_fabric_tls_alpn_v1(Some(HOSTED_TLS_ALPN_V2)).unwrap(),
            HostedTlsRouteV1::Hosted(HostedTlsProtocol::V2)
        );
        assert_eq!(
            classify_hosted_or_fabric_tls_alpn_v1(Some(HOSTED_TLS_ALPN_MESH_V1)).unwrap(),
            HostedTlsRouteV1::Hosted(HostedTlsProtocol::MeshV1)
        );
        assert!(classify_hosted_or_fabric_tls_alpn_v1(None).is_err());
        assert!(classify_hosted_or_fabric_tls_alpn_v1(Some(b"ostadix-hosted/3")).is_err());
    }

    #[test]
    fn authenticated_peer_leaf_fingerprint_uses_the_exact_first_certificate() {
        let (_directory, identity) = test_server_tls_identity().unwrap();
        let certificates = load_certificates(&identity.cert_path).unwrap();
        assert_eq!(
            authenticated_peer_leaf_sha256(Some(&certificates)).unwrap(),
            certificate_leaf_sha256(&identity.cert_path).unwrap()
        );
        assert!(authenticated_peer_leaf_sha256(None)
            .unwrap_err()
            .to_string()
            .contains("no peer leaf certificate"));
        assert!(authenticated_peer_leaf_sha256(Some(&[]))
            .unwrap_err()
            .to_string()
            .contains("no peer leaf certificate"));
    }
}
