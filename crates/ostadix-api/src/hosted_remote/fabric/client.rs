//! One-exchange authenticated execution-Fabric client.
//!
//! This client deliberately owns no retry, polling, resubmission, attempt
//! generation, candidate validation, publication, or settlement policy. Each
//! call opens a fresh Fabric-only mutually authenticated TLS connection and
//! performs exactly one request/response exchange.

use std::net::SocketAddr;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};

use crate::execution_fabric_authority::{FabricRequestV1, FabricResponseV1};

use super::super::tls::{
    connect_mutual_tls_execution_fabric_v1_until, peer_server_principal_sha256,
    ExecutionFabricClientTlsV1, ExecutionFabricTlsConnectFailureV1,
};
#[cfg(test)]
use super::super::tls::{prepare_execution_fabric_client_tls_v1, ClientTlsIdentity};
use super::wire::{
    fabric_wire_io_disposition_v1, prepare_fabric_client_request_v1,
    read_fabric_client_response_v1, write_prepared_fabric_client_request_v1,
    FabricWireIoDispositionV1,
};

/// Immutable result of one authenticated Fabric request/response exchange.
///
/// The peer principal comes from the TLS handshake rather than response
/// metadata. The observation time is sampled only after canonical response
/// decoding and authenticated response EOF both succeed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FabricClientExchangeV1 {
    response: FabricResponseV1,
    tls_server_principal_sha256: String,
    coordinator_observed_unix_ms: u64,
}

impl FabricClientExchangeV1 {
    pub(crate) fn response(&self) -> &FabricResponseV1 {
        &self.response
    }

    pub(crate) fn tls_server_principal_sha256(&self) -> &str {
        &self.tls_server_principal_sha256
    }

    pub(crate) fn coordinator_observed_unix_ms(&self) -> u64 {
        self.coordinator_observed_unix_ms
    }
}

/// Exact endpoint and timeout policy for one-at-a-time Fabric exchanges.
///
/// `exchange` never reconnects, retries, polls, or resubmits. An Accepted or
/// Running response therefore requires the coordinator to issue an explicit
/// QueryAttempt through a separate call and fresh TLS connection.
#[derive(Clone, Debug)]
pub(crate) struct FabricAttemptClientV1 {
    address: SocketAddr,
    tls: ExecutionFabricClientTlsV1,
    expected_server_principal_sha256: String,
    connect_timeout: Duration,
    io_timeout: Duration,
}

/// Stable failure phase for one Fabric exchange. Acceptance gates begin only
/// after a response exists; connect/request failures remain infrastructure,
/// while an authenticated-but-unpinned peer is specifically Gate 2.
#[derive(Debug, thiserror::Error)]
pub(crate) enum FabricClientFailureV1 {
    #[error("execution-Fabric connection or TLS setup failed: {0:#}")]
    Connection(anyhow::Error),
    #[error("execution-Fabric peer authentication failed: {0:#}")]
    NodeAuthentication(anyhow::Error),
    #[error("execution-Fabric server principal `{actual}` differs from pinned `{expected}`")]
    WrongServerPrincipal { expected: String, actual: String },
    #[error("execution-Fabric request transport failed: {0:#}")]
    RequestTransport(anyhow::Error),
    #[error("execution-Fabric request preparation failed: {0:#}")]
    RequestPreparation(anyhow::Error),
    #[error("execution-Fabric response representation failed: {0:#}")]
    ResponseRepresentation(anyhow::Error),
    #[error("execution-Fabric response transport failed: {0:#}")]
    ResponseTransport(anyhow::Error),
    #[error("execution-Fabric exchange exceeded its monotonic deadline")]
    Deadline,
    #[error("coordinator observation clock failed: {0:#}")]
    CoordinatorClock(anyhow::Error),
}

impl FabricAttemptClientV1 {
    #[cfg(test)]
    pub(crate) fn new(
        address: SocketAddr,
        tls_identity: ClientTlsIdentity,
        expected_server_principal_sha256: impl Into<String>,
        connect_timeout: Duration,
        io_timeout: Duration,
    ) -> Result<Self> {
        let tls = prepare_execution_fabric_client_tls_v1(&tls_identity)
            .context("failed to freeze execution-Fabric client TLS identity")?;
        Ok(Self {
            address,
            tls,
            expected_server_principal_sha256: expected_server_principal_sha256.into(),
            connect_timeout,
            io_timeout,
        })
    }

    pub(crate) fn from_frozen_tls(
        address: SocketAddr,
        tls: ExecutionFabricClientTlsV1,
        expected_server_principal_sha256: impl Into<String>,
        connect_timeout: Duration,
        io_timeout: Duration,
    ) -> Self {
        Self {
            address,
            tls,
            expected_server_principal_sha256: expected_server_principal_sha256.into(),
            connect_timeout,
            io_timeout,
        }
    }

