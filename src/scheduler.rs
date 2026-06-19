// ─────────────────────────────────────────────────────────────────────────────
// src/scheduler.rs
//
// STEP-4: Autonomous scheduler — buffered, concurrent, disk-cached Request
// dispatch for Policy::Autonomous.
//
// Provides:
//   DiskCache            — persistent on-disk cache keyed by fingerprint hex
//   AutonomousScheduler  — concurrent topological dispatch of Nix-family Requests
//
// Architecture:
//   Under Policy::Autonomous, non-Eval Requests are buffered by the Evaluator
//   instead of being executed immediately. At force points (exit of an
//   autonomous(...) block, explicit now(), document end), the Evaluator calls
//   execute_batch() here. The scheduler:
//     1. Collects the full transitive closure of all buffered root Requests.
//     2. Pre-populates results from the two-level cache (L1 memory, L2 disk).
//     3. Builds a dependency graph (fingerprint → dep fingerprints).
//     4. Executes remaining requests in topological waves, dispatching
//        independent nodes as concurrent std::threads (up to `parallelism`
//        at a time per wave).
//
//   RequestKind::Eval is excluded from this path. Eval requests need the
//   ProcessRegistry (which lives in the Evaluator and is !Send). They are
//   executed eagerly even under Autonomous policy, bypassing the buffer. Full
//   Eval-level parallelism is a STEP5 goal; an `eval_fn` callback parameter
//   is provided on execute_batch so the wiring can be added later without
//   changing the public API.
//
// Cache layout:
//   {cache_dir}/{fingerprint}.json — one JSON file per cached result.
//   Writes use atomic rename (tmp → final) to avoid partial-write corruption.
//   The default cache dir is $XDG_CACHE_HOME/o-lang/sched, falling back to
//   ~/.cache/o-lang/sched or $TMPDIR/o-lang-cache/sched.
//
// Parallelism:
//   Defaults to min(available_cpus, 8) to avoid overwhelming the Nix daemon.
//   Can be overridden via AutonomousScheduler::with_parallelism.
// ─────────────────────────────────────────────────────────────────────────────

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;

use anyhow::{anyhow, bail, Context, Result};

use crate::nix_ops;
use crate::nixos_ops;
use crate::value::{OValue, RequestKind};

// ═════════════════════════════════════════════════════════════════════════════
// DiskCache
// ═════════════════════════════════════════════════════════════════════════════

/// Persistent on-disk cache for Request execution results.
///
/// Layout: `{dir}/{fingerprint}.json` — one file per cached result.
/// Serialization: serde_json (same OValue wire format as the IPC protocol).
///
/// Failure modes are intentionally soft:
///   - A read miss or parse error falls through to re-execution.
///   - A write error is logged to stderr but never propagates to the caller.
///   - Partial writes are avoided by atomic write-then-rename.
pub struct DiskCache {
    dir: PathBuf,
}

impl DiskCache {
    /// Open or create a cache at `dir`. Creates all missing parent directories.
    pub fn new(dir: PathBuf) -> Result<Self> {
        fs::create_dir_all(&dir)
            .with_context(|| format!("failed to create disk cache dir: {}", dir.display()))?;
        Ok(Self { dir })
    }

