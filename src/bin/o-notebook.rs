/// O ◦ Notebook — a Jupyter-style interactive notebook for O-lang.
///
/// Launch:  cargo run --features notebook --bin o-notebook [backends_dir]
/// Opens http://localhost:8888 in your default browser automatically.
///
/// Variable bindings introduced with `let` in one cell are visible in all
/// subsequent cells (like Jupyter kernel state). Use "Restart Kernel" to
/// clear all bindings and subprocess state.
use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use axum::{
    extract::State,
    response::Html,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;

use o_lang::eval::Evaluator;
use o_lang::parser::Parser;
use o_lang::value::OValue;

// ─── Session ─────────────────────────────────────────────────────────────────

/// Mutable per-session kernel state: the evaluator (which owns the backend
/// subprocess registry) and the accumulated O-level variable scope.
struct Session {
    evaluator: Evaluator,
    scope: HashMap<String, OValue>,
}

impl Session {
    fn new(shim_dir: PathBuf, backends: HashSet<String>) -> Self {
        Session {
            evaluator: Evaluator::new(shim_dir).with_registered_backends(backends),
            scope: HashMap::new(),
        }
    }
}

// ─── Shared state ────────────────────────────────────────────────────────────

#[derive(Clone)]
struct AppState {
    session: Arc<Mutex<Session>>,
    shim_dir: Arc<PathBuf>,
    backends: Arc<HashSet<String>>,
}

impl AppState {
    fn new_session(&self) {
        let mut guard = self.session.lock().unwrap();
        *guard = Session::new((*self.shim_dir).clone(), (*self.backends).clone());
    }
}

// ─── API types ───────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct EvalRequest {
    code: String,
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum EvalOutput {
    Text { display: String },
    Html { html: String },
    Image { mime: String, data: String },
    Null,
}

#[derive(Serialize)]
struct EvalResponse {
    ok: bool,
    value_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<EvalOutput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

// ─── Handlers ────────────────────────────────────────────────────────────────

async fn root() -> Html<&'static str> {
    Html(NOTEBOOK_HTML)
}

async fn eval(State(state): State<AppState>, Json(req): Json<EvalRequest>) -> Json<EvalResponse> {
    let session = state.session.clone();
    let backends = state.backends.clone();
    let code = req.code.trim().to_string();

    if code.is_empty() {
        return Json(EvalResponse {
            ok: true,
            value_type: "null".into(),
            result: Some(EvalOutput::Null),
            error: None,
        });
    }

    let result = tokio::task::spawn_blocking(move || {
        let mut parser = Parser::new(&code, &backends);
        let nodes = parser.parse()?;
        // Lock inside spawn_blocking so we don't hold the Mutex across an
        // await point — this is fine since the closure runs on a thread pool.
        let mut guard = session.lock().unwrap();
        // Destructure to split the borrow: the compiler cannot prove that
        // `guard.evaluator` and `guard.scope` are disjoint through `guard`
        // alone, but a single destructure makes both sub-borrows visible.
        let Session {
            ref mut evaluator,
            ref mut scope,
        } = *guard;
        evaluator.eval_document_with_scope(nodes, scope)
    })
    .await;

    match result {
        Ok(Ok(value)) => {
            let value_type = value.type_name().to_string();
            let output = match &value {
                OValue::Null => EvalOutput::Null,
                OValue::Html { v } => EvalOutput::Html { html: v.clone() },
                OValue::Blob { v, mime } if mime.starts_with("image/") => EvalOutput::Image {
                    mime: mime.clone(),
                    data: v.clone(),
                },
                other => EvalOutput::Text {
                    display: format!("{other}"),
                },
            };
            Json(EvalResponse {
                ok: true,
                value_type,
                result: Some(output),
                error: None,
            })
        }
        Ok(Err(e)) => Json(EvalResponse {
            ok: false,
            value_type: "error".into(),
            result: None,
            error: Some(e.to_string()),
        }),
        Err(e) => Json(EvalResponse {
            ok: false,
            value_type: "error".into(),
            result: None,
            error: Some(format!("internal: {e}")),
        }),
    }
}

async fn reset(State(state): State<AppState>) -> Json<serde_json::Value> {
    state.new_session();
    Json(serde_json::json!({ "ok": true }))
}

// ─── Entry point ─────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    if o_lang::backend::run_backend_from_env_args()? {
        return Ok(());
    }

    let shim_dir = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("backends"));

    let backends = Arc::new(registered_backends());
    let shim_dir = Arc::new(shim_dir);

    let state = AppState {
        session: Arc::new(Mutex::new(Session::new(
            (*shim_dir).clone(),
            (*backends).clone(),
        ))),
        shim_dir,
        backends,
    };

    let app = Router::new()
        .route("/", get(root))
        .route("/eval", post(eval))
        .route("/reset", post(reset))
        .with_state(state);

    let addr = SocketAddr::from(([127, 0, 0, 1], 8888));
    let url = format!("http://{addr}");
    let listener = TcpListener::bind(addr).await?;

    eprintln!("\x1b[1m\x1b[34m  O ◦ Notebook\x1b[0m");
    eprintln!("  \x1b[2mListening on \x1b[0m\x1b[4m{url}\x1b[0m");
    eprintln!("  \x1b[2mShift+Enter to run a cell · Ctrl+C to stop\x1b[0m\n");

    let _ = std::process::Command::new(if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    })
    .arg(&url)
    .spawn();

    axum::serve(listener, app).await?;
    Ok(())
}

