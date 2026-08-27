//! Hosted node request handling and the bounded synchronous TCP server.

use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;

use anyhow::{bail, Context, Result};

use crate::backend_catalog::BackendRegistry;
use crate::eval::Evaluator;
use crate::parser::Parser;
use crate::runtime_exec::validate_native_runtime_binary;

use super::fabric::{serve_fabric_stream_v1, FabricAttemptProviderV1};
use super::protocol::{
    canonical_hosted_bytes, canonical_hosted_sha256, read_hosted_frame, sha256_hex, unix_time_ms,
    write_hosted_frame, HostedFailureStageV1, HostedOperationOutcomeV1, HostedOperationReceiptV1,
    HostedProtocolErrorV1, HostedRequestV1, HostedResponseV1, NodeDoctorCheckV1, NodeDoctorV1,
    NodeProfileV1, RemotePreparedOperationV1, NODE_DOCTOR_SCHEMA_V1,
};
use super::tls::{
    accept_mutual_tls, accept_mutual_tls_with_execution_fabric_v1, build_server_config,
    build_server_config_with_execution_fabric_v1, peer_principal_sha256, HostedTlsProtocol,
    HostedTlsRouteV1, ServerTlsIdentity, DEFAULT_CONNECT_TIMEOUT, DEFAULT_IO_TIMEOUT,
};

pub const DEFAULT_NODE_BIND: &str = "127.0.0.1:7337";
pub const DEFAULT_NODE_ID: &str = "ostadix-local-node";
pub const DEFAULT_MAX_CONNECTIONS: usize = 32;

#[derive(Debug, Clone)]
pub struct HostedNodeRuntime {
    pub node_id: String,
    pub shim_dir: PathBuf,
    /// Native evaluator image used for admitted `--o-backend` proxy launches.
    /// `o-node` is an embedding host and cannot serve as its own proxy. Image
    /// validation is format-only; an admitted run exercises the protocol.
    pub runtime_executable: PathBuf,
    pub max_concurrent_connections: usize,
}

impl HostedNodeRuntime {
    pub fn validate(&self) -> Result<()> {
        if self.max_concurrent_connections == 0 || self.max_concurrent_connections > 1024 {
            bail!("node max-connections must be between 1 and 1024");
        }
        validate_native_runtime_binary(&self.runtime_executable)
            .context("node runtime executable is not a supported native image")?;
        // NodeProfile validates the identifier and catalog projection for us.
        self.profile().map(|_| ())
    }

    pub fn profile(&self) -> Result<NodeProfileV1> {
        NodeProfileV1::local(&self.node_id, self.max_concurrent_connections)
    }

    pub fn doctor(&self) -> Result<NodeDoctorV1> {
        let profile = self.profile()?;
        let shim_exists = self.shim_dir.exists();
        let shim_is_directory = self.shim_dir.is_dir();
        let runtime_check = match validate_native_runtime_binary(&self.runtime_executable) {
            Ok(path) => NodeDoctorCheckV1 {
                name: "native-runtime-image-valid".to_string(),
                ok: true,
                detail: format!(
                    "{} (native-image preflight only; the first admitted hosted-backend launch exercises the O protocol)",
                    path.display()
                ),
            },
            Err(error) => NodeDoctorCheckV1 {
                name: "native-runtime-image-valid".to_string(),
                ok: false,
                detail: format!("{error:#}"),
            },
        };
        let checks = vec![
            NodeDoctorCheckV1 {
                name: "shim-directory-exists".to_string(),
                ok: shim_exists,
                detail: self.shim_dir.display().to_string(),
            },
            NodeDoctorCheckV1 {
                name: "shim-path-is-directory".to_string(),
                ok: shim_is_directory,
                detail: self.shim_dir.display().to_string(),
            },
            runtime_check,
            NodeDoctorCheckV1 {
                name: "backend-catalog-is-descriptive".to_string(),
                ok: true,
                detail: "catalog digest binds compiled adapter metadata; it is not a runtime availability probe"
                    .to_string(),
            },
            NodeDoctorCheckV1 {
                name: "transport-boundaries".to_string(),
                ok: true,
                detail: "TLS 1.3 mTLS; no plaintext or early data; one bounded prepared operation per request"
                    .to_string(),
            },
        ];
        Ok(NodeDoctorV1 {
            schema: NODE_DOCTOR_SCHEMA_V1.to_string(),
            node_id: self.node_id.clone(),
            ready: checks.iter().all(|check| check.ok),
            backend_catalog_sha256: profile.backend_catalog_sha256.clone(),
            profile_sha256: canonical_hosted_sha256(&profile)?,
            shim_directory: self.shim_dir.display().to_string(),
            checks,
        })
    }

