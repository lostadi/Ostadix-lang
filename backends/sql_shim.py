#!/usr/bin/env python3
"""Backend shim for sql^(...)_sql blocks.

Executes SQL statements against an in-memory SQLite database using
Python's built-in sqlite3 module and returns the result as a string.

Each persistent env (env_id) maintains its own database connection so
that tables created in one sql^ block are visible to later blocks with
the same env_id.
"""
import sys
import json
import sqlite3
import traceback


def send_ok(value):
    print(json.dumps({"status": "ok", "value": value}), flush=True)


def send_err(message):
    print(json.dumps({"status": "err", "message": message}), flush=True)


# Map from env_id to persistent sqlite3 connection.
_connections = {}


def get_conn(env_id):
    if env_id not in _connections:
        _connections[env_id] = sqlite3.connect(":memory:")
    return _connections[env_id]


def handle_exec(cmd):
    code = cmd.get("code", "").strip()
    env_id = cmd.get("env_id", 0)

    if not code:
        send_ok({"t": "null"})
        return

    try:
        conn = get_conn(env_id)
        cursor = conn.cursor()

        # Execute all statements. executescript commits implicitly.
        # For SELECT queries we want to fetch results, so we handle them
        # individually.
        statements = [s.strip() for s in code.split(";") if s.strip()]
        rows = []
        description = None

        for stmt in statements:
            cursor.execute(stmt)
            upper = stmt.lstrip().upper()
            if upper.startswith("SELECT") or upper.startswith("WITH") or upper.startswith("PRAGMA"):
                rows = cursor.fetchall()
                description = cursor.description

        conn.commit()

        if description and rows:
            # Format as tab-separated table with header.
            headers = [d[0] for d in description]
            lines = ["\t".join(str(h) for h in headers)]
            for row in rows:
                lines.append("\t".join(str(v) if v is not None else "NULL" for v in row))
            send_ok({"t": "str", "v": "\n".join(lines)})
        elif description:
            # Query returned no rows.
            headers = [d[0] for d in description]
            send_ok({"t": "str", "v": "\t".join(str(h) for h in headers)})
        else:
            # Non-query statement (INSERT, CREATE, etc.).
            affected = cursor.rowcount
            if affected < 0:
                send_ok({"t": "str", "v": "Statement executed successfully"})
            else:
                send_ok({"t": "str", "v": f"{affected} row(s) affected"})

    except Exception:
        send_err(traceback.format_exc())


def handle_cleanup():
    for conn in _connections.values():
        try:
            conn.close()
        except Exception:
            pass
    _connections.clear()
    send_ok({"t": "null"})


for line in sys.stdin:
    try:
        cmd = json.loads(line)
        tag = cmd.get("cmd")

        if tag == "exec":
            handle_exec(cmd)
        elif tag == "cleanup":
            handle_cleanup()
        elif tag == "ping":
            send_ok({"t": "null"})
        else:
            send_err(f"unknown command: {tag!r}")

    except Exception:
        send_err(traceback.format_exc())