fn registered_backends() -> HashSet<String> {
    [
        "O",
        "python",
        "html",
        "latex",
        "markdown",
        "bash",
        "shell",
        "rust",
        "racket",
        "nix",
        "nix_expr",
        "nix_store",
        "nixos_test",
        "text",
        "csharp",
        "cpp",
        "haskell",
        "lisp",
        "common_lisp",
        "sql",
        "ruby",
        "matlab",
        "mathematica",
        "webassembly",
        "java",
        "javascript",
        "ocaml",
        "quote",
        // Aliases (canonicalized by the parser via the BackendRegistry).
        "py",
        "md",
        "tex",
        "plain",
        "o",
    ]
    .into_iter()
    .map(String::from)
    .collect()
}

// ─── Embedded notebook UI ────────────────────────────────────────────────────

const NOTEBOOK_HTML: &str = r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>O · Notebook</title>
<style>
:root {
  --base:     #1e1e2e;
  --mantle:   #181825;
  --crust:    #11111b;
  --surface0: #313244;
  --surface1: #45475a;
  --overlay0: #6c7086;
  --overlay1: #7f849c;
  --text:     #cdd6f4;
  --subtext1: #bac2de;
  --blue:     #89b4fa;
  --green:    #a6e3a1;
  --yellow:   #f9e2af;
  --red:      #f38ba8;
  --mauve:    #cba6f7;
  --peach:    #fab387;
  --teal:     #94e2d5;
  --font-mono: 'SF Mono','Fira Code','JetBrains Mono','Cascadia Code',ui-monospace,monospace;
  --font-ui:   'SF Pro Display','Segoe UI',system-ui,-apple-system,sans-serif;
}

*,*::before,*::after { box-sizing: border-box; margin:0; padding:0; }

body {
  background: var(--base);
  color: var(--text);
  font-family: var(--font-ui);
  min-height: 100vh;
}

/* ── header ─────────────────────────────────────────────────────────────── */
header {
  position: sticky; top:0; z-index:200;
  background: var(--mantle);
  border-bottom: 1px solid var(--surface0);
  display: flex; align-items: center; gap: 10px;
  padding: 9px 20px;
}

.logo {
  font-weight: 700; font-size: 17px;
  color: var(--blue); letter-spacing: -0.3px;
  user-select: none; display:flex; align-items:center; gap:6px;
}
.logo-dot { color: var(--mauve); }

.vsep { width:1px; height:20px; background:var(--surface0); }

.kernel-badge {
  display: flex; align-items: center; gap: 6px;
  font-size: 12px; color: var(--overlay1);
}
.kdot {
  width:7px; height:7px; border-radius:50%;
  background: var(--green);
  transition: background 0.2s;
}
.kdot.busy { background:var(--yellow); animation:blink 0.9s infinite; }
.kdot.error { background:var(--red); }

