
This is the OTHER side of the wire protocol. The Python process that lives as a subprocess, reads commands, executes them, writes results. This is maybe 60 lines of Python — and it's the ONLY Python in the entire system. Every other O component is Racket.

python
import sys, json, io, base64, traceback

def py_to_oval(v):
    if v is None:               return {"t": "null"}
    if isinstance(v, bool):     return {"t": "bool",  "v": v}
    if isinstance(v, int):      return {"t": "int",   "v": v}
    if isinstance(v, float):    return {"t": "float", "v": v}
    if isinstance(v, str):      return {"t": "str",   "v": v}
    if isinstance(v, bytes):    return {"t": "blob",  "v": base64.b64encode(v).decode(), "mime": "application/octet-stream"}
    if isinstance(v, list):     return {"t": "list",  "v": [py_to_oval(i) for i in v]}
    if isinstance(v, dict):     return {"t": "map",   "v": {str(k): py_to_oval(val) for k, val in v.items()}}
    # Fallback: convert to string
    return {"t": "str", "v": str(v)}

def oval_to_py(oval):
    t = oval["t"]
    if t == "null":   return None
    if t == "blob":   return base64.b64decode(oval["v"])
    return oval.get("v")

env = {}
for line in sys.stdin:
    line = line.strip()
    if not line: continue
    req = json.loads(line)
    
    if req["cmd"] == "exec":
        for name, oval in req["bindings"].items():
            env[name] = oval_to_py(oval)
        try:
            captured = io.StringIO()
            old_stdout = sys.stdout
            sys.stdout = captured
            exec(req["code"], env)
            sys.stdout = old_stdout
            result = env.pop("__oval_result__", captured.getvalue() or None)
            sys.stdout = old_stdout
            print(json.dumps({"status": "ok", "value": py_to_oval(result)}))
        except Exception:
            sys.stdout = old_stdout
            print(json.dumps({"status": "err", "message": traceback.format_exc()}))
    
    elif req["cmd"] == "cleanup":
        env.clear()
        print(json.dumps({"status": "ok", "value": {"t": "null"}}))
    
    sys.stdout.flush()
Notice the pattern: every language backend is JUST this shim. The Haskell shim reads JSON, runs GHCi, writes JSON. The LaTeX shim reads JSON, runs pdflatex, encodes the PDF as a blob, writes JSON. Each shim is maybe 50-80 lines in whatever language it's shim-ing. The O orchestration logic never changes regardless of how many backends you add.

