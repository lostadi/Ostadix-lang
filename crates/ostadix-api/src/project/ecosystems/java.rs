//! Java route discovery (thin, best-effort): Maven and Gradle build routes.

use std::path::Path;

use crate::project::discover::{has_file, EcosystemDiscoverer};
use crate::project::model::{ProjectFile, RouteGuard, RouteKind, RouteProvenance, RouteSpec};

pub struct JavaDiscoverer;

fn build_route(id: &str, evidence: &str, tool: &str, args: &[&str]) -> RouteSpec {
    let mut route = RouteSpec::new(
        id.to_string(),
        RouteProvenance::Discovered {
            ecosystem: "java".to_string(),
            evidence: evidence.to_string(),
        },
    );
    route.kind = RouteKind::BuildTarget;
    route.command = std::iter::once(tool.to_string())
        .chain(args.iter().map(|s| s.to_string()))
        .collect();
    route.label = route.command.join(" ");
    route.provides = vec!["build".to_string()];
    route
        .guards
        .push(RouteGuard::CommandAvailable(tool.to_string()));
    route
}

impl EcosystemDiscoverer for JavaDiscoverer {
    fn name(&self) -> &str {
        "java"
    }

    fn discover(&self, _root: &Path, files: &[ProjectFile]) -> Vec<RouteSpec> {
        let mut routes = Vec::new();
        if has_file(files, "pom.xml") {
            routes.push(build_route(
                "java-maven-build",
                "pom.xml present",
                "mvn",
                &["-q", "package"],
            ));
        }
        if has_file(files, "build.gradle") || has_file(files, "build.gradle.kts") {
            routes.push(build_route(
                "java-gradle-build",
                "build.gradle present",
                "gradle",
                &["build"],
            ));
        }
        routes
    }
}