    pub(crate) fn exchange(
        &self,
        request: &FabricRequestV1,
        deadline: Instant,
    ) -> std::result::Result<FabricClientExchangeV1, FabricClientFailureV1> {
        if Instant::now() >= deadline {
            return Err(FabricClientFailureV1::Deadline);
        }
        let prepared = prepare_fabric_client_request_v1(request)
            .map_err(FabricClientFailureV1::RequestPreparation)?;
        if Instant::now() >= deadline {
            return Err(FabricClientFailureV1::Deadline);
        }
        let (mut stream, deadline_guard) = connect_mutual_tls_execution_fabric_v1_until(
            self.address,
            &self.tls,
            self.connect_timeout,
            self.io_timeout,
            deadline,
        )
        .map_err(|error| match error {
            ExecutionFabricTlsConnectFailureV1::Deadline => FabricClientFailureV1::Deadline,
            ExecutionFabricTlsConnectFailureV1::PeerAuthentication(error) => {
                FabricClientFailureV1::NodeAuthentication(error)
            }
            ExecutionFabricTlsConnectFailureV1::Connection(error)
            | ExecutionFabricTlsConnectFailureV1::LocalSetup(error)
            | ExecutionFabricTlsConnectFailureV1::ProtocolNegotiation(error) => {
                FabricClientFailureV1::Connection(error)
            }
        })?;
        let tls_server_principal_sha256 =
            peer_server_principal_sha256(&stream).map_err(|error| {
                FabricClientFailureV1::NodeAuthentication(
                    error
                        .context("execution-Fabric TLS server has no authenticated leaf principal"),
                )
            })?;
        if tls_server_principal_sha256 != self.expected_server_principal_sha256 {
            return Err(FabricClientFailureV1::WrongServerPrincipal {
                expected: self.expected_server_principal_sha256.clone(),
                actual: tls_server_principal_sha256,
            });
        }

        let write_budget = remaining_timeout(self.io_timeout, deadline)?;
        write_prepared_fabric_client_request_v1(&mut stream, &prepared, write_budget.timeout)
            .map_err(|error| {
                if transport_failure_is_deadline(&error, &deadline_guard, write_budget, deadline) {
                    FabricClientFailureV1::Deadline
                } else {
                    FabricClientFailureV1::RequestTransport(error.context(format!(
                        "failed to send Fabric request to `{}`",
                        self.address
                    )))
                }
            })?;
        let read_budget = remaining_timeout(self.io_timeout, deadline)?;
        let response =
            read_fabric_client_response_v1(&mut stream, read_budget.timeout).map_err(|error| {
                match fabric_wire_io_disposition_v1(&error) {
                    Some(FabricWireIoDispositionV1::TruncatedRepresentation) | None => {
                        FabricClientFailureV1::ResponseRepresentation(error.context(format!(
                            "failed to read Fabric response from `{}`",
                            self.address
                        )))
                    }
                    Some(
                        FabricWireIoDispositionV1::Timeout
                        | FabricWireIoDispositionV1::Transport
                        | FabricWireIoDispositionV1::NoResponse,
                    ) if transport_failure_is_deadline(
                        &error,
                        &deadline_guard,
                        read_budget,
                        deadline,
                    ) =>
                    {
                        FabricClientFailureV1::Deadline
                    }
                    Some(
                        FabricWireIoDispositionV1::Timeout
                        | FabricWireIoDispositionV1::Transport
                        | FabricWireIoDispositionV1::NoResponse,
                    ) => FabricClientFailureV1::ResponseTransport(error.context(format!(
                        "failed to receive Fabric response from `{}`",
                        self.address
                    ))),
                }
            })?;
        if Instant::now() >= deadline {
            return Err(FabricClientFailureV1::Deadline);
        }
        let coordinator_observed_unix_ms =
            current_unix_millis().map_err(FabricClientFailureV1::CoordinatorClock)?;

        Ok(FabricClientExchangeV1 {
            response,
            tls_server_principal_sha256,
            coordinator_observed_unix_ms,
        })
    }
}

#[derive(Clone, Copy)]
struct FabricIoBudgetV1 {
    timeout: Duration,
    deadline_limited: bool,
}

fn remaining_timeout(
    configured: Duration,
    deadline: Instant,
) -> std::result::Result<FabricIoBudgetV1, FabricClientFailureV1> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err(FabricClientFailureV1::Deadline);
    }
    Ok(FabricIoBudgetV1 {
        timeout: configured.min(remaining),
        deadline_limited: remaining <= configured,
    })
}

fn transport_failure_is_deadline(
    error: &anyhow::Error,
    deadline_guard: &super::super::tls::FabricTransportDeadlineGuardV1,
    budget: FabricIoBudgetV1,
    deadline: Instant,
) -> bool {
    deadline_guard.expired()
        || (fabric_wire_io_disposition_v1(error) == Some(FabricWireIoDispositionV1::Timeout)
            && budget.deadline_limited
            && Instant::now() >= deadline)
}

