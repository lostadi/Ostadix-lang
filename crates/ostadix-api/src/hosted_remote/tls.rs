//! TLS 1.3-only mutual-authentication helpers for hosted placement.

use std::fs::File;
use std::io::BufReader;
use std::net::{TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName};
use rustls::server::WebPkiClientVerifier;
use rustls::{
    ClientConfig, ClientConnection, RootCertStore, ServerConfig, ServerConnection, StreamOwned,
};
use sha2::{Digest, Sha256};

pub const HOSTED_TLS_ALPN_V1: &[u8] = b"ostadix-hosted/1";
pub const HOSTED_TLS_ALPN_V2: &[u8] = b"ostadix-hosted/2";
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

pub type HostedClientStream = StreamOwned<ClientConnection, TcpStream>;
pub type HostedServerStream = StreamOwned<ServerConnection, TcpStream>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostedTlsProtocol {
    V1,
    V2,
}

pub fn build_client_config(identity: &ClientTlsIdentity) -> Result<Arc<ClientConfig>> {
    build_client_config_for_alpn(identity, HOSTED_TLS_ALPN_V1)
}

pub fn build_client_config_v2(identity: &ClientTlsIdentity) -> Result<Arc<ClientConfig>> {
    build_client_config_for_alpn(identity, HOSTED_TLS_ALPN_V2)
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

pub fn build_dual_server_config(identity: &ServerTlsIdentity) -> Result<Arc<ServerConfig>> {
    build_server_config_for_alpns(
        identity,
        vec![HOSTED_TLS_ALPN_V2.to_vec(), HOSTED_TLS_ALPN_V1.to_vec()],
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

fn connect_mutual_tls_for_protocol(
    address: &str,
    identity: &ClientTlsIdentity,
    connect_timeout: Duration,
    io_timeout: Duration,
    protocol: HostedTlsProtocol,
) -> Result<HostedClientStream> {
    let config = match protocol {
        HostedTlsProtocol::V1 => build_client_config(identity)?,
        HostedTlsProtocol::V2 => build_client_config_v2(identity)?,
    };
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
        if let Err(error) = complete_client_handshake(&mut connection, &mut tcp, protocol) {
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
    let protocol = match connection.alpn_protocol() {
        Some(protocol) if protocol == HOSTED_TLS_ALPN_V1 => HostedTlsProtocol::V1,
        Some(protocol) if protocol == HOSTED_TLS_ALPN_V2 => HostedTlsProtocol::V2,
        _ => bail!("client did not negotiate a supported hosted-transport ALPN"),
    };
    set_timeouts(&tcp, io_timeout)?;
    Ok((StreamOwned::new(connection, tcp), protocol))
}

pub fn peer_principal_sha256(stream: &HostedServerStream) -> Result<String> {
    let certificate = stream
        .conn
        .peer_certificates()
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
    protocol: HostedTlsProtocol,
) -> Result<()> {
    while connection.is_handshaking() {
        connection
            .complete_io(tcp)
            .context("mutual TLS handshake failed")?;
    }
    let expected = match protocol {
        HostedTlsProtocol::V1 => HOSTED_TLS_ALPN_V1,
        HostedTlsProtocol::V2 => HOSTED_TLS_ALPN_V2,
    };
    if connection.alpn_protocol() != Some(expected) {
        bail!("node did not negotiate the hosted-transport ALPN");
    }
    Ok(())
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
