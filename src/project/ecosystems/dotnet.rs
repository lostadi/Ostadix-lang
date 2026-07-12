//! .NET route discovery (thin, best-effort): `dotnet run` when a project or
//! solution file is present.

use std::path::Path;

use crate::project::discover::EcosystemDiscoverer;
use crate::project::model::{ProjectFile, RouteGuard, RouteKind, RouteProvenance, RouteSpec};

pub struct DotnetDiscoverer;

impl EcosystemDiscoverer for DotnetDiscoverer {
    fn name(&self) -> &str {
        "dotnet"
    }

    fn discover(&self, _root: &Path, files: &[ProjectFile]) -> Vec<RouteSpec> {
        let has_project = files
            .iter()
            .any(|f| f.path.ends_with(".csproj") || f.path.ends_with(".fsproj") || f.path.ends_with(".sln"));
        if !has_project {
            return Vec::new();
        }
        let mut route = RouteSpec::new(
            "dotnet-run".to_string(),
            RouteProvenance::Discovered {
                ecosystem: "dotnet".to_string(),
                evidence: "a .NET project/solution file is present".to_string(),
            },
        );
        route.kind = RouteKind::CompiledBinary;
        route.command = vec!["dotnet".to_string(), "run".to_string()];
        route.label = "dotnet run".to_string();
        route.provides = vec!["main".to_string()];
        route.guards.push(RouteGuard::CommandAvailable("dotnet".to_string()));
        vec![route]
    }
}
