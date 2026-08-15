#!/usr/bin/env python3
"""Backend shim for sql^(...)_sql blocks.

Executes SQL statements against an in-memory SQLite database using
Python's built-in sqlite3 module and returns the result as a string.

Each persistent env (env_id) maintains its own database connection so
that tables created in one sql^ block are visible to later blocks with
the same env_id.
"""
import base64
import os
import sys
import json
import sqlite3
import tempfile
import traceback
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parent))
from o_shim_common import (
    StatePinRequired,
    backend_runtime_binding_sha256,
    command_loop,
    make_checkpoint,
    state_capabilities,
    validate_checkpoint,
    write_wire_message,
)
from o_shim_common import float_to_oval, int_to_oval


def send_ok(value):
    write_wire_message({"status": "ok", "value": value})


def send_err(message):
    write_wire_message({"status": "err", "message": message})


def sqlite_value_to_oval(value):
    if value is None:
        return {"t": "null"}
    if isinstance(value, bool):
        return {"t": "bool", "v": value}
    if isinstance(value, int):
        return int_to_oval(value)
    if isinstance(value, float):
        return float_to_oval(value)
    if isinstance(value, (bytes, bytearray, memoryview)):
        return {
            "t": "bytes",
            "v": {
                "bytes": list(bytes(value)),
                "media_type": "application/octet-stream",
            },
        }
    return {"t": "str", "v": str(value)}


# Map from env_id to persistent sqlite3 connection.
_connections = {}
_checkpoint_safe = True
SQL_PYTHON_CODEC_V1 = "ostadix.sqlite-python-main/v1"


def checkpoint_profile_accepts(code):
    lower = code.lower()
    pinning_tokens = (
        "attach", "detach", "begin", "commit", "rollback", "savepoint",
        "release", "pragma", " temp ", "temporary", "load_extension",
        ".load", ".open", ".restore", ".backup", "create virtual table",
        "last_insert_rowid", "changes(", "total_changes(", "random(",
        "randomblob(",
    )
    return (
        not any(token in lower for token in pinning_tokens)
        and not lower.lstrip().startswith(".")
        and "\n." not in lower
    )


def get_conn(env_id):
    if env_id not in _connections:
        _connections[env_id] = sqlite3.connect(":memory:")
    return _connections[env_id]


def handle_exec(cmd):
    global _checkpoint_safe
    code = cmd.get("code", "").strip()
    env_id = cmd.get("env_id", 0)

    if not code:
        send_ok({"t": "null"})
        return

    try:
        if not checkpoint_profile_accepts(code):
            _checkpoint_safe = False
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
            headers = [d[0] for d in description]
            if len(headers) == 1 and len(rows) == 1:
                send_ok(sqlite_value_to_oval(rows[0][0]))
            else:
                send_ok({
                    "t": "list",
                    "v": [
                        {
                            "t": "map",
                            "v": {
                                str(header): sqlite_value_to_oval(value)
                                for header, value in zip(headers, row)
                            },
                        }
                        for row in rows
                    ],
                })
        elif description:
            send_ok({"t": "list", "v": []})
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
    global _checkpoint_safe
    for conn in _connections.values():
        try:
            conn.close()
        except Exception:
            pass
    _connections.clear()
    _checkpoint_safe = True
    send_ok({"t": "null"})


def _serialize_connection(conn):
    if hasattr(conn, "serialize"):
        return conn.serialize()
    descriptor, path = tempfile.mkstemp(prefix="ostadix-sql-checkpoint-", suffix=".sqlite3")
    os.close(descriptor)
    try:
        disk = sqlite3.connect(path)
        try:
            conn.backup(disk)
        finally:
            disk.close()
        with open(path, "rb") as handle:
            return handle.read()
    finally:
        try:
            os.unlink(path)
        except FileNotFoundError:
            pass


def _deserialize_connection(database):
    conn = sqlite3.connect(":memory:")
    try:
        if hasattr(conn, "deserialize"):
            conn.deserialize(database)
        else:
            descriptor, path = tempfile.mkstemp(
                prefix="ostadix-sql-restore-", suffix=".sqlite3"
            )
            try:
                with os.fdopen(descriptor, "wb") as handle:
                    handle.write(database)
                disk = sqlite3.connect(path)
                try:
                    disk.backup(conn)
                finally:
                    disk.close()
            finally:
                try:
                    os.unlink(path)
                except FileNotFoundError:
                    pass
        integrity = conn.execute("PRAGMA integrity_check").fetchone()
        if integrity != ("ok",):
            raise ValueError(f"SQLite checkpoint failed integrity_check: {integrity!r}")
        return conn
    except Exception:
        conn.close()
        raise


def handle_state_capabilities():
    return state_capabilities(
        "sql", "semantic_snapshot", SQL_PYTHON_CODEC_V1, True
    )


def handle_checkpoint(max_bytes):
    if not _checkpoint_safe:
        raise StatePinRequired(
            "$sql.connection",
            "SQL history used transaction-, attachment-, TEMP-, PRAGMA-, extension-, "
            "or connection-local state outside the constrained database codec",
        )
    snapshots = []
    for env_id in sorted(_connections):
        conn = _connections[env_id]
        if conn.in_transaction:
            raise StatePinRequired(
                f"$sql.connections[{env_id}]",
                "SQLite connection has an open transaction",
            )
        snapshots.append({
            "env_id": str(env_id),
            "database_b64": base64.b64encode(_serialize_connection(conn)).decode("ascii"),
        })
    return make_checkpoint(
        "sql",
        "semantic_snapshot",
        SQL_PYTHON_CODEC_V1,
        {
            "profile": "autocommit-databases-only",
            "sqlite_version": sqlite3.sqlite_version,
            "connections": snapshots,
        },
    )


def handle_restore(checkpoint):
    global _checkpoint_safe
    validate_checkpoint(checkpoint)
    if _connections:
        raise ValueError("state.restore-conflict: SQL actor already owns open connections")
    if (
        checkpoint["backend"] != "sql"
        or checkpoint["tier"] != "semantic_snapshot"
        or checkpoint["codec"] != SQL_PYTHON_CODEC_V1
        or checkpoint["runtime_binding_sha256"] != backend_runtime_binding_sha256()
        or checkpoint.get("external_resources", [])
    ):
        raise ValueError("SQL checkpoint is incompatible with this shim")
    payload = checkpoint["payload"]
    if (
        not isinstance(payload, dict)
        or payload.get("profile") != "autocommit-databases-only"
        or payload.get("sqlite_version") != sqlite3.sqlite_version
        or not isinstance(payload.get("connections"), list)
    ):
        raise ValueError("SQL checkpoint has an incompatible SQLite profile")
    replacement = {}
    try:
        for entry in payload["connections"]:
            if not isinstance(entry, dict) or set(entry) != {"env_id", "database_b64"}:
                raise ValueError("SQL checkpoint connection entry is malformed")
            env_id = int(entry["env_id"])
            if env_id in replacement:
                raise ValueError(f"SQL checkpoint repeats environment {env_id}")
            database = base64.b64decode(entry["database_b64"], validate=True)
            replacement[env_id] = _deserialize_connection(database)
    except Exception:
        for conn in replacement.values():
            conn.close()
        raise
    _connections.update(replacement)
    _checkpoint_safe = True


command_loop(
    handle_exec,
    handle_cleanup=handle_cleanup,
    handle_state_capabilities=handle_state_capabilities,
    handle_checkpoint=handle_checkpoint,
    handle_restore=handle_restore,
    state_backend="sql",
)
