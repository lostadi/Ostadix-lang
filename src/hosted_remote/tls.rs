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

pub const HOSTED_TLS_ALPN_V1: &[u8] = b"ostadix-hosted/1";
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

pub fn build_client_config(identity: &ClientTlsIdentity) -> Result<Arc<ClientConfig>> {
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
    config.alpn_protocols = vec![HOSTED_TLS_ALPN_V1.to_vec()];
    Ok(Arc::new(config))
}

pub fn build_server_config(identity: &ServerTlsIdentity) -> Result<Arc<ServerConfig>> {
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
    config.alpn_protocols = vec![HOSTED_TLS_ALPN_V1.to_vec()];
    Ok(Arc::new(config))
}

pub fn connect_mutual_tls(
    address: &str,
    identity: &ClientTlsIdentity,
    connect_timeout: Duration,
    io_timeout: Duration,
) -> Result<HostedClientStream> {
    let config = build_client_config(identity)?;
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
        if let Err(error) = complete_client_handshake(&mut connection, &mut tcp) {
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
    mut tcp: TcpStream,
    config: Arc<ServerConfig>,
    handshake_timeout: Duration,
    io_timeout: Duration,
) -> Result<HostedServerStream> {
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
    if connection.alpn_protocol() != Some(HOSTED_TLS_ALPN_V1) {
        bail!("client did not negotiate the hosted-transport ALPN");
    }
    set_timeouts(&tcp, io_timeout)?;
    Ok(StreamOwned::new(connection, tcp))
}

fn complete_client_handshake(connection: &mut ClientConnection, tcp: &mut TcpStream) -> Result<()> {
    while connection.is_handshaking() {
        connection
            .complete_io(tcp)
            .context("mutual TLS handshake failed")?;
    }
    if connection.alpn_protocol() != Some(HOSTED_TLS_ALPN_V1) {
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