    pub fn handle_request(&self, request: HostedRequestV1) -> HostedResponseV1 {
        if let Err(error) = request.validate() {
            return HostedResponseV1::Error {
                error: HostedProtocolErrorV1::new("invalid-request", format!("{error:#}")),
            };
        }
        match request {
            HostedRequestV1::Profile { .. } => self
                .profile()
                .map(|profile| HostedResponseV1::Profile { profile })
                .unwrap_or_else(protocol_internal_error),
            HostedRequestV1::Doctor { .. } => self
                .doctor()
                .map(|doctor| HostedResponseV1::Doctor { doctor })
                .unwrap_or_else(protocol_internal_error),
            HostedRequestV1::Run { operation, .. } => self
                .execute_prepared(operation)
                .map(|receipt| HostedResponseV1::Run { receipt })
                .unwrap_or_else(protocol_internal_error),
        }
    }

    /// Execute one structurally valid prepared operation using a fresh local
    /// evaluator. The deadline is checked before parsing and after evaluation.
    /// A late result is never published, though already-running evaluator
    /// effects cannot yet be cancelled safely by the public evaluator API.
    pub fn execute_prepared(
        &self,
        operation: RemotePreparedOperationV1,
    ) -> Result<HostedOperationReceiptV1> {
        operation.validate_structure()?;
        let started = unix_time_ms()?;
        let actual_source_sha256 = sha256_hex(operation.source_utf8.as_bytes());
        let actual_catalog_sha256 = BackendRegistry::global().catalog_sha256();
        let runtime_executable = validate_native_runtime_binary(&self.runtime_executable)
            .map_err(|error| format!("{error:#}"));

        let outcome = if actual_source_sha256 != operation.source_sha256 {
            HostedOperationOutcomeV1::failed(
                HostedFailureStageV1::Admission,
                "source-digest-mismatch",
                format!(
                    "prepared source digest {} does not match received bytes {}",
                    operation.source_sha256, actual_source_sha256
                ),
            )
        } else if actual_catalog_sha256 != operation.expected_backend_catalog_sha256 {
            HostedOperationOutcomeV1::failed(
                HostedFailureStageV1::Admission,
                "backend-catalog-mismatch",
                format!(
                    "prepared catalog digest {} does not match node catalog {}",
                    operation.expected_backend_catalog_sha256, actual_catalog_sha256
                ),
            )
        } else if let Err(error) = runtime_executable {
            HostedOperationOutcomeV1::failed(
                HostedFailureStageV1::Admission,
                "runtime-executable-invalid",
                error,
            )
        } else if started >= operation.deadline_unix_ms {
            HostedOperationOutcomeV1::failed(
                HostedFailureStageV1::Deadline,
                "deadline-expired",
                format!(
                    "operation deadline {} was not later than node start time {}",
                    operation.deadline_unix_ms, started
                ),
            )
        } else {
            self.parse_and_evaluate(&operation)
        };

        let finished = unix_time_ms()?;
        let outcome = if finished > operation.deadline_unix_ms
            && matches!(&outcome, HostedOperationOutcomeV1::Succeeded { .. })
        {
            HostedOperationOutcomeV1::failed(
                HostedFailureStageV1::Deadline,
                "deadline-exceeded",
                format!(
                    "evaluation completed at {finished}, after deadline {}",
                    operation.deadline_unix_ms
                ),
            )
        } else {
            outcome
        };

        HostedOperationReceiptV1::issue(
            &self.node_id,
            &operation,
            actual_source_sha256,
            actual_catalog_sha256,
            started,
            finished,
            outcome,
        )
    }

