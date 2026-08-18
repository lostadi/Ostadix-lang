//! Mutually authenticated hosted-node client.

use std::time::Duration;

use anyhow::{bail, Context, Result};

use super::protocol::{
    read_hosted_frame, write_hosted_frame, HostedOperationReceiptV1, HostedRequestV1,
    HostedResponseV1, NodeDoctorV1, NodeProfileV1, RemotePreparedOperationV1,
    NODE_DOCTOR_SCHEMA_V1,
};
use super::tls::{
    connect_mutual_tls, ClientTlsIdentity, DEFAULT_CONNECT_TIMEOUT, DEFAULT_IO_TIMEOUT,
};

pub const DEFAULT_NODE_ADDRESS: &str = "127.0.0.1:7337";
pub const DEFAULT_TLS_SERVER_NAME: &str = "localhost";

#[derive(Debug, Clone)]
pub struct HostedNodeClient {
    pub address: String,
    pub tls_identity: ClientTlsIdentity,
    pub connect_timeout: Duration,
    pub io_timeout: Duration,
}

impl HostedNodeClient {
    pub fn new(address: impl Into<String>, tls_identity: ClientTlsIdentity) -> Self {
        Self {
            address: address.into(),
            tls_identity,
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
            io_timeout: DEFAULT_IO_TIMEOUT,
        }
    }

    pub fn profile(&self) -> Result<NodeProfileV1> {
        match self.request(HostedRequestV1::profile())? {
            HostedResponseV1::Profile { profile } => {
                profile.validate()?;
                Ok(profile)
            }
            HostedResponseV1::Error { error } => {
                bail!(
                    "node rejected profile request [{}]: {}",
                    error.code,
                    error.message
                )
            }
            other => bail!("node returned wrong response to profile request: {other:?}"),
        }
    }

    pub fn doctor(&self) -> Result<NodeDoctorV1> {
        match self.request(HostedRequestV1::doctor())? {
            HostedResponseV1::Doctor { doctor } => {
                if doctor.schema != NODE_DOCTOR_SCHEMA_V1 {
                    bail!(
                        "node returned unsupported doctor schema `{}`",
                        doctor.schema
                    );
                }
                Ok(doctor)
            }
            HostedResponseV1::Error { error } => {
                bail!(
                    "node rejected doctor request [{}]: {}",
                    error.code,
                    error.message
                )
            }
            other => bail!("node returned wrong response to doctor request: {other:?}"),
        }
    }

    pub fn run(&self, operation: RemotePreparedOperationV1) -> Result<HostedOperationReceiptV1> {
        operation.validate_structure()?;
        let expected_operation_sha256 = operation.operation_sha256()?;
        let task_id = operation.task_id.clone();
        let attempt_id = operation.attempt_id.clone();
        match self.request(HostedRequestV1::run(operation))? {
            HostedResponseV1::Run { receipt } => {
                receipt.validate()?;
                if receipt.task_id != task_id || receipt.attempt_id != attempt_id {
                    bail!("node receipt task/attempt identity does not match the request");
                }
                if receipt.operation_sha256 != expected_operation_sha256 {
                    bail!("node receipt operation digest does not match the request");
                }
                Ok(receipt)
            }
            HostedResponseV1::Error { error } => {
                bail!(
                    "node rejected run request [{}]: {}",
                    error.code,
                    error.message
                )
            }
            other => bail!("node returned wrong response to run request: {other:?}"),
        }
    }

    pub fn request(&self, request: HostedRequestV1) -> Result<HostedResponseV1> {
        request.validate()?;
        let mut stream = connect_mutual_tls(
            &self.address,
            &self.tls_identity,
            self.connect_timeout,
            self.io_timeout,
        )?;
        write_hosted_frame(&mut stream, &request)
            .with_context(|| format!("failed to send hosted request to `{}`", self.address))?;
        read_hosted_frame(&mut stream)?
            .with_context(|| format!("node `{}` closed before returning a response", self.address))
    }
}
