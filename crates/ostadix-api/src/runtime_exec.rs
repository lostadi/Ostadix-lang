//! Evidence-bound direct executable entrypoints.
//!
//! Admission selects one complete catalog alternative for every shim-backed
//! backend, opens and hashes those executables once, and retains the open files
//! until execution completes. Dispatch consumes admitted absolute invocation
//! paths from this manifest instead of resolving command names through `PATH`
//! again; each invocation name is bound to its opened and hashed canonical
//! target so multicall symlinks keep their required `argv[0]` behavior.
//!
//! This is deliberately a *direct-launcher* guarantee.  It does not claim to
//! bind interpreters selected by shebangs, compiler drivers' subordinate tools,
//! dynamic libraries, or descendants launched by user code. The retained
//! handle plus path/file-identity checks close ambient `PATH` reselection and
//! detect drift immediately before spawn. They do not make executable bytes
//! immutable or eliminate the final same-principal verification-to-path-exec
//! micro-window for foreign launchers. On Linux, the runtime-owned O backend
//! proxy is executed opportunistically through its retained `/proc` file
//! descriptor while preserving the admitted invocation name as `argv[0]`.
//! That closes pathname substitution for the proxy without copying bytes or
//! changing worker capacity. Foreign launchers remain path-executed because
//! ELF magic alone does not prove that `$ORIGIN`, `AT_EXECFN`, or self-location
//! behavior is descriptor-compatible.
//! On non-Unix targets, where the standard library exposes no equivalent
//! stable file identity, launch remains available under a weaker guarantee:
//! the canonical target is re-hashed immediately before each spawn.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs::{self, File, Metadata};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::backend_catalog::{
    backend_executable_set_v2 as project_backend_executable_set_v2, BackendExecutableSelectionV2,
    BackendExecutableSetRowV2,
};
use crate::backend_catalog::{BackendAdapterKind, BackendRegistry, ExecutionMode};
use crate::ir::{ExecutionPlan, PlanNodeKind};
use crate::placement::SemanticDigestV1;
use crate::resource_identity::ArtifactId;

pub const EXECUTABLE_MANIFEST_SCHEMA_V1: &str = "oexec.direct-executable-manifest/v1";
pub const ADMITTED_EXECUTABLE_MANIFEST_ENV: &str = "O_ADMITTED_EXECUTABLE_MANIFEST";
pub const ADMITTED_PROXY_EXECUTION_ENV: &str = "O_ADMITTED_PROXY_EXECUTION";
const LINUX_PROC_FD_PROXY_EXECUTION_V1: &str = "linux-procfd-open-object/v1";
pub const CURRENT_O_LOGICAL_COMMAND: &str = "__ostadix_current_executable__";
pub const SANDBOX_EXEC_LOGICAL_COMMAND: &str = "__sandbox_exec__";

/// Resolve and validate a supported native executable image. This is a
/// format-and-permission preflight, not an O backend-protocol or ABI probe.
/// Shell dispatchers are not acceptable here: hashing a wrapper would leave
/// the executable it selects outside the admitted artifact identity.
pub fn validate_native_runtime_binary(path: &Path) -> Result<PathBuf> {
    let metadata = path
        .metadata()
        .with_context(|| format!("could not inspect `{}`", path.display()))?;
    if !metadata.is_file() {
        bail!("`{}` is not a regular file", path.display());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        if metadata.permissions().mode() & 0o111 == 0 {
            bail!("`{}` is not executable", path.display());
        }
    }
    let canonical = path
        .canonicalize()
        .with_context(|| format!("could not canonicalize `{}`", path.display()))?;
    let file = File::open(&canonical)
        .with_context(|| format!("could not open `{}`", canonical.display()))?;
    ensure_native_executable_image(&file, &canonical)?;
    Ok(canonical)
}

fn has_native_executable_magic(prefix: &[u8]) -> bool {
    #[cfg(target_os = "macos")]
    return matches!(
        prefix,
        [0xfe, 0xed, 0xfa, 0xce]
            | [0xce, 0xfa, 0xed, 0xfe]
            | [0xfe, 0xed, 0xfa, 0xcf]
            | [0xcf, 0xfa, 0xed, 0xfe]
            | [0xca, 0xfe, 0xba, 0xbe]
            | [0xbe, 0xba, 0xfe, 0xca]
            | [0xca, 0xfe, 0xba, 0xbf]
            | [0xbf, 0xba, 0xfe, 0xca]
    );

    #[cfg(windows)]
    return prefix.starts_with(b"MZ");

    #[cfg(all(unix, not(target_os = "macos")))]
    return prefix.starts_with(b"\x7fELF");

    #[cfg(not(any(unix, windows)))]
    false
}