fn current_unix_millis() -> Result<u64> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before the Unix epoch")?;
    u64::try_from(elapsed.as_millis()).context("Unix millisecond timestamp exceeds u64")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_client_identity(
        server: &super::super::super::tls::ServerTlsIdentity,
    ) -> ClientTlsIdentity {
        ClientTlsIdentity {
            ca_path: server.client_ca_path.clone(),
            cert_path: server.cert_path.clone(),
            key_path: server.key_path.clone(),
            server_name: "localhost".to_string(),
        }
    }

    #[test]
    fn coordinator_observation_clock_is_nonzero_unix_milliseconds() {
        assert!(current_unix_millis().unwrap() > 0);
    }

    #[test]
    fn wrong_server_pin_sends_zero_fabric_application_bytes() {
        let (_directory, server_identity) =
            super::super::super::tls::test_server_tls_identity().unwrap();
        let server_config =
            super::super::super::tls::test_server_config_without_client_auth_execution_fabric_v1(
                &server_identity,
            )
            .unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (tcp, _) = listener.accept().unwrap();
            let (mut stream, route) =
                super::super::super::tls::accept_mutual_tls_with_execution_fabric_v1(
                    tcp,
                    server_config,
                    Duration::from_secs(5),
                    Duration::from_secs(5),
                )
                .unwrap();
            assert_eq!(
                route,
                super::super::super::tls::HostedTlsRouteV1::ExecutionFabricV1
            );
            let mut byte = [0_u8; 1];
            match std::io::Read::read(&mut stream, &mut byte) {
                Ok(count) => count,
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::UnexpectedEof
                            | std::io::ErrorKind::ConnectionReset
                            | std::io::ErrorKind::BrokenPipe
                    ) =>
                {
                    0
                }
                Err(error) => panic!("unexpected server read failure: {error}"),
            }
        });
        let client = FabricAttemptClientV1::new(
            address,
            test_client_identity(&server_identity),
            "00".repeat(32),
            Duration::from_secs(5),
            Duration::from_secs(5),
        )
        .unwrap();
        let request = super::super::wire::tests::request_fixture();

        let result = client.exchange(
            &request,
            Instant::now().checked_add(Duration::from_secs(5)).unwrap(),
        );

        assert!(matches!(
            result,
            Err(FabricClientFailureV1::WrongServerPrincipal { .. })
        ));
        assert_eq!(server.join().unwrap(), 0);
    }

    #[test]
    fn watchdog_stalled_response_is_deadline_not_representation() {
        let (_directory, server_identity) =
            super::super::super::tls::test_server_tls_identity().unwrap();
        let expected_principal =
            super::super::super::tls::certificate_leaf_sha256(&server_identity.cert_path).unwrap();
        let server_config =
            super::super::super::tls::test_server_config_without_client_auth_execution_fabric_v1(
                &server_identity,
            )
            .unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (tcp, _) = listener.accept().unwrap();
            let (mut stream, route) =
                super::super::super::tls::accept_mutual_tls_with_execution_fabric_v1(
                    tcp,
                    server_config,
                    Duration::from_secs(5),
                    Duration::from_secs(5),
                )
                .unwrap();
            assert_eq!(
                route,
                super::super::super::tls::HostedTlsRouteV1::ExecutionFabricV1
            );
            assert!(super::super::wire::read_fabric_request_v1(&mut stream)
                .unwrap()
                .is_some());
            std::thread::sleep(Duration::from_millis(250));
        });
        let client = FabricAttemptClientV1::new(
            address,
            test_client_identity(&server_identity),
            expected_principal,
            Duration::from_secs(5),
            Duration::from_secs(5),
        )
        .unwrap();
        let request = super::super::wire::tests::request_fixture();
        let deadline = Instant::now()
            .checked_add(Duration::from_millis(75))
            .unwrap();

        let result = client.exchange(&request, deadline);

        assert!(matches!(result, Err(FabricClientFailureV1::Deadline)));
        server.join().unwrap();
    }

    #[test]
    fn raw_peer_eof_before_response_is_transport_infrastructure() {
        let (_directory, server_identity) =
            super::super::super::tls::test_server_tls_identity().unwrap();
        let expected_principal =
            super::super::super::tls::certificate_leaf_sha256(&server_identity.cert_path).unwrap();
        let server_config =
            super::super::super::tls::test_server_config_without_client_auth_execution_fabric_v1(
                &server_identity,
            )
            .unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (tcp, _) = listener.accept().unwrap();
            let (mut stream, _) =
                super::super::super::tls::accept_mutual_tls_with_execution_fabric_v1(
                    tcp,
                    server_config,
                    Duration::from_secs(5),
                    Duration::from_secs(5),
                )
                .unwrap();
            assert!(super::super::wire::read_fabric_request_v1(&mut stream)
                .unwrap()
                .is_some());
        });
        let client = FabricAttemptClientV1::new(
            address,
            test_client_identity(&server_identity),
            expected_principal,
            Duration::from_secs(5),
            Duration::from_secs(5),
        )
        .unwrap();
        let request = super::super::wire::tests::request_fixture();

        let result = client.exchange(
            &request,
            Instant::now().checked_add(Duration::from_secs(5)).unwrap(),
        );

        assert!(matches!(
            result,
            Err(FabricClientFailureV1::ResponseTransport(_))
        ));
        server.join().unwrap();
    }
}