.spacer { flex:1; }

.hbtn {
  display:inline-flex; align-items:center; gap:5px;
  background:var(--surface0); border:1px solid transparent;
  color:var(--text); padding:5px 12px;
  border-radius:7px; cursor:pointer; font-size:13px;
  font-family:var(--font-ui);
  transition:background .15s,border-color .15s,opacity .15s;
}
.hbtn:hover { background:var(--surface1); border-color:var(--overlay0); }
.hbtn.primary {
  background:var(--blue); color:var(--crust);
  font-weight:600;
}
.hbtn.primary:hover { opacity:.88; }
.hbtn.danger { color:var(--red); }
.hbtn.danger:hover { border-color:var(--red); background:rgba(243,139,168,.08); }

/* ── notebook ───────────────────────────────────────────────────────────── */
#notebook {
  max-width: 860px; margin:0 auto;
  padding: 28px 20px 160px;
  display:flex; flex-direction:column; gap:14px;
}

/* ── cells ──────────────────────────────────────────────────────────────── */
.cell {
  position:relative;
  background:var(--mantle);
  border:1.5px solid var(--surface0);
  border-radius:12px; overflow:hidden;
  transition:border-color .2s,box-shadow .2s;
}
.cell:focus-within { border-color:var(--surface1); box-shadow:0 0 0 3px rgba(137,180,250,.07); }
.cell.s-running { border-color:var(--yellow); box-shadow:0 0 0 3px rgba(249,226,175,.07); }
.cell.s-done    { border-color:var(--surface1); }
.cell.s-error   { border-color:var(--red);    box-shadow:0 0 0 3px rgba(243,139,168,.07); }

/* accent stripe */
.cell::before {
  content:''; position:absolute; left:0; top:0; bottom:0; width:3px;
  background:transparent; transition:background .2s;
  border-radius:12px 0 0 12px;
}
.cell.s-running::before { background:var(--yellow); }
.cell.s-done::before    { background:var(--green);  }
.cell.s-error::before   { background:var(--red);    }

/* toolbar */
.ct {
  display:flex; align-items:center; gap:8px;
  padding:7px 12px 7px 14px;
  background:var(--crust);
  border-bottom:1px solid var(--surface0);
}
.ec {
  font-family:var(--font-mono); font-size:11px;
  color:var(--overlay1); min-width:36px; user-select:none;
}
.rbtn {
  display:inline-flex; align-items:center; justify-content:center;
  width:28px; height:28px;
  background:none; border:1px solid var(--surface0); border-radius:6px;
  color:var(--blue); cursor:pointer; font-size:13px;
  transition:background .12s,border-color .12s;
}
.rbtn:hover { background:var(--surface0); border-color:var(--blue); }
.rbtn.busy  { color:var(--yellow); border-color:var(--yellow); }
.lang-badge {
  font-family:var(--font-mono); font-size:10px;
  color:var(--overlay0); padding:1px 6px;
  background:var(--surface0); border-radius:4px; user-select:none;
}
.ca { margin-left:auto; display:flex; gap:2px; opacity:0; transition:opacity .15s; }
.cell:hover .ca, .cell:focus-within .ca { opacity:1; }
.cbtn {
  background:none; border:none; color:var(--overlay1);
  cursor:pointer; font-size:13px; padding:3px 7px;
  border-radius:5px; font-family:var(--font-ui);
  transition:background .1s,color .1s;
}
.cbtn:hover { background:var(--surface0); color:var(--text); }
.cbtn.del:hover { color:var(--red); }

/* input */
.ci {
  display:block; width:100%;
  background:transparent; border:none;
  padding:14px 16px 14px 18px;
  font-family:var(--font-mono); font-size:13.5px; line-height:1.65;
  color:var(--text); resize:none; outline:none;
  min-height:58px; overflow:hidden;
  caret-color:var(--blue);
}
.ci::placeholder { color:var(--overlay0); }