    fn parse_and_evaluate(
        &self,
        operation: &RemotePreparedOperationV1,
    ) -> HostedOperationOutcomeV1 {
        let source = strip_shebang(&operation.source_utf8);
        let backends = BackendRegistry::global().registered_backend_tags();
        let mut parser = Parser::new(source, &backends);
        let nodes = match parser.parse() {
            Ok(nodes) => nodes,
            Err(error) => {
                return HostedOperationOutcomeV1::failed(
                    HostedFailureStageV1::Parse,
                    "parse-failed",
                    format!("{error:#}"),
                )
            }
        };

        let mut evaluator = Evaluator::new(self.shim_dir.clone())
            .with_registered_backends(backends)
            .with_runtime_executable(self.runtime_executable.clone());
        match evaluator.eval_document(nodes) {
            Err(error) => HostedOperationOutcomeV1::failed(
                HostedFailureStageV1::Evaluate,
                "evaluation-failed",
                format!("{error:#}"),
            ),
            Ok(value) => match canonical_hosted_bytes(&value) {
                Err(error) => HostedOperationOutcomeV1::failed(
                    HostedFailureStageV1::Output,
                    "result-encoding-failed",
                    format!("{error:#}"),
                ),
                Ok(bytes) if bytes.len() > operation.output_limit_bytes as usize => {
                    HostedOperationOutcomeV1::failed(
                        HostedFailureStageV1::Output,
                        "result-too-large",
                        format!(
                            "serialized result length {} exceeds prepared output limit {}",
                            bytes.len(),
                            operation.output_limit_bytes
                        ),
                    )
                }
                Ok(_) => HostedOperationOutcomeV1::Succeeded { value },
            },
        }
    }
}

#[derive(Debug, Clone)]
pub struct HostedNodeServerConfig {
    pub bind_address: String,
    pub runtime: HostedNodeRuntime,
    pub tls_identity: ServerTlsIdentity,
}

/// Explicitly opt frozen Hosted V1 into sharing its listener with Fabric V1.
/// The existing [`HostedNodeServerConfig`] and serve functions remain
/// Fabric-free; callers must provide a configured execution authority here.
#[derive(Clone)]
pub struct HostedNodeWithFabricServerConfigV1 {
    pub hosted: HostedNodeServerConfig,
    pub fabric_provider: Arc<FabricAttemptProviderV1>,
}

/// Serve requests until the listener fails or the process is terminated.
/// Connections are isolated in bounded worker threads. Excess connections are
/// closed before TLS, keeping the concurrency cap authoritative.
pub fn serve_node(config: HostedNodeServerConfig) -> Result<()> {
    serve_node_with_listener_ready(config, |_| Ok(()))
}

/// Serve requests and invoke `listener_ready` exactly once after the TCP
/// listener is successfully bound and inspected, before accepting connections.
pub fn serve_node_with_listener_ready<F>(
    config: HostedNodeServerConfig,
    listener_ready: F,
) -> Result<()>
where
    F: FnOnce(SocketAddr) -> Result<()>,
{
    config.runtime.validate()?;
    let tls_config = build_server_config(&config.tls_identity)?;
    let listener = TcpListener::bind(&config.bind_address)
        .with_context(|| format!("failed to bind hosted node at `{}`", config.bind_address))?;
    let listening_address = listener.local_addr().with_context(|| {
        format!(
            "failed to inspect hosted listener bound from `{}`",
            config.bind_address
        )
    })?;
    listener_ready(listening_address).context("hosted listener-ready hook failed")?;
    let runtime = Arc::new(config.runtime);
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
        if previous >= runtime.max_concurrent_connections {
            active.fetch_sub(1, Ordering::AcqRel);
            drop(tcp);
            continue;
        }

        let runtime = Arc::clone(&runtime);
        let tls_config = Arc::clone(&tls_config);
        let active = Arc::clone(&active);
        thread::spawn(move || {
            let _slot = ActiveConnectionGuard(active);
            if let Err(error) = serve_connection(tcp, tls_config, &runtime) {
                eprintln!("o-node: hosted connection failed: {error:#}");
            }
        });
    }
    Ok(())
}

