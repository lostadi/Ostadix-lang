//! Machine-readable Ostadix package and compatibility coordinates.
//!
//! These fields are descriptive. They do not prove that a backend executable
//! is installed, that a placement is authorized, or that a World is live.

use serde::Serialize;

use crate::backend_catalog::BACKEND_CATALOG_CURRENT_SCHEMA;
use crate::evidence::{
    ADMISSION_SCHEMA_V6, ANALYZER_ID_V6, EVIDENCE_SCHEMA_V6, EXECUTION_INTENT_SCHEMA_V1,
};
use crate::hosted_remote::v2::HOSTED_PROTOCOL_V2;
use crate::hosted_remote::{HOSTED_PROTOCOL_V1, HOSTED_TLS_ALPN_V1, HOSTED_TLS_ALPN_V2};
use crate::world::{
    IDENTITY_WIRE_VERSION, OVALUE_WIRE_SCHEMA_V1, WORLD_RECEIPT_SCHEMA_V1, WORLD_SCHEMA_V1,
    WORLD_WIRE_CODEC_VERSION,
};

pub const VERSION_REPORT_SCHEMA_V1: &str = "ostadix.version-report/v1";
/// Package coordinate frozen into the V1 compatibility report.
pub const VERSION_REPORT_PACKAGE_NAME_V1: &str = "o-lang";
/// The crate that now owns the runtime implementation.
pub const ENGINE_PACKAGE_NAME: &str = "ostadix-api";
pub const RELEASE_RUST_TOOLCHAIN: &str = "1.97.1";

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct OstadixVersionReportV1 {
    pub schema: &'static str,
    pub package_name: &'static str,
    pub package_version: &'static str,
    pub minimum_rust_version: &'static str,
    pub release_rust_toolchain: String,
    pub admission_schema: &'static str,
    pub evidence_schema: &'static str,
    pub evidence_analyzer: &'static str,
    pub execution_intent_schema: &'static str,
    pub backend_catalog_schema: &'static str,
    pub hosted_transport_protocols: [&'static str; 2],
    pub hosted_tls_alpn: [String; 2],
    pub world_schema: u16,
    pub world_wire_codec: u16,
    pub world_identity_wire: u16,
    pub world_value_wire: u16,
    pub world_receipt_wire: u16,
    pub graph_executor_enabled: bool,
    pub notebook_enabled: bool,
}

impl OstadixVersionReportV1 {
    pub fn current() -> Self {
        Self {
            schema: VERSION_REPORT_SCHEMA_V1,
            package_name: VERSION_REPORT_PACKAGE_NAME_V1,
            package_version: env!("CARGO_PKG_VERSION"),
            minimum_rust_version: env!("CARGO_PKG_RUST_VERSION"),
            release_rust_toolchain: release_rust_toolchain(),
            admission_schema: ADMISSION_SCHEMA_V6,
            evidence_schema: EVIDENCE_SCHEMA_V6,
            evidence_analyzer: ANALYZER_ID_V6,
            execution_intent_schema: EXECUTION_INTENT_SCHEMA_V1,
            backend_catalog_schema: BACKEND_CATALOG_CURRENT_SCHEMA,
            hosted_transport_protocols: [HOSTED_PROTOCOL_V1, HOSTED_PROTOCOL_V2],
            hosted_tls_alpn: [
                String::from_utf8_lossy(HOSTED_TLS_ALPN_V1).into_owned(),
                String::from_utf8_lossy(HOSTED_TLS_ALPN_V2).into_owned(),
            ],
            world_schema: WORLD_SCHEMA_V1,
            world_wire_codec: WORLD_WIRE_CODEC_VERSION,
            world_identity_wire: IDENTITY_WIRE_VERSION,
            world_value_wire: OVALUE_WIRE_SCHEMA_V1,
            world_receipt_wire: WORLD_RECEIPT_SCHEMA_V1,
            graph_executor_enabled: cfg!(feature = "graph_executor"),
            notebook_enabled: cfg!(feature = "notebook"),
        }
    }
}

fn release_rust_toolchain() -> String {
    RELEASE_RUST_TOOLCHAIN.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_keeps_independent_version_axes_explicit() {
        let report = OstadixVersionReportV1::current();
        assert_eq!(report.schema, VERSION_REPORT_SCHEMA_V1);
        assert_eq!(report.package_name, VERSION_REPORT_PACKAGE_NAME_V1);
        assert_eq!(ENGINE_PACKAGE_NAME, env!("CARGO_PKG_NAME"));
        assert_eq!(report.package_version, "0.4.0");
        assert_eq!(report.minimum_rust_version, "1.93.1");
        assert_eq!(report.release_rust_toolchain, "1.97.1");
        assert_ne!(report.admission_schema, report.execution_intent_schema);
        assert_ne!(
            report.hosted_transport_protocols[0],
            report.hosted_transport_protocols[1]
        );
    }
}
