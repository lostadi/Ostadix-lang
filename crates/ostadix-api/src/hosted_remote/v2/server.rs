use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};
use thiserror::Error;

use super::super::mesh::{serve_mesh_stream, MeshNodeRuntime};
use super::super::node::{serve_v1_stream, HostedNodeRuntime};
use super::super::protocol::{read_hosted_frame, write_hosted_frame};
use super::super::tls::{
    accept_mutual_tls_versioned, build_dual_server_config, build_dual_server_config_with_mesh,
    peer_principal_sha256, HostedTlsProtocol, ServerTlsIdentity, DEFAULT_CONNECT_TIMEOUT,
    DEFAULT_IO_TIMEOUT,
};
use super::protocol::{HostedProtocolErrorV2, HostedRequestV2, HostedResponseV2};
use super::runtime::{HostedV2Runtime, HostedV2RuntimeHandle, HostedV2RuntimeOwner};

const SHUTDOWN_POLL_INTERVAL: Duration = Duration::from_millis(25);

#[derive(Debug, Clone)]
pub struct HostedDualNodeServerConfig {
    pub bind_address: String,
    pub v1_runtime: HostedNodeRuntime,
    pub v2_runtime: HostedV2Runtime,
    pub mesh_runtime: Option<MeshNodeRuntime>,
    pub tls_identity: ServerTlsIdentity,
}

/// Preferred server configuration with unique runtime-lifecycle ownership.
#[derive(Debug)]
pub struct HostedOwnedDualNodeServerConfig {
    pub bind_address: String,
    pub v1_runtime: HostedNodeRuntime,
    pub v2_runtime: HostedV2RuntimeOwner,
    pub mesh_runtime: Option<MeshNodeRuntime>,
    pub tls_identity: ServerTlsIdentity,
}

/// Cloneable, monotonic request to stop accepting new dual-node connections.
///
/// Requesting shutdown does not itself terminate a worker or release durable
/// state. [`serve_node_dual_until_shutdown`] owns that barrier: it stops
/// admission, drains the V2 runtime, joins every accepted connection worker,
/// and only then returns.
#[derive(Clone, Debug, Default)]
pub struct HostedDualNodeShutdown {
    requested: Arc<AtomicBool>,
}

impl HostedDualNodeShutdown {
    pub fn new() -> Self {
        Self::default()
    }

    /// Request graceful shutdown. Returns `true` only for the first request.
    pub fn request(&self) -> bool {
        !self.requested.swap(true, Ordering::AcqRel)
    }

    pub fn is_requested(&self) -> bool {
        self.requested.load(Ordering::Acquire)
    }
}

/// The dual-node runtime released its durable root, but one or more accepted
/// connection workers panicked while the server was joining them.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[error("hosted dual-node shutdown completed with connection worker failures: {message}")]
pub struct HostedDualNodeServerShutdownErrorV2 {
    message: String,
}

impl HostedDualNodeServerShutdownErrorV2 {
    pub fn message(&self) -> &str {
        &self.message
    }
}

/// Serve frozen V1 and durable V2 on one TLS port.  ALPN selects the decoder
/// before any application bytes are read, so neither protocol can be parsed as
/// the other and there is no downgrade after negotiation.
pub fn serve_node_dual(config: HostedDualNodeServerConfig) -> Result<()> {
    serve_node_dual_until_shutdown(config, HostedDualNodeShutdown::new())
}

/// Serve with unique Hosted V2 lifecycle ownership until process termination.
pub fn serve_owned_node_dual(config: HostedOwnedDualNodeServerConfig) -> Result<()> {
    serve_owned_node_dual_until_shutdown(config, HostedDualNodeShutdown::new())
}

/// Serve frozen V1 and durable V2 until `shutdown` is requested.
///
/// Returning from this function is a deterministic lifecycle barrier. No new
/// TCP connection can be admitted, the V2 runtime and optional mesh runtime
/// have settled every admitted actor command, their durable roots remain
/// coherent, and every connection worker accepted by this server has joined.
pub fn serve_node_dual_until_shutdown(
    config: HostedDualNodeServerConfig,
    shutdown: HostedDualNodeShutdown,
) -> Result<()> {
    let HostedDualNodeServerConfig {
        bind_address,
        v1_runtime,
        v2_runtime,
        mesh_runtime,
        tls_identity,
    } = config;
    serve_node_dual_with_runtime(
        bind_address,
        v1_runtime,
        DualRuntimeAuthorityV2::Compatibility(v2_runtime),
        mesh_runtime,
        tls_identity,
        shutdown,
    )
}

/// Preferred deterministic server barrier. The server owns the only shutdown
/// authority and distributes request-only handles to connection workers.
pub fn serve_owned_node_dual_until_shutdown(
    config: HostedOwnedDualNodeServerConfig,
    shutdown: HostedDualNodeShutdown,
) -> Result<()> {
    let HostedOwnedDualNodeServerConfig {
        bind_address,
        v1_runtime,
        v2_runtime,
        mesh_runtime,
        tls_identity,
    } = config;
    serve_node_dual_with_runtime(
        bind_address,
        v1_runtime,
        DualRuntimeAuthorityV2::Owned(v2_runtime),
        mesh_runtime,
        tls_identity,
        shutdown,
    )
}