/// Serve frozen Hosted V1 and execution Fabric V1 on one explicitly opted-in
/// TLS listener. ALPN selects the route before any application bytes are read.
pub fn serve_node_with_execution_fabric_v1(
    config: HostedNodeWithFabricServerConfigV1,
) -> Result<()> {
    serve_node_with_execution_fabric_v1_and_listener_ready(config, |_| Ok(()))
}

/// Fabric-enabled counterpart to [`serve_node_with_listener_ready`]. Hosted
/// and Fabric connections share the same authoritative concurrency counter.
pub fn serve_node_with_execution_fabric_v1_and_listener_ready<F>(
    config: HostedNodeWithFabricServerConfigV1,
    listener_ready: F,
) -> Result<()>
where
    F: FnOnce(SocketAddr) -> Result<()>,
{
    let HostedNodeWithFabricServerConfigV1 {
        hosted,
        fabric_provider,
    } = config;
    if hosted.runtime.node_id != fabric_provider.node_id() {
        bail!(
            "Hosted runtime node identity `{}` differs from Fabric provider identity `{}`",
            hosted.runtime.node_id,
            fabric_provider.node_id()
        );
    }
    hosted.runtime.validate()?;
    let tls_config = build_server_config_with_execution_fabric_v1(&hosted.tls_identity)?;
    let listener = TcpListener::bind(&hosted.bind_address).with_context(|| {
        format!(
            "failed to bind hosted/Fabric node at `{}`",
            hosted.bind_address
        )
    })?;
    let listening_address = listener.local_addr().with_context(|| {
        format!(
            "failed to inspect hosted/Fabric listener bound from `{}`",
            hosted.bind_address
        )
    })?;
    listener_ready(listening_address).context("hosted/Fabric listener-ready hook failed")?;
    let runtime = Arc::new(hosted.runtime);
    let active = Arc::new(AtomicUsize::new(0));

    for accepted in listener.incoming() {
        let tcp = match accepted {
            Ok(tcp) => tcp,
            Err(error) => {
                eprintln!("o-node: hosted/Fabric TCP accept failed: {error}");
                continue;
            }
        };
        let previous = active.fetch_add(1, Ordering::AcqRel);
        if previous >= runtime.max_concurrent_connections {
            active.fetch_sub(1, Ordering::AcqRel);
            drop(tcp);
            continue;
        }

        let runtime = Arc::clone(&runtime);
        let fabric_provider = Arc::clone(&fabric_provider);
        let tls_config = Arc::clone(&tls_config);
        let active = Arc::clone(&active);
        thread::spawn(move || {
            let _slot = ActiveConnectionGuard(active);
            if let Err(error) = serve_connection_with_execution_fabric_v1(
                tcp,
                tls_config,
                &runtime,
                &fabric_provider,
            ) {
                eprintln!("o-node: hosted/Fabric connection failed: {error:#}");
            }
        });
    }
    Ok(())
}

fn serve_connection(
    tcp: TcpStream,
    tls_config: Arc<rustls::ServerConfig>,
    runtime: &HostedNodeRuntime,
) -> Result<()> {
    let mut stream =
        accept_mutual_tls(tcp, tls_config, DEFAULT_CONNECT_TIMEOUT, DEFAULT_IO_TIMEOUT)?;
    serve_v1_stream(&mut stream, runtime)
}