/* output */
.co {
  display:none;
  border-top:1px solid var(--surface0);
  background:var(--crust);
}
.co.show { display:block; }
.coi {
  padding:12px 18px;
  font-family:var(--font-mono); font-size:13px; line-height:1.7;
  white-space:pre-wrap; word-break:break-word;
}
.o-text  { color:var(--subtext1); }
.o-html  { font-family:var(--font-ui); color:var(--text); white-space:normal; }
.o-err   { color:var(--red); }
.o-wait  { color:var(--yellow); }
.o-type  { font-size:10px; color:var(--overlay0); margin-left:8px; font-family:var(--font-mono); }

/* add-cell strip */
.add-strip { display:flex; justify-content:center; padding:4px; }
.add-btn {
  display:inline-flex; align-items:center; gap:6px;
  background:none; border:1px dashed var(--surface1);
  color:var(--overlay1); padding:6px 20px; border-radius:8px;
  cursor:pointer; font-size:12px; font-family:var(--font-ui);
  transition:border-color .15s,color .15s,background .15s;
}
.add-btn:hover { border-color:var(--blue); color:var(--blue); background:rgba(137,180,250,.05); }

/* footer */
footer {
  position:fixed; bottom:0; left:0; right:0;
  background:var(--mantle); border-top:1px solid var(--surface0);
  padding:6px 20px; display:flex; align-items:center; gap:12px;
  font-size:11px; color:var(--overlay0);
}

/* utils */
@keyframes blink { 0%,100%{opacity:1} 50%{opacity:.3} }
::-webkit-scrollbar { width:6px; height:6px; }
::-webkit-scrollbar-track { background:transparent; }
::-webkit-scrollbar-thumb { background:var(--surface1); border-radius:3px; }
</style>
</head>
<body>

<header>
  <div class="logo"><span>O</span><span class="logo-dot">◦</span><span>Notebook</span></div>
  <div class="vsep"></div>
  <div class="kernel-badge">
    <div class="kdot" id="kdot"></div>
    <span id="klabel">Kernel ready</span>
  </div>
  <div class="spacer"></div>
  <button class="hbtn" onclick="addCell()">+ Cell</button>
  <button class="hbtn primary" onclick="runAll()">▶ Run All</button>
  <button class="hbtn" onclick="clearAll()">Clear Outputs</button>
  <button class="hbtn" onclick="saveNotebook()" title="Save notebook to JSON (Ctrl+S)">⬇ Save</button>
  <button class="hbtn" onclick="loadNotebook()" title="Load notebook from JSON">⬆ Load</button>
  <button class="hbtn danger" onclick="restartKernel()" title="Clear all variable bindings and subprocess state">↺ Restart Kernel</button>
</header>

<div id="notebook"></div>

<footer>
  <span>Shift+Enter — run &nbsp;·&nbsp; Tab — indent &nbsp;·&nbsp; ↑↓ arrows — move cell</span>
  <span id="fstatus" style="margin-left:auto"></span>
</footer>

<script>
'use strict';

let cellSeq   = 0;
let execCount = 0;
let busy      = 0;

// ── kernel indicator ─────────────────────────────────────────────────────────
function setKernel(state) {   // 'idle' | 'busy' | 'error'
  const d = document.getElementById('kdot');
  const l = document.getElementById('klabel');
  d.className = state === 'idle' ? 'kdot' : state === 'busy' ? 'kdot busy' : 'kdot error';
  l.textContent = state === 'idle' ? 'Kernel ready' : state === 'busy' ? 'Running…' : 'Kernel error';
}

function setStatus(s) { document.getElementById('fstatus').textContent = s; }

// ── cell creation ─────────────────────────────────────────────────────────────
function createCell(code = '') {
  const id  = ++cellSeq;
  const div = document.createElement('div');
  div.className = 'cell';
  div.id = `c${id}`;
  div.innerHTML = `
    <div class="ct">
      <span class="ec" id="ec${id}">[ ]</span>
      <button class="rbtn" id="rb${id}" onclick="runCell(${id})" title="Run (Shift+Enter)">▶</button>
      <span class="lang-badge">O</span>
      <div class="ca">
        <button class="cbtn" onclick="moveCell(${id},-1)" title="Move up">↑</button>
        <button class="cbtn" onclick="moveCell(${id},1)"  title="Move down">↓</button>
        <button class="cbtn" onclick="clearCell(${id})"  title="Clear output">⬜</button>
        <button class="cbtn del" onclick="deleteCell(${id})" title="Delete">✕</button>
      </div>
    </div>
    <textarea class="ci" id="ci${id}" placeholder="O-lang expression… (Shift+Enter to run)" spellcheck="false">${esc(code)}</textarea>
    <div class="co" id="co${id}"><div class="coi" id="coi${id}"></div></div>
  `;

  document.getElementById('notebook').appendChild(div);

  const ta = document.getElementById(`ci${id}`);
  ta.addEventListener('input',   () => autosize(ta));
  ta.addEventListener('keydown', e  => onKey(e, id));
  autosize(ta);
  ta.focus();
  return id;
}