enum DualRuntimeAuthorityV2 {
    Compatibility(HostedV2Runtime),
    Owned(HostedV2RuntimeOwner),
}

impl DualRuntimeAuthorityV2 {
    fn handle(&self) -> HostedV2RuntimeHandle {
        match self {
            Self::Compatibility(runtime) => runtime.handle(),
            Self::Owned(runtime) => runtime.handle(),
        }
    }

    fn shutdown(&self) -> Result<()> {
        match self {
            Self::Compatibility(runtime) => runtime.shutdown(),
            Self::Owned(runtime) => runtime.shutdown(),
        }
    }
}

fn serve_node_dual_with_runtime(
    bind_address: String,
    v1_runtime: HostedNodeRuntime,
    v2_runtime: DualRuntimeAuthorityV2,
    mesh_runtime: Option<MeshNodeRuntime>,
    tls_identity: ServerTlsIdentity,
    shutdown: HostedDualNodeShutdown,
) -> Result<()> {
    let v2_handle = v2_runtime.handle();

    let setup = (|| -> Result<_> {
        v1_runtime.validate()?;
        let tls_config = if mesh_runtime.is_some() {
            build_dual_server_config_with_mesh(&tls_identity)?
        } else {
            build_dual_server_config(&tls_identity)?
        };
        let listener = TcpListener::bind(&bind_address)
            .with_context(|| format!("failed to bind dual hosted node at `{bind_address}`"))?;
        listener.set_nonblocking(true).with_context(|| {
            format!("failed to make dual hosted listener `{bind_address}` nonblocking")
        })?;
        Ok((listener, tls_config))
    })();
    let (listener, tls_config) = match setup {
        Ok(setup) => setup,
        Err(error) => {
            return finish_dual_node_server(
                Some(error),
                &v2_runtime,
                mesh_runtime.as_ref(),
                Vec::new(),
                Vec::new(),
            )
        }
    };

    let maximum = v1_runtime.max_concurrent_connections;
    let v1_runtime = Arc::new(v1_runtime);
    let active = Arc::new(AtomicUsize::new(0));
    let mut workers = Vec::new();
    let mut worker_failures = Vec::new();
    let mut server_error = None;

    loop {
        reap_finished_connection_workers(&mut workers, &mut worker_failures);
        if shutdown.is_requested() {
            break;
        }

        let tcp = match listener.accept() {
            Ok((tcp, _peer)) => tcp,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(SHUTDOWN_POLL_INTERVAL);
                continue;
            }
            Err(error) => {
                server_error = Some(anyhow::Error::new(error).context(format!(
                    "dual hosted listener `{bind_address}` failed while accepting a connection"
                )));
                break;
            }
        };
        // A shutdown request may have arrived between the loop check and the
        // successful nonblocking accept. That socket was never admitted.
        if shutdown.is_requested() {
            drop(tcp);
            break;
        }
        // Some platforms propagate the listener's nonblocking status to an
        // accepted socket. TLS uses bounded blocking I/O timeouts, so normalize
        // the per-connection stream before handing it to a worker.
        if let Err(error) = tcp.set_nonblocking(false) {
            eprintln!("o-node: failed to configure accepted TCP stream: {error}");
            drop(tcp);
            continue;
        }
        let previous = active.fetch_add(1, Ordering::AcqRel);
        if previous >= maximum {
            active.fetch_sub(1, Ordering::AcqRel);
            drop(tcp);
            continue;
        }
        let tls_config = Arc::clone(&tls_config);
        let v1_runtime = Arc::clone(&v1_runtime);
        let v2_runtime = v2_handle.clone();
        let mesh_runtime = mesh_runtime.clone();
        let worker_active = Arc::clone(&active);
        if let Ok(worker) = spawn_dual_connection_worker_with(
            Arc::clone(&active),
            move || {
                let _guard = ActiveDualConnectionGuard(worker_active);
                if let Err(error) = serve_dual_connection(
                    tcp,
                    tls_config,
                    &v1_runtime,
                    &v2_runtime,
                    mesh_runtime.as_ref(),
                ) {
                    eprintln!("o-node: hosted connection failed: {error:#}");
                }
            },
            |builder, job| builder.spawn(job),
        ) {
            workers.push(worker);
        }
    }

    // Dropping the listener is the admission barrier. Runtime shutdown then
    // drains any request already inside V2 and every accepted mesh actor.
    // Finally, joining connection workers settles authenticated replies.
    drop(listener);
    finish_dual_node_server(
        server_error,
        &v2_runtime,
        mesh_runtime.as_ref(),
        workers,
        worker_failures,
    )
}