fn serve_connection_with_execution_fabric_v1(
    tcp: TcpStream,
    tls_config: Arc<rustls::ServerConfig>,
    runtime: &HostedNodeRuntime,
    fabric_provider: &FabricAttemptProviderV1,
) -> Result<()> {
    let (mut stream, route) = accept_mutual_tls_with_execution_fabric_v1(
        tcp,
        tls_config,
        DEFAULT_CONNECT_TIMEOUT,
        DEFAULT_IO_TIMEOUT,
    )?;
    match route {
        HostedTlsRouteV1::Hosted(HostedTlsProtocol::V1) => serve_v1_stream(&mut stream, runtime),
        HostedTlsRouteV1::ExecutionFabricV1 => {
            let principal = peer_principal_sha256(&stream)?;
            serve_fabric_stream_v1(&mut stream, fabric_provider, &principal)
        }
        HostedTlsRouteV1::Hosted(protocol) => {
            bail!("V1/Fabric listener negotiated unsupported Hosted protocol {protocol:?}")
        }
    }
}

pub(crate) fn serve_v1_stream(
    stream: &mut super::tls::HostedServerStream,
    runtime: &HostedNodeRuntime,
) -> Result<()> {
    let request = match read_hosted_frame::<_, HostedRequestV1>(&mut *stream) {
        Ok(Some(request)) => request,
        Ok(None) => bail!("authenticated client closed before sending a request"),
        Err(error) => {
            let response = HostedResponseV1::Error {
                error: HostedProtocolErrorV1::new("invalid-frame", format!("{error:#}")),
            };
            write_hosted_frame(&mut *stream, &response)
                .context("failed to return invalid-frame response")?;
            return Ok(());
        }
    };
    let response = runtime.handle_request(request);
    write_hosted_frame(&mut *stream, &response).context("failed to write hosted response")?;
    Ok(())
}

fn strip_shebang(source: &str) -> &str {
    if !source.starts_with("#!") {
        return source;
    }
    source
        .find('\n')
        .map_or("", |newline| &source[newline + 1..])
}

fn protocol_internal_error(error: anyhow::Error) -> HostedResponseV1 {
    HostedResponseV1::Error {
        error: HostedProtocolErrorV1::new("internal-error", format!("{error:#}")),
    }
}

struct ActiveConnectionGuard(Arc<AtomicUsize>);

