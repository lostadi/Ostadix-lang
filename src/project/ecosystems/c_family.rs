//! C / C++ route discovery (thin, best-effort).
//!
//! When a `CMakeLists.txt` is present a `cmake --build` route is offered;
//! otherwise nothing is discovered here (Makefile-based C projects are handled
//! by the shell discoverer).

use std::path::Path;

use crate::project::discover::{has_file, EcosystemDiscoverer};
use crate::project::model::{ProjectFile, RouteGuard, RouteKind, RouteProvenance, RouteSpec};

pub struct CFamilyDiscoverer;

impl EcosystemDiscoverer for CFamilyDiscoverer {
    fn name(&self) -> &str {
        "c_family"
    }

    fn discover(&self, _root: &Path, files: &[ProjectFile]) -> Vec<RouteSpec> {
        let mut routes = Vec::new();
        if has_file(files, "CMakeLists.txt") {
            let mut route = RouteSpec::new(
                "cmake-build".to_string(),
                RouteProvenance::Discovered {
                    ecosystem: "c_family".to_string(),
                    evidence: "CMakeLists.txt present".to_string(),
                },
            );
            route.kind = RouteKind::BuildTarget;
            route.command = vec![
                "cmake".to_string(),
                "-S".to_string(),
                ".".to_string(),
                "-B".to_string(),
                "build".to_string(),
            ];
            route.label = "cmake configure".to_string();
            route.provides = vec!["build".to_string()];
            route.guards.push(RouteGuard::CommandAvailable("cmake".to_string()));
            routes.push(route);
        }
        routes
    }
}