fn spawn_dual_connection_worker_with<Job, Spawn>(
    active: Arc<AtomicUsize>,
    job: Job,
    spawn: Spawn,
) -> std::io::Result<thread::JoinHandle<()>>
where
    Job: FnOnce() + Send + 'static,
    Spawn: FnOnce(thread::Builder, Job) -> std::io::Result<thread::JoinHandle<()>>,
{
    match spawn(
        thread::Builder::new().name("ostadix-hosted-connection".to_owned()),
        job,
    ) {
        Ok(handle) => Ok(handle),
        Err(error) => {
            active.fetch_sub(1, Ordering::AcqRel);
            eprintln!(
                "o-node: failed to spawn hosted connection worker; dropping accepted connection: {error}"
            );
            Err(error)
        }
    }
}

fn finish_dual_node_server(
    server_error: Option<anyhow::Error>,
    v2_runtime: &DualRuntimeAuthorityV2,
    mesh_runtime: Option<&MeshNodeRuntime>,
    workers: Vec<thread::JoinHandle<()>>,
    mut worker_failures: Vec<String>,
) -> Result<()> {
    // Mesh streams are multi-request and may already be authenticated when the
    // listener closes. Stop their actor admission first so V2 draining cannot
    // leave an existing mesh connection able to admit fresh work meanwhile.
    let mesh_shutdown_error = mesh_runtime.and_then(|runtime| runtime.shutdown().err());
    let v2_shutdown_error = v2_runtime.shutdown().err();
    let runtime_shutdown_error = match (v2_shutdown_error, mesh_shutdown_error) {
        (None, None) => None,
        (Some(error), None) => Some(error),
        (None, Some(error)) => Some(error.context("mesh runtime shutdown failed")),
        (Some(error), Some(mesh_error)) => {
            Some(error.context(format!("mesh runtime shutdown also failed: {mesh_error:#}")))
        }
    };
    for worker in workers {
        record_connection_worker_result(worker, &mut worker_failures);
    }
    worker_failures.sort();
    worker_failures.dedup();

    let worker_error = (!worker_failures.is_empty()).then(|| HostedDualNodeServerShutdownErrorV2 {
        message: worker_failures.join("; "),
    });
    match (server_error, runtime_shutdown_error, worker_error) {
        (None, None, None) => Ok(()),
        (Some(error), None, None) => Err(error),
        (None, Some(error), None) => Err(error),
        (None, None, Some(error)) => Err(error.into()),
        (Some(error), runtime_error, worker_error) => {
            let mut cleanup = Vec::new();
            if let Some(runtime_error) = runtime_error {
                cleanup.push(format!("V2 runtime shutdown failed: {runtime_error:#}"));
            }
            if let Some(worker_error) = worker_error {
                cleanup.push(worker_error.to_string());
            }
            Err(error.context(format!(
                "dual hosted server cleanup also failed: {}",
                cleanup.join("; ")
            )))
        }
        (None, Some(runtime_error), Some(worker_error)) => Err(runtime_error.context(worker_error)),
    }
}

fn reap_finished_connection_workers(
    workers: &mut Vec<thread::JoinHandle<()>>,
    failures: &mut Vec<String>,
) {
    let mut index = 0;
    while index < workers.len() {
        if workers[index].is_finished() {
            let worker = workers.swap_remove(index);
            record_connection_worker_result(worker, failures);
        } else {
            index += 1;
        }
    }
}

fn record_connection_worker_result(worker: thread::JoinHandle<()>, failures: &mut Vec<String>) {
    if let Err(payload) = worker.join() {
        let detail = payload
            .downcast_ref::<&str>()
            .map(|message| (*message).to_owned())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "non-string panic payload".to_owned());
        failures.push(format!("hosted connection worker panicked: {detail}"));
    }
}

fn serve_dual_connection(
    tcp: TcpStream,
    tls_config: Arc<rustls::ServerConfig>,
    v1_runtime: &HostedNodeRuntime,
    v2_runtime: &HostedV2RuntimeHandle,
    mesh_runtime: Option<&MeshNodeRuntime>,
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
        HostedTlsProtocol::MeshV1 => {
            let runtime = mesh_runtime
                .context("client negotiated the mesh ALPN while the mesh runtime is disabled")?;
            serve_mesh_stream(&mut stream, runtime)
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

    #[test]
    fn shutdown_request_is_monotonic_and_shared_by_clones() {
        let shutdown = HostedDualNodeShutdown::new();
        let clone = shutdown.clone();
        assert!(!shutdown.is_requested());
        assert!(clone.request());
        assert!(shutdown.is_requested());
        assert!(!shutdown.request());
    }

    #[test]
    fn successful_worker_is_joined_and_releases_capacity() {
        let active = Arc::new(AtomicUsize::new(1));
        let worker_active = Arc::clone(&active);
        let worker = spawn_dual_connection_worker_with(
            Arc::clone(&active),
            move || drop(ActiveDualConnectionGuard(worker_active)),
            |builder, job| builder.spawn(job),
        )
        .unwrap();
        worker.join().unwrap();
        assert_eq!(active.load(Ordering::Acquire), 0);
    }
}
