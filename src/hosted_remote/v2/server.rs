use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;

use anyhow::{Context, Result};

use super::super::node::{serve_v1_stream, HostedNodeRuntime};
use super::super::protocol::{read_hosted_frame, write_hosted_frame};
use super::super::tls::{
    accept_mutual_tls_versioned, build_dual_server_config, peer_principal_sha256,
    HostedTlsProtocol, ServerTlsIdentity, DEFAULT_CONNECT_TIMEOUT, DEFAULT_IO_TIMEOUT,
};
use super::protocol::{HostedProtocolErrorV2, HostedRequestV2, HostedResponseV2};
use super::runtime::HostedV2Runtime;

#[derive(Debug, Clone)]
pub struct HostedDualNodeServerConfig {
    pub bind_address: String,
    pub v1_runtime: HostedNodeRuntime,
    pub v2_runtime: HostedV2Runtime,
    pub tls_identity: ServerTlsIdentity,
}

/// Serve frozen V1 and durable V2 on one TLS port.  ALPN selects the decoder
/// before any application bytes are read, so neither protocol can be parsed as
/// the other and there is no downgrade after negotiation.
pub fn serve_node_dual(config: HostedDualNodeServerConfig) -> Result<()> {
    config.v1_runtime.validate()?;
    let tls_config = build_dual_server_config(&config.tls_identity)?;
    let listener = TcpListener::bind(&config.bind_address).with_context(|| {
        format!(
            "failed to bind dual hosted node at `{}`",
            config.bind_address
        )
    })?;
    let maximum = config.v1_runtime.max_concurrent_connections;
    let v1_runtime = Arc::new(config.v1_runtime);
    let v2_runtime = config.v2_runtime;
    let active = Arc::new(AtomicUsize::new(0));
    for accepted in listener.incoming() {
        let tcp = match accepted {
            Ok(tcp) => tcp,
            Err(error) => {
                eprintln!("o-node: TCP accept failed: {error}");
                continue;
            }
        };
        let previous = active.fetch_add(1, Ordering::AcqRel);
        if previous >= maximum {
            active.fetch_sub(1, Ordering::AcqRel);
            drop(tcp);
            continue;
        }
        let tls_config = Arc::clone(&tls_config);
        let v1_runtime = Arc::clone(&v1_runtime);
        let v2_runtime = v2_runtime.clone();
        let worker_active = Arc::clone(&active);
        let _ = spawn_dual_connection_worker_with(
            Arc::clone(&active),
            move || {
                let _guard = ActiveDualConnectionGuard(worker_active);
                if let Err(error) = serve_dual_connection(tcp, tls_config, &v1_runtime, &v2_runtime)
                {
                    eprintln!("o-node: hosted connection failed: {error:#}");
                }
            },
            |builder, job| builder.spawn(job),
        );
    }
    Ok(())
}

fn spawn_dual_connection_worker_with<Job, Spawn>(
    active: Arc<AtomicUsize>,
    job: Job,
    spawn: Spawn,
) -> std::io::Result<()>
where
    Job: FnOnce() + Send + 'static,
    Spawn: FnOnce(thread::Builder, Job) -> std::io::Result<thread::JoinHandle<()>>,
{
    match spawn(
        thread::Builder::new().name("ostadix-hosted-connection".to_owned()),
        job,
    ) {
        Ok(handle) => {
            drop(handle);
            Ok(())
        }
        Err(error) => {
            active.fetch_sub(1, Ordering::AcqRel);
            eprintln!(
                "o-node: failed to spawn hosted connection worker; dropping accepted connection: {error}"
            );
            Err(error)
        }
    }
}

fn serve_dual_connection(
    tcp: TcpStream,
    tls_config: Arc<rustls::ServerConfig>,
    v1_runtime: &HostedNodeRuntime,
    v2_runtime: &HostedV2Runtime,
) -> Result<()> {
    let (mut stream, protocol) =
        accept_mutual_tls_versioned(tcp, tls_config, DEFAULT_CONNECT_TIMEOUT, DEFAULT_IO_TIMEOUT)?;
    match protocol {
        HostedTlsProtocol::V1 => serve_v1_stream(&mut stream, v1_runtime),
        HostedTlsProtocol::V2 => {
            let principal = peer_principal_sha256(&stream)?;
            let request = match read_hosted_frame::<_, HostedRequestV2>(&mut stream) {
                Ok(Some(request)) => request,
                Ok(None) => anyhow::bail!("authenticated V2 client closed before a request"),
                Err(error) => {
                    write_hosted_frame(
                        &mut stream,
                        &HostedResponseV2::Error {
                            error: HostedProtocolErrorV2::new(
                                "invalid-frame",
                                format!("{error:#}"),
                                false,
                            ),
                        },
                    )?;
                    return Ok(());
                }
            };
            let response = v2_runtime.handle_request(&principal, request);
            write_hosted_frame(&mut stream, &response).context("failed to write hosted V2 response")
        }
    }
}

struct ActiveDualConnectionGuard(Arc<AtomicUsize>);

impl Drop for ActiveDualConnectionGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

#[cfg(test)]
mod tests {
    use std::io::Read;
    use std::time::Duration;

    use super::*;

    #[test]
    fn worker_spawn_failure_releases_capacity_and_drops_accepted_stream() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut peer = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (accepted, _) = listener.accept().unwrap();
        peer.set_read_timeout(Some(Duration::from_secs(1))).unwrap();

        let active = Arc::new(AtomicUsize::new(1));
        let error = spawn_dual_connection_worker_with(
            Arc::clone(&active),
            move || drop(accepted),
            |_builder, _job| {
                Err(std::io::Error::new(
                    std::io::ErrorKind::WouldBlock,
                    "injected thread creation failure",
                ))
            },
        )
        .unwrap_err();

        assert_eq!(error.kind(), std::io::ErrorKind::WouldBlock);
        assert_eq!(active.load(Ordering::Acquire), 0);
        let mut byte = [0_u8; 1];
        assert_eq!(peer.read(&mut byte).unwrap(), 0);
    }
}