fn ensure_native_executable_image(file: &File, path: &Path) -> Result<()> {
    use std::io::{Read, Seek, SeekFrom};

    let mut reader = file
        .try_clone()
        .with_context(|| format!("could not inspect `{}`", path.display()))?;
    reader
        .seek(SeekFrom::Start(0))
        .with_context(|| format!("could not inspect `{}`", path.display()))?;
    let mut prefix = [0_u8; 4];
    let read = reader
        .read(&mut prefix)
        .with_context(|| format!("could not inspect `{}`", path.display()))?;
    if !has_native_executable_magic(&prefix[..read]) {
        bail!(
            "`{}` is a script or unsupported executable format",
            path.display()
        );
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ExecutableGuaranteeV1 {
    /// The bytes were hashed once at capture; retained-file and canonical-path
    /// identities are compared immediately before each direct spawn.
    DirectLauncherPathAndOpenFileIdentity,
    /// Portable fallback for platforms without stable Unix file identity.
    /// The canonical target is re-hashed immediately before every launch.
    DirectLauncherContentHashImmediatelyBeforeLaunch,
    /// Inspection names a requirement without looking at the host filesystem.
    InspectionNotProbed,
}

impl ExecutableGuaranteeV1 {
    pub const fn name(self) -> &'static str {
        match self {
            Self::DirectLauncherPathAndOpenFileIdentity => {
                "direct-launcher-path-and-open-file-identity"
            }
            Self::DirectLauncherContentHashImmediatelyBeforeLaunch => {
                "direct-launcher-content-hash-immediately-before-launch"
            }
            Self::InspectionNotProbed => "inspection-not-probed",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ExecutableArtifactStateV1 {
    LocatedHashed,
    NotProbed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ExecutableSelectionV1 {
    CompleteCatalogAlternative,
    AdapterDirectLauncherRefinement,
    NotSelected,
}

impl ExecutableSelectionV1 {
    pub const fn name(self) -> &'static str {
        match self {
            Self::CompleteCatalogAlternative => "complete-catalog-alternative",
            Self::AdapterDirectLauncherRefinement => "adapter-direct-launcher-refinement",
            Self::NotSelected => "not-selected",
        }
    }

    /// Project an execution-manifest selection into the path-independent
    /// implementation-identity vocabulary. Inspection rows deliberately have
    /// no such projection.
    pub const fn implementation_selection_v2(self) -> Option<BackendExecutableSelectionV2> {
        match self {
            Self::CompleteCatalogAlternative => {
                Some(BackendExecutableSelectionV2::CompleteCatalogAlternative)
            }
            Self::AdapterDirectLauncherRefinement => {
                Some(BackendExecutableSelectionV2::AdapterDirectLauncherRefinement)
            }
            Self::NotSelected => None,
        }
    }
}

/// One resolved direct-launch alternative shared by runtime admission and
/// local registry discovery. This describes filesystem resolution; the V2
/// semantic projector later removes paths while retaining exact content.
#[derive(Clone, Debug)]
pub struct ResolvedBackendLaunchSelectionV1 {
    requirement_key: String,
    selected_alternative: usize,
    selection: ExecutableSelectionV1,
    direct_commands: Vec<(String, PathBuf)>,
}

impl ResolvedBackendLaunchSelectionV1 {
    pub fn requirement_key(&self) -> &str {
        &self.requirement_key
    }

    pub const fn selected_alternative(&self) -> usize {
        self.selected_alternative
    }

    pub const fn selection(&self) -> ExecutableSelectionV1 {
        self.selection
    }

    pub fn direct_commands(&self) -> &[(String, PathBuf)] {
        &self.direct_commands
    }
}

impl ExecutableArtifactStateV1 {
    pub const fn name(&self) -> &'static str {
        match self {
            Self::LocatedHashed => "located-hashed",
            Self::NotProbed => "not-probed",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutableFileIdentityV1 {
    pub device: u64,
    pub inode: u64,
    pub size: u64,
    pub mode: u32,
    pub mtime_seconds: i64,
    pub mtime_nanoseconds: i64,
    pub ctime_seconds: i64,
    pub ctime_nanoseconds: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutableArtifactV1 {
    pub canonical_backend: String,
    pub requirement_key: String,
    pub selected_alternative: Option<usize>,
    /// Whether the selected rows cover a complete catalog alternative or a
    /// deliberately narrower direct-launcher projection.
    pub selection: ExecutableSelectionV1,
    pub logical_command: String,
    /// `direct-launcher`, `ostadix-proxy`, or `sandbox-wrapper`.
    pub role: String,
    pub state: ExecutableArtifactStateV1,
    /// Absolute path passed to `exec`. This preserves multicall symlink names
    /// such as `rustc -> rustup` while `canonical_path` identifies the opened
    /// and hashed target bytes.
    pub invocation_path: Option<PathBuf>,
    pub canonical_path: Option<PathBuf>,
    pub invocation_identity: String,
    pub invocation_file_identity: Option<ExecutableFileIdentityV1>,
    pub resolved_identity: String,
    pub sha256: Option<String>,
    pub file_identity: Option<ExecutableFileIdentityV1>,
    pub guarantee: ExecutableGuaranteeV1,
}

#[derive(Clone, Copy)]
struct ArtifactSelection<'a> {
    requirement_key: &'a str,
    selected_alternative: Option<usize>,
    selection: ExecutableSelectionV1,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutableManifestV1 {
    pub schema: String,
    /// This manifest binds direct launch entrypoints, not their transitive
    /// runtime closure.
    pub scope: String,
    pub artifacts: Vec<ExecutableArtifactV1>,
    pub sha256: String,
}

impl ExecutableManifestV1 {
    pub fn artifacts(&self) -> &[ExecutableArtifactV1] {
        &self.artifacts
    }

    pub fn sha256(&self) -> &str {
        &self.sha256
    }

    pub(crate) fn validate_execution(&self) -> Result<()> {
        validate_decoded_manifest(self, None)
    }

    pub(crate) fn validate_inspection(&self) -> Result<()> {
        validate_manifest_envelope(self, None)?;
        for artifact in &self.artifacts {
            validate_inspection_artifact_shape(artifact)?;
        }
        Ok(())
    }

    pub(crate) fn finish(mut artifacts: Vec<ExecutableArtifactV1>) -> Self {
        artifacts.sort_by(|left, right| {
            (
                &left.canonical_backend,
                &left.requirement_key,
                left.selected_alternative,
                left.selection.name(),
                &left.role,
                &left.logical_command,
            )
                .cmp(&(
                    &right.canonical_backend,
                    &right.requirement_key,
                    right.selected_alternative,
                    right.selection.name(),
                    &right.role,
                    &right.logical_command,
                ))
        });
        let sha256 = manifest_sha256(&artifacts);
        Self {
            schema: EXECUTABLE_MANIFEST_SCHEMA_V1.to_string(),
            scope: "direct-launcher-only".to_string(),
            artifacts,
            sha256,
        }
    }
}

#[derive(Debug)]
struct RetainedExecutable {
    path: PathBuf,
    file: File,
    identity: ExecutableFileIdentityV1,
    sha256: String,
    #[cfg(target_os = "linux")]
    is_elf: bool,
}

/// Process-local launch authority retained by `AdmittedExecution`.
///
/// Equality is intentionally based on the evidence-visible manifest.  Open
/// file descriptors are authority handles, not canonical evidence bytes.
#[derive(Debug)]
pub struct ExecutableLeaseSet {
    manifest: ExecutableManifestV1,
    retained: BTreeMap<PathBuf, RetainedExecutable>,
    backend_artifacts: HashMap<String, Vec<usize>>,
    backend_digests: BTreeMap<String, String>,
}

impl PartialEq for ExecutableLeaseSet {
    fn eq(&self, other: &Self) -> bool {
        self.manifest == other.manifest
    }
}

impl Eq for ExecutableLeaseSet {}

impl ExecutableLeaseSet {
    pub fn manifest(&self) -> &ExecutableManifestV1 {
        &self.manifest
    }

    pub fn verify_all(&self) -> Result<()> {
        for artifact in &self.manifest.artifacts {
            self.verify_artifact(artifact)?;
        }
        Ok(())
    }

    pub fn verify_backend(&self, backend: &str) -> Result<()> {
        let indices = self.backend_artifacts.get(backend).with_context(|| {
            format!("no admitted direct executable manifest for backend `{backend}`")
        })?;
        for &index in indices {
            self.verify_artifact(&self.manifest.artifacts[index])?;
        }
        Ok(())
    }

    pub fn command_path(&self, backend: &str, logical_command: &str) -> Result<&Path> {
        let artifact = self.artifact(backend, logical_command).with_context(|| {
            format!("backend `{backend}` has no admitted direct executable `{logical_command}`")
        })?;
        self.verify_artifact(artifact)?;
        artifact
            .invocation_path
            .as_deref()
            .context("admitted direct executable has no invocation path")
    }

    pub fn current_o_path(&self) -> Result<&Path> {
        self.unique_reserved_path(CURRENT_O_LOGICAL_COMMAND)
    }

    /// Return the admitted invocation spelling for compatibility metadata.
    ///
    /// This is deliberately not an execution-authority check. Linux procfd
    /// launch uses it only to preserve `argv[0]` and to derive a fallback
    /// runtime-root hint without reopening the pathname before dispatch.
    pub fn current_o_invocation_path(&self) -> Result<&Path> {
        self.unique_reserved_artifact(CURRENT_O_LOGICAL_COMMAND)?
            .invocation_path
            .as_deref()
            .context("admitted O proxy has no invocation path")
    }

    /// Build the admitted O backend-proxy command.
    ///
    /// Linux opportunistically executes the already-open runtime-owned ELF
    /// object through procfs. This is zero-copy and preserves the admitted
    /// invocation path as `argv[0]`. If procfs is unavailable, or the current
    /// O image is not ELF, dispatch automatically retains the compatible
    /// admitted-path behavior.
    pub fn current_o_command(&self) -> Result<Command> {
        #[cfg(target_os = "linux")]
        {
            self.current_o_command_with_proc_root(Path::new("/proc"))
        }

        #[cfg(not(target_os = "linux"))]
        {
            let artifact = self.unique_reserved_artifact(CURRENT_O_LOGICAL_COMMAND)?;
            self.verify_artifact_with_proxy_procfd(artifact, false)?;
            let invocation_path = artifact
                .invocation_path
                .as_deref()
                .context("admitted O proxy has no invocation path")?;
            let mut command = Command::new(invocation_path);
            command.env_remove(ADMITTED_PROXY_EXECUTION_ENV);
            Ok(command)
        }
    }

    #[cfg(target_os = "linux")]
    fn current_o_command_with_proc_root(&self, proc_root: &Path) -> Result<Command> {
        let artifact = self.unique_reserved_artifact(CURRENT_O_LOGICAL_COMMAND)?;
        let invocation_path = artifact
            .invocation_path
            .as_deref()
            .context("admitted O proxy has no invocation path")?;

        if let Some(procfd_path) = self.linux_proxy_procfd_under(artifact, proc_root)? {
            self.verify_artifact_with_proxy_procfd(artifact, true)?;
            use std::os::unix::process::CommandExt;

            let mut command = Command::new(procfd_path);
            command.arg0(invocation_path).env(
                ADMITTED_PROXY_EXECUTION_ENV,
                LINUX_PROC_FD_PROXY_EXECUTION_V1,
            );
            return Ok(command);
        }

        // Procfs is optional. Fall back to the compatible path launch only
        // after revalidating the pathname as path-mode execution authority.
        self.verify_artifact_with_proxy_procfd(artifact, false)?;
        let mut command = Command::new(invocation_path);
        command.env_remove(ADMITTED_PROXY_EXECUTION_ENV);
        Ok(command)
    }

    pub fn sandbox_exec_path(&self) -> Result<Option<&Path>> {
        let artifacts = self
            .manifest
            .artifacts
            .iter()
            .filter(|artifact| artifact.logical_command == SANDBOX_EXEC_LOGICAL_COMMAND)
            .collect::<Vec<_>>();
        let Some(artifact) = artifacts.first().copied() else {
            return Ok(None);
        };
        for candidate in &artifacts {
            if candidate.invocation_path != artifact.invocation_path
                || candidate.canonical_path != artifact.canonical_path
                || candidate.sha256 != artifact.sha256
                || candidate.file_identity != artifact.file_identity
            {
                bail!("admission contains conflicting sandbox-exec identities");
            }
        }
        self.verify_artifact(artifact)?;
        artifact
            .invocation_path
            .as_deref()
            .map(Some)
            .context("admitted sandbox-exec executable has no invocation path")
    }

    pub fn backend_manifest_json(&self, backend: &str) -> Result<String> {
        let indices = self.backend_artifacts.get(backend).with_context(|| {
            format!("no admitted direct executable manifest for backend `{backend}`")
        })?;
        let artifacts = indices
            .iter()
            .map(|&index| self.manifest.artifacts[index].clone())
            .collect::<Vec<_>>();
        serde_json::to_string(&ExecutableManifestV1::finish(artifacts))
            .context("failed to encode admitted executable manifest for backend child")
    }

    pub fn backend_executable_set_sha256(&self, backend: &str) -> Result<&str> {
        self.backend_digests
            .get(backend)
            .map(String::as_str)
            .with_context(|| format!("no admitted executable-set digest for backend `{backend}`"))
    }

    /// Path-independent V2 executable-set identity projected from the exact
    /// launch-bound rows retained by this admission.
    pub fn backend_executable_set_v2(&self, backend: &str) -> Result<SemanticDigestV1> {
        backend_executable_set_v2_from_manifest(&self.manifest, backend)
    }

    fn artifact(&self, backend: &str, logical: &str) -> Option<&ExecutableArtifactV1> {
        self.manifest.artifacts.iter().find(|artifact| {
            artifact.canonical_backend == backend && artifact.logical_command == logical
        })
    }

    fn unique_reserved_artifact(&self, logical: &str) -> Result<&ExecutableArtifactV1> {
        let artifacts = self
            .manifest
            .artifacts
            .iter()
            .filter(|artifact| artifact.logical_command == logical)
            .collect::<Vec<_>>();
        let artifact = artifacts
            .first()
            .copied()
            .with_context(|| format!("admission has no `{logical}` executable"))?;
        for candidate in &artifacts {
            if candidate.invocation_path != artifact.invocation_path
                || candidate.canonical_path != artifact.canonical_path
                || candidate.sha256 != artifact.sha256
                || candidate.file_identity != artifact.file_identity
            {
                bail!("admission contains conflicting `{logical}` executable identities");
            }
        }
        Ok(artifact)
    }

    fn unique_reserved_path(&self, logical: &str) -> Result<&Path> {
        let artifact = self.unique_reserved_artifact(logical)?;
        self.verify_artifact(artifact)?;
        artifact
            .invocation_path
            .as_deref()
            .with_context(|| format!("admitted `{logical}` executable has no invocation path"))
    }

    fn verify_artifact(&self, artifact: &ExecutableArtifactV1) -> Result<()> {
        #[cfg(target_os = "linux")]
        let retained_proxy_procfd = self.linux_proxy_procfd(artifact)?.is_some();
        #[cfg(not(target_os = "linux"))]
        let retained_proxy_procfd = false;
        self.verify_artifact_with_proxy_procfd(artifact, retained_proxy_procfd)
    }

    fn verify_artifact_with_proxy_procfd(
        &self,
        artifact: &ExecutableArtifactV1,
        retained_proxy_procfd: bool,
    ) -> Result<()> {
        if artifact.state != ExecutableArtifactStateV1::LocatedHashed {
            bail!(
                "executable `{}` for backend `{}` was not execution-bound",
                artifact.logical_command,
                artifact.canonical_backend
            );
        }
        let path = artifact
            .canonical_path
            .as_ref()
            .context("execution-bound executable lacks a canonical path")?;
        let expected = artifact
            .file_identity
            .as_ref()
            .context("execution-bound executable lacks a file identity")?;
        let retained = self
            .retained
            .get(path)
            .with_context(|| format!("no retained executable handle for {}", path.display()))?;
        if !retained_proxy_procfd {
            verify_invocation_path(artifact, expected)?;
        }
        // Check the name-to-object binding first. Replacing an executable by
        // rename can also change ctime on the still-open inode when its old
        // directory entry is unlinked; diagnosing the path substitution is
        // both more precise and independent of that retained-inode detail.
        if !retained_proxy_procfd {
            let path_identity = file_identity(&fs::metadata(path).with_context(|| {
                format!("failed to stat admitted executable path {}", path.display())
            })?)?;
            if &path_identity != expected {
                bail!(
                    "admitted direct executable path was replaced before launch: backend `{}` command `{}` path {}",
                    artifact.canonical_backend,
                    artifact.logical_command,
                    path.display()
                );
            }
        }
        let handle_identity = file_identity(&retained.file.metadata().with_context(|| {
            format!(
                "failed to stat retained executable handle {}",
                retained.path.display()
            )
        })?)?;
        if (retained_proxy_procfd && !same_open_object_identity(&handle_identity, expected))
            || (!retained_proxy_procfd && &handle_identity != expected)
            || &retained.identity != expected
        {
            bail!(
                "admitted direct executable changed through retained handle: backend `{}` command `{}`",
                artifact.canonical_backend,
                artifact.logical_command
            );
        }
        if !retained_proxy_procfd {
            verify_content_if_required(artifact, path)?;
        }
        Ok(())
    }

    #[cfg(target_os = "linux")]
    fn linux_proxy_procfd(&self, artifact: &ExecutableArtifactV1) -> Result<Option<PathBuf>> {
        self.linux_proxy_procfd_under(artifact, Path::new("/proc"))
    }

    #[cfg(target_os = "linux")]
    fn linux_proxy_procfd_under(
        &self,
        artifact: &ExecutableArtifactV1,
        proc_root: &Path,
    ) -> Result<Option<PathBuf>> {
        if artifact.role != "ostadix-proxy" {
            return Ok(None);
        }
        let canonical_path = artifact
            .canonical_path
            .as_ref()
            .context("execution-bound O proxy lacks a canonical path")?;
        let expected = artifact
            .file_identity
            .as_ref()
            .context("execution-bound O proxy lacks a file identity")?;
        let retained = self.retained.get(canonical_path).with_context(|| {
            format!(
                "no retained O proxy handle for {}",
                canonical_path.display()
            )
        })?;
        if !retained.is_elf {
            return Ok(None);
        }
        use std::os::fd::AsRawFd;
        let procfd = proc_root
            .join(std::process::id().to_string())
            .join("fd")
            .join(retained.file.as_raw_fd().to_string());
        let Ok(metadata) = fs::metadata(&procfd) else {
            return Ok(None);
        };
        if !same_open_object_identity(&file_identity(&metadata)?, expected) {
            return Ok(None);
        }
        Ok(Some(procfd))
    }
}

/// Project one backend's launch-bound manifest rows into the shared semantic
/// executable-set identity used by registry publication and placement
/// preflight. Unprobed, unhashed, malformed, or duplicate rows fail before a
/// digest is returned.
pub fn backend_executable_set_v2_from_manifest(
    manifest: &ExecutableManifestV1,
    backend: &str,
) -> Result<SemanticDigestV1> {
    validate_decoded_manifest(manifest, None)
        .context("cannot project an invalid execution manifest")?;
    let artifacts = manifest
        .artifacts
        .iter()
        .filter(|artifact| artifact.canonical_backend == backend)
        .collect::<Vec<_>>();
    if artifacts.is_empty() {
        bail!("execution manifest contains no launch rows for backend `{backend}`");
    }

    let rows = artifacts
        .into_iter()
        .map(|artifact| {
            let selected_alternative = artifact
                .selected_alternative
                .context("launch-bound executable has no selected alternative")?;
            let selected_alternative = u32::try_from(selected_alternative)
                .context("selected executable alternative exceeds the V2 coordinate range")?;
            let selection = artifact
                .selection
                .implementation_selection_v2()
                .context("unprobed executable selection cannot enter implementation identity")?;
            let sha256 = artifact
                .sha256
                .as_deref()
                .context("launch-bound executable has no content SHA-256")?;
            let artifact_id = ArtifactId::from_sha256(sha256)
                .context("launch-bound executable has an invalid content SHA-256")?;
            BackendExecutableSetRowV2::new(
                artifact.requirement_key.clone(),
                selected_alternative,
                selection,
                artifact.logical_command.clone(),
                artifact.role.clone(),
                artifact_id,
            )
            .map_err(anyhow::Error::from)
        })
        .collect::<Result<Vec<_>>>()?;
    project_backend_executable_set_v2(rows).map_err(Into::into)
}

/// Child-process projection used by this crate's `backend.rs`. It independently
/// revalidates the admitted rows immediately before every direct launch.
#[derive(Debug)]
pub struct BackendToolchain {
    backend: String,
    manifest: ExecutableManifestV1,
    linux_procfd_proxy: bool,
}

impl BackendToolchain {
    pub fn from_env(backend: &str) -> Result<Self> {
        let raw = std::env::var(ADMITTED_EXECUTABLE_MANIFEST_ENV).with_context(|| {
            format!(
                "backend `{backend}` has no admitted direct executable manifest in {ADMITTED_EXECUTABLE_MANIFEST_ENV}"
            )
        })?;
        let manifest: ExecutableManifestV1 =
            serde_json::from_str(&raw).context("invalid admitted direct executable manifest")?;
        let linux_procfd_proxy = cfg!(target_os = "linux")
            && std::env::var(ADMITTED_PROXY_EXECUTION_ENV).ok().as_deref()
                == Some(LINUX_PROC_FD_PROXY_EXECUTION_V1);
        validate_backend_process_manifest(&manifest, backend, linux_procfd_proxy)?;
        Ok(Self {
            backend: backend.to_string(),
            manifest,
            linux_procfd_proxy,
        })
    }

    pub fn command_path(&self, logical_command: &str) -> Result<&Path> {
        let artifact = self
            .manifest
            .artifacts
            .iter()
            .find(|artifact| artifact.logical_command == logical_command)
            .with_context(|| {
                format!(
                    "backend `{}` has no admitted direct executable `{logical_command}`",
                    self.backend
                )
            })?;
        verify_decoded_artifact(artifact)?;
        artifact
            .invocation_path
            .as_deref()
            .context("admitted executable has no invocation path")
    }

    pub fn command(&self, logical_command: &str) -> Result<Command> {
        Ok(Command::new(self.command_path(logical_command)?))
    }

    pub fn contains(&self, logical_command: &str) -> bool {
        self.manifest
            .artifacts
            .iter()
            .any(|artifact| artifact.logical_command == logical_command)
    }

    pub fn verify_all(&self) -> Result<()> {
        for artifact in &self.manifest.artifacts {
            if self.linux_procfd_proxy && artifact.role == "ostadix-proxy" {
                verify_running_linux_proxy(artifact)?;
            } else {
                verify_decoded_artifact(artifact)?;
            }
        }
        Ok(())
    }

    pub fn executable_set_sha256(&self) -> &str {
        &self.manifest.sha256
    }
}

/// Resolve one exact direct-launch alternative using the same catalog and
/// host refinement rules consumed by runtime admission.
pub fn resolve_backend_launch_selection(backend: &str) -> Result<ResolvedBackendLaunchSelectionV1> {
    let registry = BackendRegistry::global();
    let Some(spec) = registry.get(backend) else {
        // Unknown language tags remain locally executable through the
        // conservative compatibility bridge. This is launch discovery only:
        // the authoritative V4 implementation-identity constructor still
        // rejects an unknown backend, so this fallback grants no placement
        // or registry-publication authority.
        let requirement = registry.runtime_requirements_for(backend);
        let (selected_alternative, direct_commands) =
            select_complete_alternative(requirement.alternatives).with_context(|| {
                format!(
                    "backend `{backend}` has no complete direct executable alternative for requirement `{}`",
                    requirement.key
                )
            })?;
        return Ok(ResolvedBackendLaunchSelectionV1 {
            requirement_key: requirement.key.to_owned(),
            selected_alternative,
            selection: ExecutableSelectionV1::CompleteCatalogAlternative,
            direct_commands: direct_commands
                .into_iter()
                .map(|(logical, path)| (logical.to_owned(), path))
                .collect(),
        });
    };
    if spec.execution != ExecutionMode::Shim {
        bail!(
            "backend `{}` is not a shim-backed execution target",
            spec.name
        );
    }
    let requirement = registry.runtime_requirements_for(spec.name);
    let (selected_alternative, selection, direct_commands) = match spec.adapter {
        BackendAdapterKind::LegacyPythonShim
            if spec.name == "nixos_test" && !nixos_test_uses_nix_on_this_host() =>
        {
            (
                0,
                ExecutableSelectionV1::AdapterDirectLauncherRefinement,
                vec![(
                    "python3",
                    which::which("python3")
                        .context("python3 is required for the legacy backend bridge")?,
                )],
            )
        }
        BackendAdapterKind::LegacyPythonShim | BackendAdapterKind::NativeRust => {
            let (index, commands) = select_complete_alternative(requirement.alternatives)
                .with_context(|| {
                    format!(
                        "backend `{}` has no complete direct executable alternative for requirement `{}`",
                        spec.name, requirement.key
                    )
                })?;
            (
                index,
                ExecutableSelectionV1::CompleteCatalogAlternative,
                commands,
            )
        }
        BackendAdapterKind::Inline => {
            bail!("shim backend `{}` has no process adapter", spec.name)
        }
    };
    Ok(ResolvedBackendLaunchSelectionV1 {
        requirement_key: requirement.key.to_owned(),
        selected_alternative,
        selection,
        direct_commands: direct_commands
            .into_iter()
            .map(|(logical, path)| (logical.to_owned(), path))
            .collect(),
    })
}

/// Capture one exact direct-launch alternative per plan-used shim backend.
/// Missing direct entrypoints reject execution before evidence/admission and
/// before any plan operation is dispatched. Inline-only plans have no process
/// launch authority to capture, so they return an empty manifest without
/// resolving the current executable. This keeps pure WASI evaluation usable
/// while shim-backed WASI plans continue to fail closed.
pub fn capture_execution_manifest(
    plan: &ExecutionPlan,
) -> Result<(ExecutableManifestV1, Arc<ExecutableLeaseSet>)> {
    capture_execution_manifest_with_current_executable_resolver(plan, || {
        std::env::current_exe().context("failed to locate current O executable")
    })
}

fn capture_execution_manifest_with_current_executable_resolver<F>(
    plan: &ExecutionPlan,
    resolve_current_executable: F,
) -> Result<(ExecutableManifestV1, Arc<ExecutableLeaseSet>)>
where
    F: FnOnce() -> Result<PathBuf>,
{
    let shim_backends = shim_backends(plan);
    let current_executable = if shim_backends.is_empty() {
        None
    } else {
        Some(resolve_current_executable()?)
    };
    capture_execution_manifest_for_shim_backends(shim_backends, current_executable.as_deref())
}

/// Capture a manifest with an explicit O proxy entrypoint.
///
/// This exists for black-box integration gates and generated-runtime hosts
/// whose executable is already known by an outer launcher. Normal execution
/// must use [`capture_execution_manifest`] so admission binds its own image.
#[doc(hidden)]
pub fn capture_execution_manifest_with_current_executable(
    plan: &ExecutionPlan,
    current_executable: &Path,
) -> Result<(ExecutableManifestV1, Arc<ExecutableLeaseSet>)> {
    capture_execution_manifest_for_shim_backends(shim_backends(plan), Some(current_executable))
}

fn capture_execution_manifest_for_shim_backends(
    shim_backends: BTreeSet<String>,
    current_executable: Option<&Path>,
) -> Result<(ExecutableManifestV1, Arc<ExecutableLeaseSet>)> {
    let backends = shim_backends.clone();
    let mut artifacts = Vec::new();
    let mut retained = BTreeMap::new();

    for backend in backends {
        let launch_selection = resolve_backend_launch_selection(&backend)?;
        let selected_alternative = launch_selection.selected_alternative;
        let selection = launch_selection.selection;
        let requirement_key = launch_selection.requirement_key;
        for (logical, path) in launch_selection.direct_commands {
            let selection_context = ArtifactSelection {
                requirement_key: &requirement_key,
                selected_alternative: Some(selected_alternative),
                selection,
            };
            artifacts.push(capture_artifact(
                &backend,
                selection_context,
                &logical,
                "direct-launcher",
                &path,
                &mut retained,
            )?);
        }

        if shim_backends.contains(&backend) {
            let current_executable =
                current_executable.context("plan-used shim backend has no current O executable")?;
            artifacts.push(capture_artifact(
                &backend,
                ArtifactSelection {
                    requirement_key: &requirement_key,
                    selected_alternative: Some(selected_alternative),
                    selection,
                },
                CURRENT_O_LOGICAL_COMMAND,
                "ostadix-proxy",
                current_executable,
                &mut retained,
            )?);

            #[cfg(target_os = "macos")]
            artifacts.push(capture_artifact(
                &backend,
                ArtifactSelection {
                    requirement_key: &requirement_key,
                    selected_alternative: Some(selected_alternative),
                    selection,
                },
                SANDBOX_EXEC_LOGICAL_COMMAND,
                "sandbox-wrapper",
                Path::new("/usr/bin/sandbox-exec"),
                &mut retained,
            )?);
        }
    }

    let manifest = ExecutableManifestV1::finish(artifacts);
    validate_decoded_manifest(&manifest, None)
        .context("captured direct executable manifest is internally invalid")?;
    let backend_digests = backend_digests(&manifest);
    let leases = Arc::new(ExecutableLeaseSet {
        manifest: manifest.clone(),
        retained,
        backend_artifacts: backend_artifact_indices(&manifest),
        backend_digests,
    });
    Ok((manifest, leases))
}

/// Non-probing projection for `olangc --explain-schedule`.  Candidate rows
/// remain explicitly unselected and carry no path, hash, or file identity.
pub fn inspection_executable_manifest(plan: &ExecutionPlan) -> ExecutableManifestV1 {
    let registry = BackendRegistry::global();
    let shim_backends = shim_backends(plan);
    let mut artifacts = Vec::new();
    let backends = shim_backends.clone();
    for backend in backends {
        let requirement = registry.runtime_requirements_for(&backend);
        let commands = if backend == "nixos_test" && !nixos_test_uses_nix_on_this_host() {
            vec!["python3"]
        } else {
            requirement
                .alternatives
                .iter()
                .flat_map(|alternative| alternative.iter().copied())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect()
        };
        for command in commands {
            artifacts.push(unprobed_artifact(
                &backend,
                requirement.key,
                command,
                "direct-launcher",
            ));
        }
        if shim_backends.contains(&backend) {
            artifacts.push(unprobed_artifact(
                &backend,
                requirement.key,
                CURRENT_O_LOGICAL_COMMAND,
                "ostadix-proxy",
            ));
            #[cfg(target_os = "macos")]
            artifacts.push(unprobed_artifact(
                &backend,
                requirement.key,
                SANDBOX_EXEC_LOGICAL_COMMAND,
                "sandbox-wrapper",
            ));
        }
    }
    ExecutableManifestV1::finish(artifacts)
}

fn shim_backends(plan: &ExecutionPlan) -> BTreeSet<String> {
    plan.nodes
        .iter()
        .filter_map(|node| match &node.kind {
            PlanNodeKind::Exec { backend, .. } if backend.execution == ExecutionMode::Shim => {
                Some(backend.canonical.clone())
            }
            _ => None,
        })
        .collect()
}

fn nixos_test_uses_nix_on_this_host() -> bool {
    cfg!(target_os = "linux")
        || std::env::var_os("NIXPKGS_ALLOW_UNSUPPORTED_SYSTEM").is_some_and(|value| value == "1")
}

fn select_complete_alternative(
    alternatives: &'static [&'static [&'static str]],
) -> Result<(usize, Vec<(&'static str, PathBuf)>)> {
    alternatives
        .iter()
        .enumerate()
        .find_map(|(index, alternative)| {
            alternative
                .iter()
                .map(|command| which::which(command).map(|path| (*command, path)))
                .collect::<std::result::Result<Vec<_>, _>>()
                .ok()
                .map(|resolved| (index, resolved))
        })
        .context("no complete executable alternative is present")
}

fn absolute_invocation_path(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    std::env::current_dir()
        .context("failed to resolve current directory for executable invocation")
        .map(|directory| directory.join(path))
}

fn verify_invocation_target(
    invocation_path: &Path,
    canonical_path: &Path,
    expected_invocation: &ExecutableFileIdentityV1,
    expected: &ExecutableFileIdentityV1,
) -> Result<()> {
    let invocation_identity =
        file_identity(&fs::symlink_metadata(invocation_path).with_context(|| {
            format!(
                "failed to stat admitted executable invocation name {}",
                invocation_path.display()
            )
        })?)?;
    if &invocation_identity != expected_invocation {
        bail!(
            "admitted executable invocation name changed identity: {}",
            invocation_path.display()
        );
    }
    let current_target = invocation_path.canonicalize().with_context(|| {
        format!(
            "failed to resolve admitted executable invocation path {}",
            invocation_path.display()
        )
    })?;
    if current_target != canonical_path {
        bail!(
            "admitted executable invocation path changed target: {}",
            invocation_path.display()
        );
    }
    let actual = file_identity(&fs::metadata(&current_target).with_context(|| {
        format!(
            "failed to stat admitted executable invocation target {}",
            current_target.display()
        )
    })?)?;
    if &actual != expected {
        bail!(
            "admitted executable invocation target changed identity: {}",
            invocation_path.display()
        );
    }
    Ok(())
}

#[cfg(unix)]
fn verify_content_if_required(_artifact: &ExecutableArtifactV1, _path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(not(unix))]
fn verify_content_if_required(artifact: &ExecutableArtifactV1, path: &Path) -> Result<()> {
    let expected = artifact
        .sha256
        .as_deref()
        .context("launch-bound executable lacks a SHA-256 digest")?;
    let file = File::open(path)
        .with_context(|| format!("failed to reopen executable target {}", path.display()))?;
    let actual = sha256_file(&file, path)?;
    if actual != expected {
        bail!(
            "admitted direct executable content changed before launch: backend `{}` command `{}`",
            artifact.canonical_backend,
            artifact.logical_command
        );
    }
    Ok(())
}

fn verify_invocation_path(
    artifact: &ExecutableArtifactV1,
    expected: &ExecutableFileIdentityV1,
) -> Result<()> {
    let invocation_path = artifact
        .invocation_path
        .as_ref()
        .context("execution-bound executable lacks an invocation path")?;
    let canonical_path = artifact
        .canonical_path
        .as_ref()
        .context("execution-bound executable lacks a canonical target path")?;
    let expected_invocation = artifact
        .invocation_file_identity
        .as_ref()
        .context("execution-bound executable lacks an invocation-name identity")?;
    verify_invocation_target(
        invocation_path,
        canonical_path,
        expected_invocation,
        expected,
    )
    .with_context(|| {
        format!(
            "admitted invocation path is stale for backend `{}` command `{}`",
            artifact.canonical_backend, artifact.logical_command
        )
    })?;
    Ok(())
}

fn capture_artifact(
    backend: &str,
    selection: ArtifactSelection<'_>,
    logical_command: &str,
    role: &str,
    path: &Path,
    retained: &mut BTreeMap<PathBuf, RetainedExecutable>,
) -> Result<ExecutableArtifactV1> {
    let invocation_path = absolute_invocation_path(path)?;
    let invocation_file_identity =
        file_identity(&fs::symlink_metadata(&invocation_path).with_context(|| {
            format!(
                "failed to stat executable invocation name {}",
                invocation_path.display()
            )
        })?)?;
    let canonical_path = invocation_path
        .canonicalize()
        .with_context(|| format!("failed to canonicalize executable {}", path.display()))?;

    // A launcher may be shared by several plan-used backends (notably the
    // current O proxy, sandbox-exec, and python3). The first capture owns the
    // only content hash. Later rows reuse that evidence after proving that the
    // canonical name and retained handle still designate the same object.
    if let Some(existing) = retained.get(&canonical_path) {
        if role == "ostadix-proxy" {
            ensure_native_executable_image(&existing.file, &canonical_path)
                .context("O backend proxy is not a native executable image")?;
        }
        let observed_path_identity =
            file_identity(&fs::metadata(&canonical_path).with_context(|| {
                format!(
                    "failed to stat executable path {}",
                    canonical_path.display()
                )
            })?)?;
        let handle_identity = file_identity(&existing.file.metadata().with_context(|| {
            format!(
                "failed to stat retained executable handle {}",
                canonical_path.display()
            )
        })?)?;
        if observed_path_identity != existing.identity || handle_identity != existing.identity {
            bail!(
                "direct executable changed while reusing captured evidence: {}",
                canonical_path.display()
            );
        }
        verify_invocation_target(
            &invocation_path,
            &canonical_path,
            &invocation_file_identity,
            &existing.identity,
        )?;
        return Ok(ExecutableArtifactV1 {
            canonical_backend: backend.to_string(),
            requirement_key: selection.requirement_key.to_string(),
            selected_alternative: selection.selected_alternative,
            selection: selection.selection,
            logical_command: logical_command.to_string(),
            role: role.to_string(),
            state: ExecutableArtifactStateV1::LocatedHashed,
            invocation_identity: path_identity(&invocation_path),
            invocation_path: Some(invocation_path),
            invocation_file_identity: Some(invocation_file_identity),
            resolved_identity: path_identity(&canonical_path),
            canonical_path: Some(canonical_path),
            sha256: Some(existing.sha256.clone()),
            file_identity: Some(existing.identity.clone()),
            guarantee: platform_execution_guarantee(),
        });
    }

    let file = File::open(&canonical_path)
        .with_context(|| format!("failed to open executable {}", canonical_path.display()))?;
    let metadata = file
        .metadata()
        .with_context(|| format!("failed to stat executable {}", canonical_path.display()))?;
    if !metadata.is_file() {
        bail!(
            "direct executable is not a regular file: {}",
            canonical_path.display()
        );
    }
    ensure_executable_mode(&metadata, &canonical_path)?;
    if role == "ostadix-proxy" {
        ensure_native_executable_image(&file, &canonical_path)
            .context("O backend proxy is not a native executable image")?;
    }
    let identity = file_identity(&metadata)?;
    verify_invocation_target(
        &invocation_path,
        &canonical_path,
        &invocation_file_identity,
        &identity,
    )?;
    let path_identity_before =
        file_identity(&fs::metadata(&canonical_path).with_context(|| {
            format!(
                "failed to stat executable path {}",
                canonical_path.display()
            )
        })?)?;
    if path_identity_before != identity {
        bail!(
            "direct executable was replaced while being opened: {}",
            canonical_path.display()
        );
    }
    let sha256 = sha256_file(&file, &canonical_path)?;
    #[cfg(target_os = "linux")]
    let is_elf = file_is_elf(&file, &canonical_path)?;
    let handle_identity_after = file_identity(&file.metadata().with_context(|| {
        format!(
            "failed to re-stat executable handle {}",
            canonical_path.display()
        )
    })?)?;
    let path_identity_after =
        file_identity(&fs::metadata(&canonical_path).with_context(|| {
            format!(
                "failed to re-stat executable path {}",
                canonical_path.display()
            )
        })?)?;
    if handle_identity_after != identity || path_identity_after != identity {
        bail!(
            "direct executable changed while being hashed: {}",
            canonical_path.display()
        );
    }
    verify_invocation_target(
        &invocation_path,
        &canonical_path,
        &invocation_file_identity,
        &identity,
    )?;
    retained.insert(
        canonical_path.clone(),
        RetainedExecutable {
            path: canonical_path.clone(),
            file,
            identity: identity.clone(),
            sha256: sha256.clone(),
            #[cfg(target_os = "linux")]
            is_elf,
        },
    );
    Ok(ExecutableArtifactV1 {
        canonical_backend: backend.to_string(),
        requirement_key: selection.requirement_key.to_string(),
        selected_alternative: selection.selected_alternative,
        selection: selection.selection,
        logical_command: logical_command.to_string(),
        role: role.to_string(),
        state: ExecutableArtifactStateV1::LocatedHashed,
        invocation_identity: path_identity(&invocation_path),
        invocation_path: Some(invocation_path),
        invocation_file_identity: Some(invocation_file_identity),
        resolved_identity: path_identity(&canonical_path),
        canonical_path: Some(canonical_path),
        sha256: Some(sha256),
        file_identity: Some(identity),
        guarantee: platform_execution_guarantee(),
    })
}

fn unprobed_artifact(
    backend: &str,
    requirement_key: &str,
    command: &str,
    role: &str,
) -> ExecutableArtifactV1 {
    ExecutableArtifactV1 {
        canonical_backend: backend.to_string(),
        requirement_key: requirement_key.to_string(),
        selected_alternative: None,
        selection: ExecutableSelectionV1::NotSelected,
        logical_command: command.to_string(),
        role: role.to_string(),
        state: ExecutableArtifactStateV1::NotProbed,
        invocation_path: None,
        canonical_path: None,
        invocation_identity: format!("logical-command:{command}"),
        invocation_file_identity: None,
        resolved_identity: format!("logical-command:{command}"),
        sha256: None,
        file_identity: None,
        guarantee: ExecutableGuaranteeV1::InspectionNotProbed,
    }
}

fn backend_digests(manifest: &ExecutableManifestV1) -> BTreeMap<String, String> {
    manifest
        .artifacts
        .iter()
        .map(|artifact| artifact.canonical_backend.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(|backend| {
            let artifacts = manifest
                .artifacts
                .iter()
                .filter(|artifact| artifact.canonical_backend == backend)
                .cloned()
                .collect();
            let digest = ExecutableManifestV1::finish(artifacts).sha256;
            (backend, digest)
        })
        .collect()
}

fn backend_artifact_indices(manifest: &ExecutableManifestV1) -> HashMap<String, Vec<usize>> {
    let mut indices = HashMap::<String, Vec<usize>>::new();
    for (index, artifact) in manifest.artifacts.iter().enumerate() {
        indices
            .entry(artifact.canonical_backend.clone())
            .or_default()
            .push(index);
    }
    indices
}

fn validate_decoded_manifest(manifest: &ExecutableManifestV1, backend: Option<&str>) -> Result<()> {
    validate_manifest_envelope(manifest, backend)?;
    for artifact in &manifest.artifacts {
        validate_execution_artifact_shape(artifact)?;
    }
    validate_backend_selections(manifest)?;
    Ok(())
}

fn validate_backend_process_manifest(
    manifest: &ExecutableManifestV1,
    backend: &str,
    linux_procfd_proxy: bool,
) -> Result<()> {
    if !linux_procfd_proxy {
        return validate_decoded_manifest(manifest, Some(backend));
    }
    validate_manifest_envelope(manifest, Some(backend))?;
    for artifact in &manifest.artifacts {
        validate_execution_artifact_shape(artifact)?;
        if artifact.role == "ostadix-proxy" {
            verify_running_linux_proxy(artifact)?;
        }
    }
    validate_backend_selections(manifest)
}

#[cfg(target_os = "linux")]
fn verify_running_linux_proxy(artifact: &ExecutableArtifactV1) -> Result<()> {
    if artifact.role != "ostadix-proxy" {
        return verify_decoded_artifact(artifact);
    }
    let expected = artifact
        .file_identity
        .as_ref()
        .context("launch-bound O proxy lacks a file identity")?;
    let actual = file_identity(
        &fs::metadata("/proc/self/exe")
            .context("failed to stat the running Linux O backend proxy")?,
    )?;
    if !same_open_object_identity(&actual, expected) {
        bail!("running Linux O backend proxy does not match its admitted open-object identity");
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn verify_running_linux_proxy(artifact: &ExecutableArtifactV1) -> Result<()> {
    verify_decoded_artifact(artifact)
}

fn validate_manifest_envelope(
    manifest: &ExecutableManifestV1,
    backend: Option<&str>,
) -> Result<()> {
    if manifest.schema != EXECUTABLE_MANIFEST_SCHEMA_V1 || manifest.scope != "direct-launcher-only"
    {
        bail!("unsupported admitted direct executable manifest schema or scope");
    }
    if manifest.sha256 != manifest_sha256(&manifest.artifacts) {
        bail!("admitted direct executable manifest digest mismatch");
    }
    if let Some(backend) = backend {
        if manifest.artifacts.is_empty()
            || manifest
                .artifacts
                .iter()
                .any(|artifact| artifact.canonical_backend != backend)
        {
            bail!("admitted direct executable manifest is not scoped to backend `{backend}`");
        }
    }
    let mut logical_commands = BTreeSet::new();
    let registry = BackendRegistry::global();
    for artifact in &manifest.artifacts {
        if !logical_commands.insert((
            artifact.canonical_backend.as_str(),
            artifact.logical_command.as_str(),
        )) {
            bail!(
                "admitted direct executable manifest repeats backend `{}` command `{}`",
                artifact.canonical_backend,
                artifact.logical_command
            );
        }
        if registry.canonical(&artifact.canonical_backend) != artifact.canonical_backend {
            bail!(
                "direct executable manifest backend `{}` is not canonical",
                artifact.canonical_backend
            );
        }
        let requirement = registry.runtime_requirements_for(&artifact.canonical_backend);
        if artifact.requirement_key != requirement.key {
            bail!(
                "direct executable manifest backend `{}` names requirement `{}` instead of `{}`",
                artifact.canonical_backend,
                artifact.requirement_key,
                requirement.key
            );
        }
        validate_artifact_role(artifact)?;
    }
    Ok(())
}

fn validate_backend_selections(manifest: &ExecutableManifestV1) -> Result<()> {
    let registry = BackendRegistry::global();
    for backend in manifest
        .artifacts
        .iter()
        .map(|artifact| artifact.canonical_backend.as_str())
        .collect::<BTreeSet<_>>()
    {
        let rows = manifest
            .artifacts
            .iter()
            .filter(|artifact| artifact.canonical_backend == backend)
            .collect::<Vec<_>>();
        let selected = rows[0]
            .selected_alternative
            .context("execution manifest backend has no selected alternative")?;
        let selection = rows[0].selection;
        if rows.iter().any(|artifact| {
            artifact.selected_alternative != Some(selected) || artifact.selection != selection
        }) {
            bail!("direct executable manifest backend `{backend}` has inconsistent selection");
        }
        let direct_commands = rows
            .iter()
            .filter(|artifact| artifact.role == "direct-launcher")
            .map(|artifact| artifact.logical_command.as_str())
            .collect::<BTreeSet<_>>();
        let requirement = registry.runtime_requirements_for(backend);
        match selection {
            ExecutableSelectionV1::CompleteCatalogAlternative => {
                let expected = requirement
                    .alternatives
                    .get(selected)
                    .with_context(|| {
                        format!(
                            "direct executable manifest backend `{backend}` selects nonexistent alternative {selected}"
                        )
                    })?
                    .iter()
                    .copied()
                    .collect::<BTreeSet<_>>();
                if direct_commands != expected {
                    bail!(
                        "direct executable manifest backend `{backend}` does not bind its complete selected catalog alternative"
                    );
                }
            }
            ExecutableSelectionV1::AdapterDirectLauncherRefinement => {
                if backend != "nixos_test"
                    || nixos_test_uses_nix_on_this_host()
                    || registry.adapter_for(backend) != BackendAdapterKind::LegacyPythonShim
                    || direct_commands != BTreeSet::from(["python3"])
                {
                    bail!(
                        "direct executable manifest backend `{backend}` has an invalid legacy-adapter launcher refinement"
                    );
                }
            }
            ExecutableSelectionV1::NotSelected => {
                bail!("execution manifest backend `{backend}` has no selected launch alternative")
            }
        }
    }
    Ok(())
}

fn validate_inspection_artifact_shape(artifact: &ExecutableArtifactV1) -> Result<()> {
    validate_artifact_identity_fields(artifact)?;
    validate_artifact_role(artifact)?;
    if artifact.state != ExecutableArtifactStateV1::NotProbed
        || artifact.guarantee != ExecutableGuaranteeV1::InspectionNotProbed
        || artifact.selected_alternative.is_some()
        || artifact.selection != ExecutableSelectionV1::NotSelected
        || artifact.invocation_path.is_some()
        || artifact.invocation_file_identity.is_some()
        || artifact.canonical_path.is_some()
        || artifact.sha256.is_some()
        || artifact.file_identity.is_some()
        || artifact.invocation_identity != format!("logical-command:{}", artifact.logical_command)
        || artifact.resolved_identity != format!("logical-command:{}", artifact.logical_command)
    {
        bail!(
            "inspection executable `{}` contains probed or selected execution state",
            artifact.logical_command
        );
    }
    Ok(())
}

fn validate_execution_artifact_shape(artifact: &ExecutableArtifactV1) -> Result<()> {
    validate_artifact_identity_fields(artifact)?;
    validate_artifact_role(artifact)?;
    if artifact.state != ExecutableArtifactStateV1::LocatedHashed
        || artifact.guarantee != platform_execution_guarantee()
        || artifact.selected_alternative.is_none()
        || artifact.selection == ExecutableSelectionV1::NotSelected
    {
        bail!(
            "direct executable `{}` is not launch-bound",
            artifact.logical_command
        );
    }
    let invocation_path = artifact
        .invocation_path
        .as_ref()
        .context("launch-bound executable lacks an invocation path")?;
    if !invocation_path.is_absolute() {
        bail!(
            "launch-bound executable invocation path is not absolute: {}",
            invocation_path.display()
        );
    }
    if artifact.invocation_identity != path_identity(invocation_path) {
        bail!(
            "launch-bound executable invocation identity disagrees with its path: {}",
            invocation_path.display()
        );
    }
    artifact
        .invocation_file_identity
        .as_ref()
        .context("launch-bound executable lacks an invocation-name identity")?;
    let path = artifact
        .canonical_path
        .as_ref()
        .context("launch-bound executable lacks a canonical path")?;
    if !path.is_absolute() {
        bail!(
            "launch-bound executable path is not absolute: {}",
            path.display()
        );
    }
    if artifact.resolved_identity != path_identity(path) {
        bail!(
            "launch-bound executable path identity disagrees with its canonical path: {}",
            path.display()
        );
    }
    let sha256 = artifact
        .sha256
        .as_deref()
        .context("launch-bound executable lacks a SHA-256 digest")?;
    if sha256.len() != 64
        || !sha256
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        bail!(
            "launch-bound executable `{}` has a malformed SHA-256 digest",
            artifact.logical_command
        );
    }
    artifact
        .file_identity
        .as_ref()
        .context("launch-bound executable lacks a file identity")?;
    Ok(())
}

#[cfg(unix)]
const fn platform_execution_guarantee() -> ExecutableGuaranteeV1 {
    ExecutableGuaranteeV1::DirectLauncherPathAndOpenFileIdentity
}

#[cfg(not(unix))]
const fn platform_execution_guarantee() -> ExecutableGuaranteeV1 {
    ExecutableGuaranteeV1::DirectLauncherContentHashImmediatelyBeforeLaunch
}

fn validate_artifact_identity_fields(artifact: &ExecutableArtifactV1) -> Result<()> {
    if artifact.canonical_backend.is_empty()
        || artifact.requirement_key.is_empty()
        || artifact.logical_command.is_empty()
    {
        bail!("direct executable manifest contains an empty identity field");
    }
    Ok(())
}

fn validate_artifact_role(artifact: &ExecutableArtifactV1) -> Result<()> {
    let valid = match artifact.role.as_str() {
        "direct-launcher" => !matches!(
            artifact.logical_command.as_str(),
            CURRENT_O_LOGICAL_COMMAND | SANDBOX_EXEC_LOGICAL_COMMAND
        ),
        "ostadix-proxy" => artifact.logical_command == CURRENT_O_LOGICAL_COMMAND,
        "sandbox-wrapper" => artifact.logical_command == SANDBOX_EXEC_LOGICAL_COMMAND,
        _ => false,
    };
    if !valid {
        bail!(
            "direct executable `{}` has invalid role `{}`",
            artifact.logical_command,
            artifact.role
        );
    }
    Ok(())
}

fn verify_decoded_artifact(artifact: &ExecutableArtifactV1) -> Result<()> {
    validate_execution_artifact_shape(artifact)?;
    let path = artifact
        .canonical_path
        .as_ref()
        .context("launch-bound executable lacks a canonical path")?;
    let expected = artifact
        .file_identity
        .as_ref()
        .context("launch-bound executable lacks a file identity")?;
    verify_invocation_path(artifact, expected)?;
    let actual =
        file_identity(&fs::metadata(path).with_context(|| {
            format!("failed to stat admitted executable path {}", path.display())
        })?)?;
    if &actual != expected {
        bail!(
            "admitted direct executable path changed before child launch: backend `{}` command `{}`",
            artifact.canonical_backend,
            artifact.logical_command
        );
    }
    verify_content_if_required(artifact, path)?;
    Ok(())
}

fn sha256_file(file: &File, path: &Path) -> Result<String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut reader = file
        .try_clone()
        .with_context(|| format!("failed to clone executable handle {}", path.display()))?;
    reader.seek(SeekFrom::Start(0))?;
    let mut hash = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    Ok(hex::encode(hash.finalize()))
}

#[cfg(target_os = "linux")]
fn file_is_elf(file: &File, path: &Path) -> Result<bool> {
    use std::io::{Read, Seek, SeekFrom};
    let mut reader = file
        .try_clone()
        .with_context(|| format!("failed to clone executable handle {}", path.display()))?;
    reader.seek(SeekFrom::Start(0))?;
    let mut magic = [0_u8; 4];
    match reader.read_exact(&mut magic) {
        Ok(()) => Ok(magic == *b"\x7fELF"),
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn manifest_sha256(artifacts: &[ExecutableArtifactV1]) -> String {
    let bytes = serde_json::to_vec(artifacts)
        .expect("serializing executable manifest artifacts into memory cannot fail");
    let mut hash = Sha256::new();
    hash.update(EXECUTABLE_MANIFEST_SCHEMA_V1.as_bytes());
    hash.update(b"direct-launcher-only");
    hash.update((bytes.len() as u64).to_be_bytes());
    hash.update(bytes);
    hex::encode(hash.finalize())
}

fn path_identity(path: &Path) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        format!("path-bytes:{}", hex::encode(path.as_os_str().as_bytes()))
    }
    #[cfg(not(unix))]
    {
        format!("path-lossy:{}", path.to_string_lossy())
    }
}

#[cfg(unix)]
fn ensure_executable_mode(metadata: &Metadata, path: &Path) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    if metadata.mode() & 0o111 == 0 {
        bail!(
            "direct executable has no execute permission bits: {}",
            path.display()
        );
    }
    Ok(())
}

#[cfg(not(unix))]
fn ensure_executable_mode(_metadata: &Metadata, _path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn file_identity(metadata: &Metadata) -> Result<ExecutableFileIdentityV1> {
    use std::os::unix::fs::MetadataExt;
    Ok(ExecutableFileIdentityV1 {
        device: metadata.dev(),
        inode: metadata.ino(),
        size: metadata.size(),
        mode: metadata.mode(),
        mtime_seconds: metadata.mtime(),
        mtime_nanoseconds: metadata.mtime_nsec(),
        ctime_seconds: metadata.ctime(),
        ctime_nanoseconds: metadata.ctime_nsec(),
    })
}

#[cfg(unix)]
fn same_open_object_identity(
    actual: &ExecutableFileIdentityV1,
    expected: &ExecutableFileIdentityV1,
) -> bool {
    // Removing or replacing the last directory entry of a still-open inode
    // changes ctime even though the retained object is unchanged. Device and
    // inode establish object identity while size, mode, and mtime retain the
    // inexpensive mutation checks that do not depend on link-count churn.
    actual.device == expected.device
        && actual.inode == expected.inode
        && actual.size == expected.size
        && actual.mode == expected.mode
        && actual.mtime_seconds == expected.mtime_seconds
        && actual.mtime_nanoseconds == expected.mtime_nanoseconds
}

#[cfg(not(unix))]
fn same_open_object_identity(
    actual: &ExecutableFileIdentityV1,
    expected: &ExecutableFileIdentityV1,
) -> bool {
    actual == expected
}

#[cfg(not(unix))]
fn file_identity(metadata: &Metadata) -> Result<ExecutableFileIdentityV1> {
    use std::time::UNIX_EPOCH;
    let modified = metadata
        .modified()
        .context("executable metadata has no modification time")?
        .duration_since(UNIX_EPOCH)
        .context("executable modification time predates the Unix epoch")?;
    Ok(ExecutableFileIdentityV1 {
        device: 0,
        inode: 0,
        size: metadata.len(),
        mode: 0,
        mtime_seconds: i64::try_from(modified.as_secs())
            .context("executable modification time exceeds i64")?,
        mtime_nanoseconds: i64::from(modified.subsec_nanos()),
        ctime_seconds: 0,
        ctime_nanoseconds: 0,
    })
}

/// Capacity-safe authority for a command discovered only when an operation is
/// actually performed. Unlike the plan manifest this does not make an
/// unforced lazy Request a host-readiness requirement. The open target, hash,
/// invocation name, and file identity are retained and rechecked before each
/// subprocess in that performed operation or autonomous batch.
#[derive(Clone, Debug)]
pub(crate) struct RuntimeCommandLease {
    inner: Arc<RuntimeCommandLeaseInner>,
}

#[derive(Debug)]
struct RuntimeCommandLeaseInner {
    logical_command: String,
    invocation_path: PathBuf,
    canonical_path: PathBuf,
    invocation_identity: ExecutableFileIdentityV1,
    identity: ExecutableFileIdentityV1,
    file: File,
    sha256: String,
}

impl RuntimeCommandLease {
    pub(crate) fn capture(logical_command: &str) -> Result<Self> {
        let discovered = which::which(logical_command).with_context(|| {
            format!("required runtime command `{logical_command}` is not available on PATH")
        })?;
        let invocation_path = absolute_invocation_path(&discovered)?;
        let invocation_identity =
            file_identity(&fs::symlink_metadata(&invocation_path).with_context(|| {
                format!(
                    "failed to stat runtime command invocation name {}",
                    invocation_path.display()
                )
            })?)?;
        let canonical_path = invocation_path.canonicalize().with_context(|| {
            format!(
                "failed to canonicalize runtime command {}",
                invocation_path.display()
            )
        })?;
        let file = File::open(&canonical_path).with_context(|| {
            format!(
                "failed to open runtime command {}",
                canonical_path.display()
            )
        })?;
        let metadata = file.metadata().with_context(|| {
            format!(
                "failed to stat runtime command {}",
                canonical_path.display()
            )
        })?;
        if !metadata.is_file() {
            bail!(
                "runtime command is not a regular file: {}",
                canonical_path.display()
            );
        }
        ensure_executable_mode(&metadata, &canonical_path)?;
        let identity = file_identity(&metadata)?;
        verify_invocation_target(
            &invocation_path,
            &canonical_path,
            &invocation_identity,
            &identity,
        )?;
        let sha256 = sha256_file(&file, &canonical_path)?;
        let lease = Self {
            inner: Arc::new(RuntimeCommandLeaseInner {
                logical_command: logical_command.to_string(),
                invocation_path,
                canonical_path,
                invocation_identity,
                identity,
                file,
                sha256,
            }),
        };
        lease.verify()?;
        Ok(lease)
    }

    pub(crate) fn command(&self) -> Result<Command> {
        self.verify()?;
        Ok(Command::new(&self.inner.invocation_path))
    }

    fn verify(&self) -> Result<()> {
        verify_invocation_target(
            &self.inner.invocation_path,
            &self.inner.canonical_path,
            &self.inner.invocation_identity,
            &self.inner.identity,
        )
        .with_context(|| {
            format!(
                "runtime command `{}` invocation path changed after perform-time capture",
                self.inner.logical_command
            )
        })?;
        let handle_identity = file_identity(&self.inner.file.metadata().with_context(|| {
            format!(
                "failed to stat retained runtime command `{}`",
                self.inner.logical_command
            )
        })?)?;
        if handle_identity != self.inner.identity {
            bail!(
                "runtime command `{}` changed after perform-time capture",
                self.inner.logical_command
            );
        }
        #[cfg(not(unix))]
        {
            let current = File::open(&self.inner.canonical_path).with_context(|| {
                format!(
                    "failed to reopen runtime command `{}`",
                    self.inner.logical_command
                )
            })?;
            if sha256_file(&current, &self.inner.canonical_path)? != self.inner.sha256 {
                bail!(
                    "runtime command `{}` content changed after perform-time capture",
                    self.inner.logical_command
                );
            }
        }
        #[cfg(unix)]
        let _ = &self.inner.sha256;
        Ok(())
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::backend_catalog::BackendRegistry;
    use crate::ir::{OIr, OIrProgram};
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;

    fn shell_plan() -> ExecutionPlan {
        OIrProgram {
            nodes: vec![OIr::Exec {
                lang: "shell".into(),
                env_id: u32::MAX,
                attr: None,
                backend: BackendRegistry::global().interface_for("shell"),
                body: vec![OIr::Text("printf ok".into())],
            }],
        }
        .plan()
    }

    fn inline_plan() -> ExecutionPlan {
        OIrProgram {
            nodes: vec![OIr::Text("inline-only".into())],
        }
        .plan()
    }

    #[test]
    fn inline_only_manifest_does_not_resolve_current_executable() {
        let (manifest, leases) =
            capture_execution_manifest_with_current_executable_resolver(&inline_plan(), || {
                panic!("inline-only plan attempted to resolve the current executable")
            })
            .unwrap();

        assert!(manifest.artifacts.is_empty());
        assert_eq!(leases.manifest(), &manifest);
        assert!(leases.retained.is_empty());
        assert!(leases.backend_artifacts.is_empty());
        assert!(leases.backend_digests.is_empty());
    }

    #[test]
    fn shim_manifest_requires_current_executable_resolution() {
        let error =
            capture_execution_manifest_with_current_executable_resolver(&shell_plan(), || {
                anyhow::bail!("distinctive current-executable resolution failure")
            })
            .unwrap_err();

        assert!(
            format!("{error:#}").contains("distinctive current-executable resolution failure"),
            "{error:#}"
        );
    }

    #[test]
    fn inspection_is_explicitly_non_probing() {
        let manifest = inspection_executable_manifest(&shell_plan());
        assert!(!manifest.artifacts.is_empty());
        assert!(manifest.artifacts.iter().all(|artifact| {
            artifact.state == ExecutableArtifactStateV1::NotProbed
                && artifact.canonical_path.is_none()
                && artifact.sha256.is_none()
                && artifact.file_identity.is_none()
        }));
    }

    #[test]
    fn executable_set_v2_rejects_unprobed_manifest_rows() {
        let manifest = inspection_executable_manifest(&shell_plan());
        let error = backend_executable_set_v2_from_manifest(&manifest, "shell").unwrap_err();
        assert!(
            format!("{error:#}").contains("not launch-bound"),
            "{error:#}"
        );
    }

    #[test]
    fn executable_set_v2_ignores_paths_but_detects_byte_changes() {
        fn manifest_for(path: &Path) -> ExecutableManifestV1 {
            let selection = ArtifactSelection {
                requirement_key: "shell",
                selected_alternative: Some(0),
                selection: ExecutableSelectionV1::CompleteCatalogAlternative,
            };
            let mut retained = BTreeMap::new();
            let artifact = capture_artifact(
                "shell",
                selection,
                "sh",
                "direct-launcher",
                path,
                &mut retained,
            )
            .unwrap();
            ExecutableManifestV1::finish(vec![artifact])
        }

        let first_dir = tempfile::tempdir().unwrap();
        let second_dir = tempfile::tempdir().unwrap();
        let first_path = first_dir.path().join("sh");
        let second_path = second_dir.path().join("sh");
        fs::write(&first_path, b"#!/bin/sh\nprintf same").unwrap();
        fs::write(&second_path, b"#!/bin/sh\nprintf same").unwrap();
        fs::set_permissions(&first_path, fs::Permissions::from_mode(0o755)).unwrap();
        fs::set_permissions(&second_path, fs::Permissions::from_mode(0o755)).unwrap();

        let first =
            backend_executable_set_v2_from_manifest(&manifest_for(&first_path), "shell").unwrap();
        let relocated =
            backend_executable_set_v2_from_manifest(&manifest_for(&second_path), "shell").unwrap();
        assert_eq!(first, relocated);

        fs::write(&second_path, b"#!/bin/sh\nprintf changed").unwrap();
        fs::set_permissions(&second_path, fs::Permissions::from_mode(0o755)).unwrap();
        let changed =
            backend_executable_set_v2_from_manifest(&manifest_for(&second_path), "shell").unwrap();
        assert_ne!(first, changed);
    }

    #[test]
    fn execution_manifest_rejects_script_as_o_proxy() {
        let temp = tempfile::tempdir().unwrap();
        let wrapper = temp.path().join("O-wrapper");
        fs::write(&wrapper, b"#!/bin/sh\nexec /some/other/O \"$@\"\n").unwrap();
        fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).unwrap();

        let error = format!(
            "{:#}",
            capture_execution_manifest_with_current_executable(&shell_plan(), &wrapper)
                .unwrap_err()
        );
        assert!(
            error.contains("O backend proxy is not a native executable image"),
            "{error}"
        );
        assert!(error.contains("script or unsupported executable format"));
    }

    #[test]
    fn o_proxy_reuse_rejects_a_previously_retained_script() {
        let temp = tempfile::tempdir().unwrap();
        let wrapper = temp.path().join("shared-launcher");
        fs::write(&wrapper, b"#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).unwrap();
        let selection = ArtifactSelection {
            requirement_key: "shell",
            selected_alternative: Some(0),
            selection: ExecutableSelectionV1::CompleteCatalogAlternative,
        };
        let mut retained = BTreeMap::new();
        capture_artifact(
            "shell",
            selection,
            "sh",
            "direct-launcher",
            &wrapper,
            &mut retained,
        )
        .unwrap();

        let error = format!(
            "{:#}",
            capture_artifact(
                "shell",
                selection,
                CURRENT_O_LOGICAL_COMMAND,
                "ostadix-proxy",
                &wrapper,
                &mut retained,
            )
            .unwrap_err()
        );
        assert!(
            error.contains("O backend proxy is not a native executable image"),
            "{error}"
        );
        assert!(error.contains("script or unsupported executable format"));
    }

    #[test]
    fn legacy_adapter_projection_includes_adapter_owned_tools() {
        let plan = OIrProgram {
            nodes: vec![OIr::Exec {
                lang: "ubuntu_vm".into(),
                env_id: u32::MAX,
                attr: None,
                backend: BackendRegistry::global().interface_for("ubuntu_vm"),
                body: vec![OIr::Text("adapter closure".into())],
            }],
        }
        .plan();
        let manifest = inspection_executable_manifest(&plan);
        let launchers = manifest
            .artifacts
            .iter()
            .filter(|artifact| artifact.role == "direct-launcher")
            .map(|artifact| artifact.logical_command.as_str())
            .collect::<BTreeSet<_>>();
        assert_eq!(launchers, BTreeSet::from(["multipass", "python3"]));
    }

    #[test]
    fn unknown_extension_launch_is_local_only_legacy_python_fallback() {
        let backend = "research_backend";
        let launch = resolve_backend_launch_selection(backend).unwrap();
        assert_eq!(launch.requirement_key(), "unknown-legacy-python-shim");
        assert_eq!(
            launch.selection(),
            ExecutableSelectionV1::CompleteCatalogAlternative
        );
        assert_eq!(
            launch
                .direct_commands()
                .iter()
                .map(|(logical, _)| logical.as_str())
                .collect::<Vec<_>>(),
            vec!["python3"]
        );

        let registry = BackendRegistry::global();
        assert_eq!(
            registry.adapter_for(backend),
            BackendAdapterKind::LegacyPythonShim
        );
        assert!(registry.specification_sha256(backend).is_none());
        let error = registry
            .backend_implementation_id_v1(
                backend,
                None,
                ArtifactId::from_sha256("11".repeat(32)).unwrap(),
                SemanticDigestV1::from_sha256("22".repeat(32)).unwrap(),
                crate::backend_catalog::LOCAL_BACKEND_PROTOCOL_ABI_V1,
            )
            .unwrap_err();
        assert!(matches!(
            error,
            crate::placement::PlacementValidationError::InvalidToken {
                field: "backend implementation canonical backend",
                ..
            }
        ));
    }

    #[test]
    fn retained_lease_rejects_atomic_path_replacement() {
        let temp = tempfile::tempdir().unwrap();
        let admitted = temp.path().join("tool");
        let replacement = temp.path().join("replacement");
        for (path, bytes) in [
            (&admitted, b"#!/bin/sh\nexit 0\n".as_slice()),
            (&replacement, b"#!/bin/sh\nexit 7\n".as_slice()),
        ] {
            fs::write(path, bytes).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let mut retained = BTreeMap::new();
        let artifact = capture_artifact(
            "shell",
            ArtifactSelection {
                requirement_key: "shell",
                selected_alternative: Some(0),
                selection: ExecutableSelectionV1::CompleteCatalogAlternative,
            },
            "sh",
            "direct-launcher",
            &admitted,
            &mut retained,
        )
        .unwrap();
        let manifest = ExecutableManifestV1::finish(vec![artifact]);
        let leases = ExecutableLeaseSet {
            backend_digests: backend_digests(&manifest),
            backend_artifacts: backend_artifact_indices(&manifest),
            manifest,
            retained,
        };
        fs::rename(&replacement, &admitted).unwrap();
        let error = format!("{:#}", leases.verify_backend("shell").unwrap_err());
        assert!(
            error.contains("admitted invocation path is stale for backend `shell` command `sh`"),
            "{error}"
        );
    }

    #[test]
    fn retained_lease_rejects_in_place_mutation() {
        let temp = tempfile::tempdir().unwrap();
        let admitted = temp.path().join("tool");
        fs::write(&admitted, b"#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(&admitted, fs::Permissions::from_mode(0o755)).unwrap();
        let mut retained = BTreeMap::new();
        let artifact = capture_artifact(
            "shell",
            ArtifactSelection {
                requirement_key: "shell",
                selected_alternative: Some(0),
                selection: ExecutableSelectionV1::CompleteCatalogAlternative,
            },
            "sh",
            "direct-launcher",
            &admitted,
            &mut retained,
        )
        .unwrap();
        let manifest = ExecutableManifestV1::finish(vec![artifact]);
        let leases = ExecutableLeaseSet {
            backend_digests: backend_digests(&manifest),
            backend_artifacts: backend_artifact_indices(&manifest),
            manifest,
            retained,
        };
        let mut writer = fs::OpenOptions::new().write(true).open(&admitted).unwrap();
        writer.write_all(b"#!/bin/sh\nexit 9\n").unwrap();
        writer.flush().unwrap();
        let error = format!("{:#}", leases.verify_backend("shell").unwrap_err());
        assert!(
            error.contains("admitted invocation path is stale for backend `shell` command `sh`"),
            "{error}"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn missing_procfs_falls_back_to_the_revalidated_invocation_path() {
        let current = std::env::current_exe().unwrap();
        let mut retained = BTreeMap::new();
        let artifact = capture_artifact(
            "shell",
            ArtifactSelection {
                requirement_key: "shell",
                selected_alternative: Some(0),
                selection: ExecutableSelectionV1::CompleteCatalogAlternative,
            },
            CURRENT_O_LOGICAL_COMMAND,
            "ostadix-proxy",
            &current,
            &mut retained,
        )
        .unwrap();
        let expected = artifact.invocation_path.clone().unwrap();
        let manifest = ExecutableManifestV1::finish(vec![artifact]);
        let leases = ExecutableLeaseSet {
            backend_digests: backend_digests(&manifest),
            backend_artifacts: backend_artifact_indices(&manifest),
            manifest,
            retained,
        };
        let absent_procfs = tempfile::tempdir().unwrap();
        let command = leases
            .current_o_command_with_proc_root(absent_procfs.path())
            .unwrap();
        assert_eq!(command.get_program(), expected.as_os_str());
        assert!(command
            .get_envs()
            .any(|(key, value)| { key == ADMITTED_PROXY_EXECUTION_ENV && value.is_none() }));
    }

    #[test]
    fn shared_launcher_reuses_one_open_and_hash_capture() {
        let temp = tempfile::tempdir().unwrap();
        let admitted = temp.path().join("shared-tool");
        fs::write(&admitted, b"#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(&admitted, fs::Permissions::from_mode(0o755)).unwrap();
        let mut retained = BTreeMap::new();
        let first = capture_artifact(
            "left",
            ArtifactSelection {
                requirement_key: "test",
                selected_alternative: Some(0),
                selection: ExecutableSelectionV1::CompleteCatalogAlternative,
            },
            "tool",
            "direct-launcher",
            &admitted,
            &mut retained,
        )
        .unwrap();
        let second = capture_artifact(
            "right",
            ArtifactSelection {
                requirement_key: "test",
                selected_alternative: Some(0),
                selection: ExecutableSelectionV1::CompleteCatalogAlternative,
            },
            "tool",
            "direct-launcher",
            &admitted,
            &mut retained,
        )
        .unwrap();

        assert_eq!(retained.len(), 1);
        assert_eq!(first.sha256, second.sha256);
        assert_eq!(first.file_identity, second.file_identity);
    }

    #[test]
    fn child_manifest_rejects_duplicate_logical_commands() {
        let temp = tempfile::tempdir().unwrap();
        let admitted = temp.path().join("tool");
        fs::write(&admitted, b"#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(&admitted, fs::Permissions::from_mode(0o755)).unwrap();
        let mut retained = BTreeMap::new();
        let artifact = capture_artifact(
            "shell",
            ArtifactSelection {
                requirement_key: "shell",
                selected_alternative: Some(0),
                selection: ExecutableSelectionV1::CompleteCatalogAlternative,
            },
            "tool",
            "direct-launcher",
            &admitted,
            &mut retained,
        )
        .unwrap();
        let manifest = ExecutableManifestV1::finish(vec![artifact.clone(), artifact]);
        let error = validate_decoded_manifest(&manifest, Some("shell"))
            .unwrap_err()
            .to_string();
        assert!(error.contains("repeats backend `shell` command `tool`"));
    }

    #[test]
    fn child_manifest_rejects_reserved_command_under_direct_launcher_role() {
        let temp = tempfile::tempdir().unwrap();
        let admitted = temp.path().join("tool");
        fs::write(&admitted, b"#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(&admitted, fs::Permissions::from_mode(0o755)).unwrap();
        let mut retained = BTreeMap::new();
        let mut artifact = capture_artifact(
            "shell",
            ArtifactSelection {
                requirement_key: "shell",
                selected_alternative: Some(0),
                selection: ExecutableSelectionV1::CompleteCatalogAlternative,
            },
            "sh",
            "direct-launcher",
            &admitted,
            &mut retained,
        )
        .unwrap();
        artifact.logical_command = CURRENT_O_LOGICAL_COMMAND.to_string();
        let manifest = ExecutableManifestV1::finish(vec![artifact]);
        let error = validate_decoded_manifest(&manifest, Some("shell"))
            .unwrap_err()
            .to_string();
        assert!(error.contains("invalid role `direct-launcher`"));
    }

    #[test]
    fn child_manifest_rejects_inconsistent_backend_selection() {
        let admitted = std::env::current_exe().unwrap();
        let mut retained = BTreeMap::new();
        let direct = capture_artifact(
            "shell",
            ArtifactSelection {
                requirement_key: "shell",
                selected_alternative: Some(0),
                selection: ExecutableSelectionV1::CompleteCatalogAlternative,
            },
            "sh",
            "direct-launcher",
            &admitted,
            &mut retained,
        )
        .unwrap();
        let proxy = capture_artifact(
            "shell",
            ArtifactSelection {
                requirement_key: "shell",
                selected_alternative: Some(0),
                selection: ExecutableSelectionV1::AdapterDirectLauncherRefinement,
            },
            CURRENT_O_LOGICAL_COMMAND,
            "ostadix-proxy",
            &admitted,
            &mut retained,
        )
        .unwrap();
        let manifest = ExecutableManifestV1::finish(vec![direct, proxy]);
        let error = validate_decoded_manifest(&manifest, Some("shell"))
            .unwrap_err()
            .to_string();
        assert!(error.contains("backend `shell` has inconsistent selection"));
    }

    #[test]
    fn child_manifest_rejects_incomplete_webassembly_alternative() {
        let temp = tempfile::tempdir().unwrap();
        let admitted = temp.path().join("tool");
        fs::write(&admitted, b"#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(&admitted, fs::Permissions::from_mode(0o755)).unwrap();
        let mut retained = BTreeMap::new();
        let artifact = capture_artifact(
            "webassembly",
            ArtifactSelection {
                requirement_key: "webassembly",
                selected_alternative: Some(0),
                selection: ExecutableSelectionV1::CompleteCatalogAlternative,
            },
            "wasmtime",
            "direct-launcher",
            &admitted,
            &mut retained,
        )
        .unwrap();
        let manifest = ExecutableManifestV1::finish(vec![artifact]);
        let error = validate_decoded_manifest(&manifest, Some("webassembly"))
            .unwrap_err()
            .to_string();
        assert!(error.contains(
            "backend `webassembly` does not bind its complete selected catalog alternative"
        ));
    }

    #[test]
    fn child_manifest_accepts_complete_wasm_tools_webassembly_alternative() {
        let temp = tempfile::tempdir().unwrap();
        let admitted = temp.path().join("tool");
        fs::write(&admitted, b"#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(&admitted, fs::Permissions::from_mode(0o755)).unwrap();
        let mut retained = BTreeMap::new();
        let artifacts = ["wasm-tools", "wasmtime"]
            .into_iter()
            .map(|logical_command| {
                capture_artifact(
                    "webassembly",
                    ArtifactSelection {
                        requirement_key: "webassembly",
                        selected_alternative: Some(2),
                        selection: ExecutableSelectionV1::CompleteCatalogAlternative,
                    },
                    logical_command,
                    "direct-launcher",
                    &admitted,
                    &mut retained,
                )
                .unwrap()
            })
            .collect::<Vec<_>>();
        let manifest = ExecutableManifestV1::finish(artifacts);

        validate_decoded_manifest(&manifest, Some("webassembly")).unwrap();
    }
}