    /// The default cache directory:
    ///   $XDG_CACHE_HOME/o-lang/sched   (if XDG_CACHE_HOME is set)
    ///   ~/.cache/o-lang/sched           (if HOME is set)
    ///   $TMPDIR/o-lang-cache/sched      (fallback)
    pub fn default_dir() -> PathBuf {
        if let Ok(xdg) = std::env::var("XDG_CACHE_HOME") {
            return PathBuf::from(xdg).join("o-lang").join("sched");
        }
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(home).join(".cache").join("o-lang").join("sched");
        }
        std::env::temp_dir().join("o-lang-cache").join("sched")
    }

    /// Look up `fingerprint` in the cache. Returns `None` on miss or any error.
    pub fn get(&self, fingerprint: &str) -> Option<OValue> {
        let path = self.dir.join(format!("{fingerprint}.json"));
        let bytes = fs::read(&path).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    /// Write `value` to the cache under `fingerprint`.
    ///
    /// Uses an atomic write-then-rename sequence. Errors are printed to stderr
    /// and silently swallowed — a cache write failure must never abort a build.
    pub fn put(&self, fingerprint: &str, value: &OValue) {
        let fp_short = &fingerprint[..fingerprint.len().min(8)];
        let path = self.dir.join(format!("{fingerprint}.json"));
        let tmp  = self.dir.join(format!("{fingerprint}.json.tmp"));

        match serde_json::to_vec(value) {
            Err(e) => {
                eprintln!("[o-lang scheduler] cache serialize failed for {fp_short}: {e}");
            }
            Ok(bytes) => {
                if let Err(e) = fs::write(&tmp, &bytes) {
                    eprintln!("[o-lang scheduler] cache write failed for {fp_short}: {e}");
                    return;
                }
                if let Err(e) = fs::rename(&tmp, &path) {
                    eprintln!("[o-lang scheduler] cache rename failed for {fp_short}: {e}");
                    let _ = fs::remove_file(&tmp);
                }
            }
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// Dependency graph helpers
// ═════════════════════════════════════════════════════════════════════════════

/// Recursively collect all Request values reachable from `req` through its
/// source chain. Inserts `fingerprint → OValue` into `out`. Deduplicates by
/// fingerprint so visiting a node twice is a no-op.
///
/// Terminates when the source is not a Request (NixExpr, Derivation, etc.).
pub fn collect_transitive_requests(req: &OValue, out: &mut HashMap<String, OValue>) {
    if let OValue::Request { fingerprint, source, .. } = req {
        if out.contains_key(fingerprint) {
            return; // already visited
        }
        out.insert(fingerprint.clone(), req.clone());
        collect_transitive_requests(source, out);
    }
}

/// Build a dependency graph from a fingerprint-indexed request map.
///
/// Returns `HashMap<fp, Vec<dep_fp>>` where each `dep_fp` is the fingerprint
/// of a request that must complete before `fp` can start. A request's only
/// structural dep is its direct source (if the source itself is a Request);
/// deeper nesting is handled by the topological loop iterating to fixpoint.
fn build_dep_graph(all: &HashMap<String, OValue>) -> HashMap<String, Vec<String>> {
    let mut graph: HashMap<String, Vec<String>> = HashMap::new();
    for (fp, req) in all {
        if let OValue::Request { source, .. } = req {
            let dep = if let OValue::Request { fingerprint: dep_fp, .. } = source.as_ref() {
                // Only add the dep if it is in our batch (it might be a
                // pre-existing request that's already cached; we don't need
                // to track it as a pending dep in that case).
                if all.contains_key(dep_fp) {
                    vec![dep_fp.clone()]
                } else {
                    vec![]
                }
            } else {
                vec![] // source is a plain value, no request dep
            };
            graph.insert(fp.clone(), dep);
        }
    }
    graph
}

/// Resolve the source value of a request, substituting any Request source
/// with the result already in `resolved`.
///
/// Used just before dispatching a thread: the thread receives the concrete
/// value to act on (NixExpr, Derivation, StorePath, ...) rather than a
/// Request reference.
fn resolve_source(req: &OValue, resolved: &HashMap<String, OValue>) -> Result<OValue> {
    match req {
        OValue::Request { source, .. } => match source.as_ref() {
            OValue::Request { fingerprint, .. } => resolved
                .get(fingerprint)
                .cloned()
                .ok_or_else(|| {
                    let short = &fingerprint[..fingerprint.len().min(8)];
                    anyhow!("scheduler: dep {short} not yet resolved (BUG: should be ready)")
                }),
            other => Ok(other.clone()),
        },
        other => bail!("resolve_source expected a Request, got {}", other.type_name()),
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// AutonomousScheduler
// ═════════════════════════════════════════════════════════════════════════════

/// STEP-4 autonomous scheduler for concurrent, disk-cached Request dispatch.
///
/// Handles `RequestKind::Instantiate`, `Realise`, and `Activate`. Eval requests
/// are not handled here (they need the Evaluator's ProcessRegistry and are
/// excluded from the buffer). See `src/eval.rs::auto_resolve` for the split.
///
/// Call `execute_batch` at force points (end of an `autonomous(...)` block,
/// explicit `now()`, document end) to flush all buffered roots.
pub struct AutonomousScheduler {
    /// L1 in-memory cache (fingerprint → result). Survives the lifetime of
    /// the Evaluator; wiped when the Evaluator is dropped.
    pub(crate) mem_cache: HashMap<String, OValue>,
    /// L2 persistent on-disk cache. `None` if the cache directory could not be
    /// created (read-only filesystem, running in a container without a home
    /// directory, etc.).
    disk_cache: Option<DiskCache>,
    /// Maximum number of concurrent threads per dispatch wave.
    pub(crate) parallelism: usize,
}

impl AutonomousScheduler {
    /// Create a scheduler with the default disk cache and auto-detected
    /// parallelism (min(CPU count, 8)).
    pub fn new() -> Self {
        let parallelism = thread::available_parallelism()
            .map(|n| n.get().min(8))
            .unwrap_or(4);
        let disk_cache = DiskCache::new(DiskCache::default_dir()).ok();
        Self { mem_cache: HashMap::new(), disk_cache, parallelism }
    }

    /// Create a scheduler that writes its disk cache to a specific directory.
    /// Useful for tests and for cases where the default XDG path is wrong.
    pub fn with_cache_dir(dir: PathBuf) -> Result<Self> {
        let parallelism = thread::available_parallelism()
            .map(|n| n.get().min(8))
            .unwrap_or(4);
        Ok(Self {
            mem_cache: HashMap::new(),
            disk_cache: Some(DiskCache::new(dir)?),
            parallelism,
        })
    }

    /// Override the parallelism cap. Useful in tests or on machines where
    /// the default 8-thread cap is wrong.
    pub fn with_parallelism(mut self, n: usize) -> Self {
        self.parallelism = n.max(1);
        self
    }

    /// Create a scheduler with no disk cache. For tests and ephemeral runs.
    #[cfg(test)]
    pub fn no_disk() -> Self {
        Self { mem_cache: HashMap::new(), disk_cache: None, parallelism: 2 }
    }

    // ── Cache access ──────────────────────────────────────────────────────────

    /// Look up `fingerprint` in L1 then L2. Promotes L2 hits into L1.
    pub fn cache_get(&mut self, fingerprint: &str) -> Option<OValue> {
        if let Some(v) = self.mem_cache.get(fingerprint) {
            return Some(v.clone());
        }
        if let Some(disk) = &self.disk_cache {
            if let Some(v) = disk.get(fingerprint) {
                self.mem_cache.insert(fingerprint.to_string(), v.clone());
                return Some(v);
            }
        }
        None
    }

    fn cache_put(&mut self, fingerprint: &str, value: OValue) {
        if let Some(disk) = &self.disk_cache {
            disk.put(fingerprint, &value);
        }
        self.mem_cache.insert(fingerprint.to_string(), value);
    }

    // ── Batch execution ───────────────────────────────────────────────────────

    /// Execute a batch of root Requests concurrently, respecting dependencies.
    ///
    /// Step-by-step:
    ///   1. Collect the full transitive closure of all requests from `roots`.
    ///   2. Pre-populate results for cache hits (L1 → L2 lookup).
    ///   3. Build a DAG (fingerprint → dep_fingerprints).
    ///   4. Loop until all requests are resolved:
    ///      a. Find "ready" nodes (all deps already resolved).
    ///      b. Dispatch Instantiate/Realise/Activate as concurrent threads,
    ///         up to `self.parallelism` threads per wave.
    ///      c. Execute any Eval requests via the `eval_fn` callback (one at a
    ///         time — Eval needs the ProcessRegistry which is !Send).
    ///      d. Collect thread/callback results, update cache.
    ///
    /// Returns a fingerprint → OValue map covering every request in the closure.
    ///
    /// `eval_fn` is `None` in the common case where the batch contains no Eval
    /// requests. If Eval requests are encountered and `eval_fn` is None, the
    /// method returns an error (rather than silently skipping them).
    pub fn execute_batch(
        &mut self,
        roots: &[OValue],
        mut eval_fn: Option<&mut dyn FnMut(&OValue) -> Result<OValue>>,
    ) -> Result<HashMap<String, OValue>> {
        // 1. Collect all unique requests (transitive closure of all roots).
        let mut all: HashMap<String, OValue> = HashMap::new();
        for root in roots {
            collect_transitive_requests(root, &mut all);
        }
        if all.is_empty() {
            return Ok(HashMap::new());
        }

        // 2. Pre-populate from caches.
        let mut resolved: HashMap<String, OValue> = HashMap::new();
        for fp in all.keys() {
            if let Some(v) = self.cache_get(fp) {
                resolved.insert(fp.clone(), v);
            }
        }

        // 3. Build dependency graph for the remaining work.
        let dep_graph = build_dep_graph(&all);

        // 4. Topological dispatch loop.
        let mut pending: HashSet<String> = all.keys()
            .filter(|fp| !resolved.contains_key(*fp))
            .cloned()
            .collect();

        while !pending.is_empty() {
            // Find nodes whose deps are all resolved.
            let ready: Vec<String> = pending.iter()
                .filter(|fp| {
                    dep_graph
                        .get(*fp)
                        .map(|deps| deps.iter().all(|d| resolved.contains_key(d)))
                        .unwrap_or(true)
                })
                .cloned()
                .collect();

            if ready.is_empty() {
                bail!(
                    "autonomous scheduler: dependency stall ({} pending, 0 ready). \
                     Possible cycle. Pending fingerprints: {:?}",
                    pending.len(),
                    pending.iter().take(3).collect::<Vec<_>>()
                );
            }

            // Classify ready nodes into thread-dispatchable vs serial.
            let mut threadable: Vec<String> = Vec::new();
            let mut serial:     Vec<String> = Vec::new();
            for fp in &ready {
                if let Some(OValue::Request { kind, .. }) = all.get(fp) {
                    match kind {
                        RequestKind::Instantiate |
                        RequestKind::Realise     |
                        RequestKind::Activate { .. } => threadable.push(fp.clone()),
                        _ => serial.push(fp.clone()),
                    }
                }
            }

            // ── Concurrent dispatch for Nix-family requests ───────────────────
            // Dispatch one wave of up to `parallelism` threadable requests.
            // We use a wave (rather than a streaming work-queue) to keep the
            // borrow structure simple: all threads complete before we write
            // results back to `resolved`.
            let wave: Vec<String> = threadable.into_iter().take(self.parallelism).collect();
            if !wave.is_empty() {
                let (tx, rx) = mpsc::channel::<(String, Result<OValue>)>();

                for fp in &wave {
                    let req  = all[fp].clone();
                    let src  = resolve_source(&req, &resolved)?;
                    let kind = match &req {
                        OValue::Request { kind, .. } => kind.clone(),
                        _ => unreachable!(),
                    };
                    let fp_c  = fp.clone();
                    let tx_c  = tx.clone();

                    thread::spawn(move || {
                        let result = match kind {
                            RequestKind::Instantiate => nix_ops::instantiate_nix(&src),
                            RequestKind::Realise     => nix_ops::realise_nix(&src),
                            RequestKind::Activate { ref profile, dry_run } => {
                                nixos_ops::activate_nix(&src, profile, dry_run)
                            }
                            _ => Err(anyhow!("unexpected kind in concurrent dispatch: {:?}", kind)),
                        };
                        let _ = tx_c.send((fp_c, result));
                    });
                }
                drop(tx); // close; receiver loop below terminates when all senders drop

                for (fp, result) in rx {
                    let value = result.with_context(|| {
                        format!("autonomous scheduler: request {} failed", &fp[..fp.len().min(8)])
                    })?;
                    self.cache_put(&fp, value.clone());
                    resolved.insert(fp.clone(), value);
                    pending.remove(&fp);
                }
            }

            // ── Serial dispatch for Eval requests ─────────────────────────────
            // Process one Eval request per loop iteration. We process one at a
            // time (rather than all serial-ready nodes at once) so that we
            // re-evaluate the ready set after each completion — a resolved Eval
            // might unblock threadable nodes that can then run concurrently in
            // the next wave.
            //
            // `eval_fn` is an independent mutable reference from the caller and
            // does not borrow from `self`, so it can be reborrowred here while
            // `self` is used for cache_put / mem_cache.insert below.
            if let Some(fp) = serial.into_iter().next() {
                let req = all[&fp].clone();
                let result = if let Some(ref mut ef) = eval_fn {
                    ef(&req)?
                } else {
                    bail!(
                        "autonomous scheduler: encountered RequestKind::Eval \
                         but no eval_fn callback was provided (fp: {}). \
                         This is a bug — Eval requests should be excluded from \
                         the autonomous buffer.",
                        &fp[..fp.len().min(8)]
                    )
                };
                // Eval results are stored only in mem_cache (the Evaluator's
                // eval_cache is the canonical store for {lazy} results; writing
                // to disk would duplicate that cache with different semantics).
                self.mem_cache.insert(fp.clone(), result.clone());
                resolved.insert(fp.clone(), result);
                pending.remove(&fp);
            }
        }

        Ok(resolved)
    }

    /// Execute a single Request using the full scheduler pipeline.
    ///
    /// Convenience wrapper around `execute_batch` for the common case of a
    /// single root with no Eval requests. Returns the result for `req`.
    pub fn execute(&mut self, req: &OValue) -> Result<OValue> {
        let fp = match req {
            OValue::Request { fingerprint, .. } => fingerprint.clone(),
            other => bail!(
                "AutonomousScheduler::execute expected a Request, got {}",
                other.type_name()
            ),
        };

        // Fast path: cache hit — no graph traversal needed.
        if let Some(v) = self.cache_get(&fp) {
            return Ok(v);
        }

        let results = self.execute_batch(std::slice::from_ref(req), None)?;
        results
            .get(&fp)
            .cloned()
            .ok_or_else(|| anyhow!("scheduler: root request {} not in results", &fp[..fp.len().min(8)]))
    }
}

impl Default for AutonomousScheduler {
    fn default() -> Self { Self::new() }
}

// ═════════════════════════════════════════════════════════════════════════════
// Tests
// ═════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::sync::{Arc, Mutex};

    // ── DiskCache ──────────────────────────────────────────────────────────────

    fn tmp_cache_dir(tag: &str) -> PathBuf {
        let dir = env::temp_dir().join(format!("o-lang-sched-test-{tag}"));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn disk_cache_miss_returns_none() {
        let dir   = tmp_cache_dir("miss");
        let cache = DiskCache::new(dir).unwrap();
        assert!(cache.get("nonexistent_fingerprint").is_none());
    }

    #[test]
    fn disk_cache_put_then_get_roundtrips_ovalue() {
        let dir   = tmp_cache_dir("put-get");
        let cache = DiskCache::new(dir).unwrap();
        let val   = OValue::str_("hello from cache");
        cache.put("abc123", &val);
        let got = cache.get("abc123").expect("cache should have hit");
        assert_eq!(got, val);
    }

    #[test]
    fn disk_cache_int_roundtrip() {
        let dir   = tmp_cache_dir("int");
        let cache = DiskCache::new(dir).unwrap();
        cache.put("fp_int", &OValue::int(42));
        assert_eq!(cache.get("fp_int").unwrap(), OValue::int(42));
    }

    #[test]
    fn disk_cache_put_is_idempotent() {
        // Writing twice: second write wins, no corruption.
        let dir   = tmp_cache_dir("idempotent");
        let cache = DiskCache::new(dir).unwrap();
        cache.put("fp", &OValue::int(1));
        cache.put("fp", &OValue::int(2));
        assert_eq!(cache.get("fp").unwrap(), OValue::int(2));
    }

    #[test]
    fn disk_cache_store_path_roundtrip() {
        let dir   = tmp_cache_dir("store-path");
        let cache = DiskCache::new(dir).unwrap();
        let sp    = OValue::store_path("/nix/store/abc-hello");
        cache.put("sp_fp", &sp);
        assert_eq!(cache.get("sp_fp").unwrap(), sp);
    }

    // ── collect_transitive_requests ────────────────────────────────────────────

    #[test]
    fn collect_single_request_no_chain() {
        let expr = OValue::nix_expr("pkgs.hello", vec![]);
        let req  = OValue::request(RequestKind::Instantiate, expr);
        let mut all = HashMap::new();
        collect_transitive_requests(&req, &mut all);
        assert_eq!(all.len(), 1);
        if let OValue::Request { fingerprint, .. } = &req {
            assert!(all.contains_key(fingerprint));
        } else {
            panic!("expected Request");
        }
    }

    #[test]
    fn collect_two_level_chain() {
        let expr     = OValue::nix_expr("pkgs.hello", vec![]);
        let inst_req = OValue::request(RequestKind::Instantiate, expr);
        let real_req = OValue::request(RequestKind::Realise, inst_req.clone());

        let mut all = HashMap::new();
        collect_transitive_requests(&real_req, &mut all);
        // Should contain both the Realise and the nested Instantiate.
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn collect_is_idempotent_on_duplicate_roots() {
        let expr = OValue::nix_expr("pkgs.hello", vec![]);
        let req  = OValue::request(RequestKind::Instantiate, expr);
        let mut all = HashMap::new();
        collect_transitive_requests(&req, &mut all);
        collect_transitive_requests(&req, &mut all); // second call is a no-op
        assert_eq!(all.len(), 1);
    }

    // ── build_dep_graph ────────────────────────────────────────────────────────

    #[test]
    fn dep_graph_single_request_has_no_deps() {
        let expr = OValue::nix_expr("pkgs.hello", vec![]);
        let req  = OValue::request(RequestKind::Instantiate, expr);
        let fp   = match &req { OValue::Request { fingerprint, .. } => fingerprint.clone(), _ => panic!() };
        let mut all = HashMap::new();
        all.insert(fp.clone(), req);
        let graph = build_dep_graph(&all);
        assert!(graph[&fp].is_empty());
    }

    #[test]
    fn dep_graph_realise_depends_on_instantiate() {
        let expr     = OValue::nix_expr("pkgs.hello", vec![]);
        let inst_req = OValue::request(RequestKind::Instantiate, expr);
        let real_req = OValue::request(RequestKind::Realise, inst_req.clone());

        let inst_fp = match &inst_req { OValue::Request { fingerprint, .. } => fingerprint.clone(), _ => panic!() };
        let real_fp = match &real_req { OValue::Request { fingerprint, .. } => fingerprint.clone(), _ => panic!() };

        let mut all = HashMap::new();
        all.insert(inst_fp.clone(), inst_req);
        all.insert(real_fp.clone(), real_req);

        let graph = build_dep_graph(&all);
        assert!(graph[&inst_fp].is_empty(), "Instantiate has no Request dep");
        assert_eq!(graph[&real_fp], vec![inst_fp], "Realise depends on Instantiate");
    }

    // ── AutonomousScheduler: cache lookup ─────────────────────────────────────

    #[test]
    fn scheduler_mem_cache_hit_bypasses_execution() {
        let mut sched = AutonomousScheduler::no_disk();
        let expr = OValue::nix_expr("pkgs.hello", vec![]);
        let req  = OValue::request(RequestKind::Instantiate, expr);
        let fp   = match &req { OValue::Request { fingerprint, .. } => fingerprint.clone(), _ => panic!() };

        // Seed the in-memory cache with a fake result.
        sched.mem_cache.insert(fp.clone(), OValue::store_path("/nix/store/fake-cached"));

        // execute() should return the cached value without calling nix.
        let got = sched.execute(&req).unwrap();
        assert_eq!(got, OValue::store_path("/nix/store/fake-cached"));
    }

    #[test]
    fn scheduler_disk_cache_hit_bypasses_execution() {
        let dir  = tmp_cache_dir("sched-disk-hit");
        let mut sched = AutonomousScheduler::with_cache_dir(dir).unwrap();
        let expr = OValue::nix_expr("pkgs.hello", vec![]);
        let req  = OValue::request(RequestKind::Instantiate, expr);
        let fp   = match &req { OValue::Request { fingerprint, .. } => fingerprint.clone(), _ => panic!() };

        // Write result to disk cache.
        let cached_val = OValue::store_path("/nix/store/disk-cached-hello");
        sched.disk_cache.as_ref().unwrap().put(&fp, &cached_val);

        // execute() should find the disk hit, promote to L1, and return it.
        let got = sched.execute(&req).unwrap();
        assert_eq!(got, cached_val);

        // Also verify L1 promotion happened.
        assert!(sched.mem_cache.contains_key(&fp));
    }

    #[test]
    fn scheduler_disk_cache_promoted_to_mem_cache() {
        let dir  = tmp_cache_dir("sched-promote");
        let mut sched = AutonomousScheduler::with_cache_dir(dir).unwrap();
        let fp = "aaaa1111".to_string();
        sched.disk_cache.as_ref().unwrap().put(&fp, &OValue::int(99));

        // First cache_get: L2 hit → promotes to L1.
        let v1 = sched.cache_get(&fp).unwrap();
        assert_eq!(v1, OValue::int(99));
        // L1 now populated.
        assert!(sched.mem_cache.contains_key(&fp));
        // Second call: L1 hit.
        let v2 = sched.cache_get(&fp).unwrap();
        assert_eq!(v2, OValue::int(99));
    }

    // ── execute_batch: Eval request with callback ─────────────────────────────

    #[test]
    fn execute_batch_dispatches_eval_via_callback() {
        let mut sched = AutonomousScheduler::no_disk();

        let thunk = OValue::thunk("x = 1 + 1", vec![]);
        let req   = OValue::request(
            RequestKind::Eval { lang: "python".to_string(), env_id: u32::MAX, cacheable: false },
            thunk,
        );
        let fp = match &req { OValue::Request { fingerprint, .. } => fingerprint.clone(), _ => panic!() };

        let called = Arc::new(Mutex::new(false));
        let called_c = called.clone();
        let mut eval_fn = move |_req: &OValue| -> Result<OValue> {
            *called_c.lock().unwrap() = true;
            Ok(OValue::int(2))
        };

        let results = sched.execute_batch(&[req], Some(&mut eval_fn)).unwrap();
        assert!(*called.lock().unwrap(), "eval_fn should have been called");
        assert_eq!(results[&fp], OValue::int(2));
    }

    #[test]
    fn execute_batch_errors_on_eval_without_callback() {
        let mut sched = AutonomousScheduler::no_disk();
        let thunk = OValue::thunk("x = 1", vec![]);
        let req   = OValue::request(
            RequestKind::Eval { lang: "python".to_string(), env_id: 0, cacheable: false },
            thunk,
        );
        let err = sched.execute_batch(&[req], None).unwrap_err();
        assert!(err.to_string().contains("eval_fn"));
    }

    #[test]
    fn execute_batch_empty_roots_returns_empty_map() {
        let mut sched = AutonomousScheduler::no_disk();
        let result = sched.execute_batch(&[], None).unwrap();
        assert!(result.is_empty());
    }

    // ── execute_batch: pre-cached chain ───────────────────────────────────────

    #[test]
    fn execute_batch_skips_cached_requests() {
        let mut sched = AutonomousScheduler::no_disk();

        let expr     = OValue::nix_expr("pkgs.hello", vec![]);
        let inst_req = OValue::request(RequestKind::Instantiate, expr);
        let real_req = OValue::request(RequestKind::Realise, inst_req.clone());

        let inst_fp = match &inst_req { OValue::Request { fingerprint, .. } => fingerprint.clone(), _ => panic!() };
        let real_fp = match &real_req { OValue::Request { fingerprint, .. } => fingerprint.clone(), _ => panic!() };

        // Pre-populate both cache slots. execute_batch should NOT try to call nix.
        let fake_drv  = OValue::derivation("/nix/store/fake.drv", vec!["out".into()], vec![]);
        let fake_path = OValue::store_path("/nix/store/fake-hello-out");
        sched.mem_cache.insert(inst_fp.clone(), fake_drv);
        sched.mem_cache.insert(real_fp.clone(), fake_path.clone());

        let results = sched.execute_batch(&[real_req], None).unwrap();
        assert_eq!(results[&real_fp], fake_path);
    }

    // ── execute: wrong input type ─────────────────────────────────────────────

    #[test]
    fn execute_errors_on_non_request() {
        let mut sched = AutonomousScheduler::no_disk();
        let err = sched.execute(&OValue::int(42)).unwrap_err();
        assert!(err.to_string().contains("Request"));
    }
}