function autosize(ta) {
  ta.style.height = 'auto';
  ta.style.height = Math.max(58, ta.scrollHeight) + 'px';
}

function onKey(e, id) {
  if (e.key === 'Enter' && e.shiftKey) { e.preventDefault(); runCell(id); }
  else if (e.key === 'Tab') {
    e.preventDefault();
    const ta = e.target, s = ta.selectionStart;
    ta.value = ta.value.substring(0,s) + '  ' + ta.value.substring(ta.selectionEnd);
    ta.selectionStart = ta.selectionEnd = s + 2;
  }
}

// ── run ───────────────────────────────────────────────────────────────────────
async function runCell(id) {
  const ta  = document.getElementById(`ci${id}`);
  const co  = document.getElementById(`co${id}`);
  const coi = document.getElementById(`coi${id}`);
  const rb  = document.getElementById(`rb${id}`);
  const ec  = document.getElementById(`ec${id}`);
  const cel = document.getElementById(`c${id}`);

  execCount++;
  ec.textContent = `[${execCount}]`;
  rb.className   = 'rbtn busy';
  cel.className  = 'cell s-running';
  co.className   = 'co show';
  coi.className  = 'coi';
  coi.innerHTML  = '<span class="o-wait">Running…</span>';

  busy++;
  setKernel('busy');
  setStatus(`[${execCount}] running…`);

  try {
    const res  = await fetch('/eval', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body:   JSON.stringify({ code: ta.value }),
    });
    const data = await res.json();
    applyResult(cel, co, coi, data);
  } catch(err) {
    cel.className = 'cell s-error';
    coi.innerHTML = `<span class="o-err">Network error: ${esc(err.message)}</span>`;
  } finally {
    rb.className = 'rbtn';
    if (--busy === 0) { setKernel('idle'); setStatus(''); }
  }
}

function applyResult(cel, co, coi, data) {
  if (!data.ok) {
    cel.className = 'cell s-error';
    coi.className = 'coi o-err';
    coi.innerHTML = esc(data.error || 'unknown error');
    return;
  }
  const r = data.result;
  cel.className = 'cell s-done';
  if (!r || r.type === 'null') { co.className = 'co'; coi.innerHTML = ''; return; }
  co.className = 'co show';
  const badge = `<span class="o-type">[${esc(data.value_type)}]</span>`;
  if (r.type === 'html') {
    coi.className = 'coi o-html';
    coi.innerHTML = r.html + badge;
  } else if (r.type === 'image') {
    coi.className = 'coi';
    coi.innerHTML = `<img src="data:${esc(r.mime)};base64,${r.data}" style="max-width:100%;border-radius:8px;display:block">${badge}`;
  } else {
    coi.className = 'coi o-text';
    coi.innerHTML = esc(r.display) + badge;
  }
}

// ── cell ops ──────────────────────────────────────────────────────────────────
function clearCell(id) {
  document.getElementById(`co${id}`).className  = 'co';
  document.getElementById(`coi${id}`).innerHTML = '';
  document.getElementById(`coi${id}`).className = 'coi';
  document.getElementById(`ec${id}`).textContent = '[ ]';
  document.getElementById(`c${id}`).className   = 'cell';
}

function deleteCell(id) {
  const nb = document.getElementById('notebook');
  if (nb.children.length === 1) { clearCell(id); return; }
  document.getElementById(`c${id}`)?.remove();
}