impl Drop for ActiveConnectionGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use crate::hosted_remote::tls::{
        test_server_tls_identity, EXECUTION_FABRIC_TLS_ALPN_V1, HOSTED_TLS_ALPN_V1,
    };

    use super::*;

    fn test_server_config(
        bind_address: String,
        tls_identity: ServerTlsIdentity,
    ) -> HostedNodeServerConfig {
        HostedNodeServerConfig {
            bind_address,
            runtime: HostedNodeRuntime {
                node_id: "listener-ready-test".to_owned(),
                shim_dir: PathBuf::from(env!("CARGO_MANIFEST_DIR")),
                runtime_executable: std::env::current_exe().unwrap(),
                max_concurrent_connections: 1,
            },
            tls_identity,
        }
    }

    #[test]
    fn fabric_opt_in_adds_only_fabric_beside_hosted_v1() {
        let (_tls_directory, identity) = test_server_tls_identity().unwrap();
        let ordinary = build_server_config(&identity).unwrap();
        assert_eq!(ordinary.alpn_protocols, vec![HOSTED_TLS_ALPN_V1.to_vec()]);

        let fabric = build_server_config_with_execution_fabric_v1(&identity).unwrap();
        assert_eq!(
            fabric.alpn_protocols,
            vec![
                EXECUTION_FABRIC_TLS_ALPN_V1.to_vec(),
                HOSTED_TLS_ALPN_V1.to_vec(),
            ]
        );
    }

    #[test]
    fn listener_ready_hook_runs_after_bind_and_error_releases_listener() {
        let (_tls_directory, tls_identity) = test_server_tls_identity().unwrap();
        let reported_address = Cell::new(None);
        let error = serve_node_with_listener_ready(
            test_server_config("127.0.0.1:0".to_owned(), tls_identity),
            |address| {
                assert!(address.ip().is_loopback());
                assert_ne!(address.port(), 0);
                assert_eq!(
                    TcpListener::bind(address).unwrap_err().kind(),
                    std::io::ErrorKind::AddrInUse
                );
                reported_address.set(Some(address));
                anyhow::bail!("injected listener-ready failure")
            },
        )
        .unwrap_err();

        assert_eq!(error.to_string(), "hosted listener-ready hook failed");
        assert!(error
            .chain()
            .any(|cause| cause.to_string() == "injected listener-ready failure"));
        let rebound = TcpListener::bind(reported_address.get().unwrap()).unwrap();
        drop(rebound);
    }

    #[test]
    fn bind_failure_does_not_invoke_listener_ready_hook() {
        let reservation = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = reservation.local_addr().unwrap();
        let (_tls_directory, tls_identity) = test_server_tls_identity().unwrap();
        let invoked = Cell::new(false);

        let error = serve_node_with_listener_ready(
            test_server_config(address.to_string(), tls_identity),
            |_| {
                invoked.set(true);
                Ok(())
            },
        )
        .unwrap_err();

        assert!(error.to_string().contains("failed to bind hosted node"));
        assert!(!invoked.get());
    }

    #[test]
    fn source_mismatch_is_a_digest_bound_rejection_receipt() {
        let mut operation = RemotePreparedOperationV1::new(
            "task-1",
            "attempt-1",
            "2",
            BackendRegistry::global().catalog_sha256(),
            unix_time_ms().unwrap() + 60_000,
            1024,
        )
        .unwrap();
        operation.source_utf8 = "3".to_string();
        let runtime = HostedNodeRuntime {
            node_id: "node-a".to_string(),
            shim_dir: PathBuf::from("backends"),
            runtime_executable: std::env::current_exe().unwrap(),
            max_concurrent_connections: 1,
        };
        let receipt = runtime.execute_prepared(operation).unwrap();
        receipt.validate().unwrap();
        assert!(matches!(
            receipt.outcome,
            HostedOperationOutcomeV1::Failed {
                stage: HostedFailureStageV1::Admission,
                ref code,
                ..
            } if code == "source-digest-mismatch"
        ));
    }

    #[test]
    fn expired_operation_never_enters_the_evaluator() {
        let operation = RemotePreparedOperationV1::new(
            "task-1",
            "attempt-1",
            "this is deliberately not valid O",
            BackendRegistry::global().catalog_sha256(),
            1,
            1024,
        )
        .unwrap();
        let runtime = HostedNodeRuntime {
            node_id: "node-a".to_string(),
            shim_dir: PathBuf::from("backends"),
            runtime_executable: std::env::current_exe().unwrap(),
            max_concurrent_connections: 1,
        };
        let receipt = runtime.execute_prepared(operation).unwrap();
        assert!(matches!(
            receipt.outcome,
            HostedOperationOutcomeV1::Failed {
                stage: HostedFailureStageV1::Deadline,
                ..
            }
        ));
    }

    #[test]
    fn script_runtime_is_rejected_before_evaluation() {
        let directory = tempfile::tempdir().unwrap();
        let wrapper = directory.path().join("O-wrapper");
        std::fs::write(&wrapper, "#!/bin/sh\nexit 0\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let operation = RemotePreparedOperationV1::new(
            "task-1",
            "attempt-1",
            "text^(must-not-run)_text",
            BackendRegistry::global().catalog_sha256(),
            unix_time_ms().unwrap() + 60_000,
            1024,
        )
        .unwrap();
        let runtime = HostedNodeRuntime {
            node_id: "node-a".to_string(),
            shim_dir: PathBuf::from("backends"),
            runtime_executable: wrapper,
            max_concurrent_connections: 1,
        };
        let receipt = runtime.execute_prepared(operation).unwrap();
        assert!(matches!(
            receipt.outcome,
            HostedOperationOutcomeV1::Failed {
                stage: HostedFailureStageV1::Admission,
                ref code,
                ..
            } if code == "runtime-executable-invalid"
        ));
    }
}