function moveCell(id, dir) {
  const nb   = document.getElementById('notebook');
  const cell = document.getElementById(`c${id}`);
  if (dir === -1 && cell.previousElementSibling) nb.insertBefore(cell, cell.previousElementSibling);
  else if (dir === 1 && cell.nextElementSibling)  nb.insertBefore(cell.nextElementSibling, cell);
}

function addCell(code = '') {
  const id = createCell(code);
  document.getElementById(`c${id}`).scrollIntoView({ behavior:'smooth', block:'nearest' });
}

async function runAll() {
  for (const cell of document.querySelectorAll('.cell')) {
    await runCell(parseInt(cell.id.slice(1), 10));
  }
}

function clearAll() {
  document.querySelectorAll('.cell').forEach(c => clearCell(parseInt(c.id.slice(1), 10)));
}

async function restartKernel() {
  if (!confirm('Restart kernel? All variable bindings will be cleared.')) return;
  try {
    await fetch('/reset', { method: 'POST' });
    clearAll();
    execCount = 0;
    document.querySelectorAll('.ec').forEach(e => e.textContent = '[ ]');
    setKernel('idle');
    setStatus('Kernel restarted');
    setTimeout(() => setStatus(''), 3000);
  } catch(e) {
    setStatus('Restart failed: ' + e.message);
  }
}

// ── utils ─────────────────────────────────────────────────────────────────────
function esc(s) {
  return String(s ?? '')
    .replace(/&/g,'&amp;').replace(/</g,'&lt;')
    .replace(/>/g,'&gt;').replace(/"/g,'&quot;');
}

// ── save / load ───────────────────────────────────────────────────────────────

function saveNotebook() {
  const cells = [];
  document.querySelectorAll('.cell').forEach(cell => {
    const id  = parseInt(cell.id.slice(1), 10);
    const ta  = document.getElementById(`ci${id}`);
    const coi = document.getElementById(`coi${id}`);
    cells.push({
      code:   ta  ? ta.value  : '',
      output: coi ? coi.innerHTML : '',
    });
  });
  const payload = JSON.stringify({ version: 1, cells }, null, 2);
  const blob = new Blob([payload], { type: 'application/json' });
  const url  = URL.createObjectURL(blob);
  const a    = Object.assign(document.createElement('a'), { href: url, download: 'notebook.o.json' });
  document.body.appendChild(a);
  a.click();
  document.body.removeChild(a);
  URL.revokeObjectURL(url);
  setStatus('Saved ✓');
  setTimeout(() => setStatus(''), 2500);
}

function loadNotebook() {
  const input = Object.assign(document.createElement('input'), {
    type: 'file', accept: '.json'
  });
  input.onchange = async (e) => {
    const file = e.target.files?.[0];
    if (!file) return;
    try {
      const text = await file.text();
      const data = JSON.parse(text);
      if (!Array.isArray(data.cells)) throw new Error('Invalid notebook format');

      // Clear current cells
      document.getElementById('notebook').innerHTML = '';
      cellSeq = 0; execCount = 0;

      // Restore cells (code + saved output if any)
      for (const cell of data.cells) {
        const id = createCell(cell.code || '');
        if (cell.output) {
          const co  = document.getElementById(`co${id}`);
          const coi = document.getElementById(`coi${id}`);
          co.className  = 'co show';
          coi.innerHTML = cell.output;
        }
      }
      if (document.querySelectorAll('.cell').length === 0) addCell('');
      setStatus(`Loaded ${data.cells.length} cell${data.cells.length === 1 ? '' : 's'} ✓`);
      setTimeout(() => setStatus(''), 2500);
    } catch(err) {
      setStatus('Load failed: ' + err.message);
    }
  };
  input.click();
}

// Ctrl+S → save
document.addEventListener('keydown', e => {
  if ((e.ctrlKey || e.metaKey) && e.key === 's') { e.preventDefault(); saveNotebook(); }
});

// ── init ──────────────────────────────────────────────────────────────────────
createCell(`# Variables defined with let persist across cells — try running both:\nlet n = python^(6 * 7)_python`);
createCell(`html^(<p>6 × 7 = $n</p>)_html`);
addCell('');
</script>
</body>
</html>"#;
