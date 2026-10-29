#!/usr/bin/env python3
"""Run the optional bridge against an explicitly pinned local attention daemon.

This acceptance harness is deliberately separate from the ordinary Agentd test
suite. It starts only copied binaries, with private XDG paths and a controlled
agentd watch stream. It never uses a user service or an installed daemon.
"""

from __future__ import annotations

import argparse
import fcntl
import hashlib
import json
import os
import pathlib
import platform
import select
import shutil
import signal
import socket
import stat
import subprocess
import sys
import tempfile
import time
from typing import Any


REPO_ROOT = pathlib.Path(__file__).resolve().parents[2]
SCHEMA = "agentd-attention-pair/v1"
HEX64 = set("0123456789abcdef")
MAX_FRAME_BYTES = 65_536
WAIT_SECONDS = 20.0


class PairFailure(RuntimeError):
    pass


def require(condition: bool, message: str) -> None:
    if not condition:
        raise PairFailure(message)


def sha256_file(path: pathlib.Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        while chunk := stream.read(1024 * 1024):
            digest.update(chunk)
    return digest.hexdigest()


def validate_digest(value: Any, length: int, label: str) -> str:
    require(isinstance(value, str) and len(value) == length, f"invalid {label}")
    require(set(value) <= HEX64, f"invalid {label}")
    return value


def validate_component(
    name: str, value: Any, input_root: pathlib.Path
) -> tuple[pathlib.Path, dict[str, Any]]:
    require(isinstance(value, dict), f"missing {name} component identity")
    source = value.get("source")
    build = value.get("build")
    binary = value.get("binary")
    require(isinstance(source, dict), f"missing {name} source identity")
    require(isinstance(build, dict), f"missing {name} build identity")
    require(isinstance(binary, dict), f"missing {name} binary identity")
    validate_digest(source.get("commit"), 40, f"{name} source commit")
    validate_digest(source.get("tree"), 40, f"{name} source tree")
    for field in ("target", "profile", "rustc", "cargo"):
        require(
            isinstance(build.get(field), str) and build[field].strip(),
            f"missing {name} build {field}",
        )
    require(
        isinstance(build.get("command"), list)
        and build["command"]
        and all(isinstance(part, str) and part for part in build["command"]),
        f"missing {name} build command",
    )
    require(
        isinstance(build.get("features"), list)
        and all(isinstance(feature, str) for feature in build["features"]),
        f"missing {name} build features",
    )
    raw_path = binary.get("path")
    require(isinstance(raw_path, str) and pathlib.Path(raw_path).is_absolute(),
            f"{name} binary path must be absolute")
    path = pathlib.Path(raw_path)
    require(not path.is_symlink(), f"{name} binary path must not be a symlink")
    resolved = path.resolve(strict=True)
    require(
        os.path.commonpath((str(input_root), str(resolved))) == str(input_root),
        f"{name} binary must be staged under the private input directory",
    )
    mode = resolved.stat().st_mode
    require(stat.S_ISREG(mode) and os.access(resolved, os.X_OK),
            f"{name} binary is not an executable regular file")
    expected = validate_digest(binary.get("sha256"), 64, f"{name} binary SHA-256")
    actual = sha256_file(resolved)
    require(actual == expected, f"{name} binary hash mismatch")
    return resolved, {
        "source": source,
        "build": build,
        "binary": {"stagedPath": str(resolved), "sha256": actual},
    }


def read_manifest(path: pathlib.Path, input_root: pathlib.Path) -> tuple[dict, dict]:
    require(path.is_absolute(), "manifest path must be absolute")
    require(not path.is_symlink(), "manifest path must not be a symlink")
    resolved = path.resolve(strict=True)
    require(
        os.path.commonpath((str(input_root), str(resolved))) == str(input_root),
        "manifest must be staged under the private input directory",
    )
    manifest = json.loads(resolved.read_text(encoding="utf-8"))
    require(isinstance(manifest, dict) and manifest.get("schema") == SCHEMA,
            f"manifest schema must be {SCHEMA}")
    agentd_path, agentd_record = validate_component(
        "agentd", manifest.get("agentd"), input_root
    )
    attentiond_path, attentiond_record = validate_component(
        "attentiond", manifest.get("attentiond"), input_root
    )
    head = subprocess.run(
        ["git", "-C", str(REPO_ROOT), "rev-parse", "HEAD", "HEAD^{tree}"],
        check=True,
        capture_output=True,
        text=True,
    ).stdout.splitlines()
    require(len(head) == 2, "could not read current Agentd commit and tree")
    require(agentd_record["source"]["commit"] == head[0],
            "manifest Agentd commit does not match this checkout")
    require(agentd_record["source"]["tree"] == head[1],
            "manifest Agentd tree does not match this checkout")
    dirty = subprocess.run(
        ["git", "-C", str(REPO_ROOT), "status", "--porcelain"],
        check=True,
        capture_output=True,
        text=True,
    ).stdout
    require(not dirty, "Agentd checkout must be clean at the recorded commit")
    return manifest, {
        "agentd": (agentd_path, agentd_record),
        "attentiond": (attentiond_path, attentiond_record),
        "manifestSha256": sha256_file(resolved),
    }


def ensure_private_directory(path: pathlib.Path, label: str) -> None:
    require(path.is_absolute(), f"{label} must be absolute")
    require(not path.is_symlink(), f"{label} must not be a symlink")
    info = path.stat()
    require(stat.S_ISDIR(info.st_mode), f"{label} must be a directory")
    require(info.st_uid == os.getuid(), f"{label} must be owned by this user")
    require(info.st_mode & 0o022 == 0, f"{label} must not be group/world writable")


def write_json_atomic(path: pathlib.Path, value: dict) -> None:
    temporary = path.with_name(path.name + ".tmp")
    temporary.write_text(json.dumps(value, sort_keys=True) + "\n", encoding="utf-8")
    os.chmod(temporary, 0o600)
    os.replace(temporary, path)


def touch(path: pathlib.Path) -> None:
    path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    path.touch(mode=0o600, exist_ok=True)


def write_agentd_stub(path: pathlib.Path) -> None:
    source = f"""#!{sys.executable}
import fcntl
import os
import pathlib
import sys
import time

if sys.argv[1:] != ["watch", "--json"]:
    raise SystemExit(64)
root = pathlib.Path(os.environ["AGENTD_PAIR_FEED_DIR"])
counter = root / "counter"
lock = root / "counter.lock"
with lock.open("a+") as handle:
    fcntl.flock(handle, fcntl.LOCK_EX)
    handle.seek(0)
    raw = handle.read().strip()
    number = int(raw or "0") + 1
    handle.seek(0)
    handle.truncate()
    handle.write(str(number))
    handle.flush()
    fcntl.flock(handle, fcntl.LOCK_UN)
stream = root / f"stream-{{number}}"
stream.mkdir(mode=0o700, parents=True, exist_ok=True)
touch = stream / "started"
touch.touch(mode=0o600, exist_ok=True)
current = root / "current.json"
deadline = time.monotonic() + 20.0
while not current.exists() and time.monotonic() < deadline:
    time.sleep(0.01)
if not current.exists():
    raise SystemExit("fixture has no initial snapshot")
sys.stdout.write(current.read_text(encoding="utf-8").rstrip("\\n") + "\\n")
sys.stdout.flush()
(stream / "sent-0").touch(mode=0o600, exist_ok=True)
sequence = 1
while True:
    if (stream / "stop").exists():
        break
    item = stream / f"{{sequence}}.json"
    if item.exists():
        sys.stdout.write(item.read_text(encoding="utf-8").rstrip("\\n") + "\\n")
        sys.stdout.flush()
        (stream / f"sent-{{sequence}}").touch(mode=0o600, exist_ok=True)
        sequence += 1
    else:
        time.sleep(0.01)
"""
    path.write_text(source, encoding="utf-8")
    os.chmod(path, 0o700)


def snapshot(revision: int, instance: str, scan: str, state: str,
             observed_at: int | None) -> dict:
    agent = {
        "id": {"pid": 48211, "startTimeTicks": 9127734},
        "harness": "codex",
        "activity": {"state": state, "observedAtUnixMs": observed_at},
        "name": "Pair target",
        "tmux": {"session": "pair-target"},
    }
    return {
        "type": "snapshot",
        "schema": "agentd.snapshot.v1",
        "instanceId": instance,
        "revision": revision,
        "scan": {"state": scan},
        "agents": [agent],
    }


class FrameConnection:
    def __init__(self, stream: socket.socket):
        self.stream = stream
        self.buffer = bytearray()
        self.pending: list[dict] = []
        self.next_ref = 1

    @classmethod
    def connect(cls, path: pathlib.Path, timeout: float = WAIT_SECONDS):
        deadline = time.monotonic() + timeout
        last_error: Exception | None = None
        while time.monotonic() < deadline:
            stream = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            stream.setblocking(False)
            try:
                stream.connect(str(path))
                return cls(stream)
            except BlockingIOError:
                _, writable, _ = select.select([], [stream], [], 0.05)
                if writable:
                    error = stream.getsockopt(socket.SOL_SOCKET, socket.SO_ERROR)
                    if error == 0:
                        return cls(stream)
                    last_error = OSError(error, os.strerror(error))
            except OSError as error:
                last_error = error
            stream.close()
            time.sleep(0.02)
        raise PairFailure(f"could not connect to private attention socket: {last_error}")

    def close(self) -> None:
        self.stream.close()

    def send(self, frame: dict) -> None:
        data = json.dumps(frame, separators=(",", ":")).encode("utf-8") + b"\n"
        require(len(data) <= MAX_FRAME_BYTES, "outbound fixture frame exceeds protocol limit")
        offset = 0
        deadline = time.monotonic() + WAIT_SECONDS
        while offset < len(data):
            try:
                sent = self.stream.send(data[offset:])
                require(sent > 0, "attention socket closed while sending a frame")
                offset += sent
            except BlockingIOError:
                remaining = deadline - time.monotonic()
                require(remaining > 0, "timed out sending an attention frame")
                _, writable, _ = select.select([], [self.stream], [], remaining)
                require(writable, "timed out sending an attention frame")

    def next_frame(self, timeout: float = WAIT_SECONDS) -> dict:
        if self.pending:
            return self.pending.pop(0)
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            newline = self.buffer.find(b"\n")
            if newline < 0:
                require(len(self.buffer) <= MAX_FRAME_BYTES,
                        "attention daemon frame exceeds the protocol limit")
            else:
                require(newline + 1 <= MAX_FRAME_BYTES,
                        "attention daemon frame exceeds the protocol limit")
            if newline >= 0:
                line = bytes(self.buffer[: newline + 1])
                del self.buffer[: newline + 1]
                value = json.loads(line)
                require(isinstance(value, dict), "daemon frame is not a JSON object")
                require(value.get("v") == 1 and isinstance(value.get("type"), str),
                        "daemon frame is missing the v1 protocol header")
                return value
            remaining = max(0.0, deadline - time.monotonic())
            readable, _, _ = select.select([self.stream], [], [], remaining)
            if not readable:
                break
            chunk = self.stream.recv(65_537)
            if not chunk:
                raise PairFailure("attention daemon disconnected")
            self.buffer.extend(chunk)
        raise TimeoutError("timed out waiting for an attention frame")

    def request(self, frame: dict) -> dict:
        reference = f"pair-{self.next_ref}"
        self.next_ref += 1
        outbound = dict(frame)
        outbound["ref"] = reference
        self.send(outbound)
        deferred: list[dict] = []
        try:
            while True:
                response = self.next_frame()
                if response.get("ref") == reference and response.get("type") in ("ok", "error"):
                    require(response.get("type") == "ok",
                            f"attention daemon rejected a fixture request: {response}")
                    require(response.get("v") == 1, "acknowledgment did not use protocol version 1")
                    return response
                deferred.append(response)
                if len(deferred) > 32:
                    raise PairFailure("too many pushed frames while awaiting an acknowledgment")
        finally:
            if deferred:
                self.pending = deferred + self.pending

    def subscribe_snapshot(self) -> list[dict]:
        self.request({"v": 1, "type": "subscribe", "sender_ids": []})
        deferred: list[dict] = []
        try:
            while True:
                frame = self.next_frame()
                if frame.get("type") == "snapshot":
                    messages = frame.get("messages", [])
                    require(isinstance(messages, list), "subscription snapshot messages must be a list")
                    return messages
                deferred.append(frame)
        finally:
            if deferred:
                self.pending = deferred + self.pending


def start_child(
    name: str, command: list[str], environment: dict[str, str], log_path: pathlib.Path,
    children: list[dict],
) -> subprocess.Popen:
    log = log_path.open("ab", buffering=0)
    os.chmod(log_path, 0o600)
    process = subprocess.Popen(
        command,
        stdin=subprocess.DEVNULL,
        stdout=log,
        stderr=log,
        env=environment,
        start_new_session=True,
        close_fds=True,
    )
    children.append({"name": name, "process": process, "log": log})
    return process


def stop_child(entry: dict, timeout: float = 5.0) -> int | None:
    process = entry["process"]
    if process.poll() is None:
        try:
            os.killpg(process.pid, signal.SIGTERM)
        except ProcessLookupError:
            pass
        try:
            return process.wait(timeout=timeout)
        except subprocess.TimeoutExpired:
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            return process.wait(timeout=timeout)
    return process.returncode


def wait_process_group_stopped(process_group: int, timeout: float = 5.0) -> bool:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            os.killpg(process_group, 0)
        except ProcessLookupError:
            return True
        time.sleep(0.02)
    try:
        os.killpg(process_group, 0)
    except ProcessLookupError:
        return True
    return False


def wait_socket(path: pathlib.Path, process: subprocess.Popen) -> None:
    deadline = time.monotonic() + WAIT_SECONDS
    while time.monotonic() < deadline:
        status = process.poll()
        require(status is None, f"attention daemon exited before opening its socket: {status}")
        try:
            info = path.stat()
            if stat.S_ISSOCK(info.st_mode):
                require(info.st_uid == os.getuid(), "private attention socket owner mismatch")
                directory = path.parent.stat()
                require(directory.st_uid == os.getuid(), "attention socket directory owner mismatch")
                require(directory.st_mode & 0o777 == 0o700,
                        "attention socket directory must be private to its owner")
                return
        except FileNotFoundError:
            pass
        time.sleep(0.02)
    raise PairFailure("private attention daemon socket did not become ready")


def wait_path(path: pathlib.Path, label: str, timeout: float = WAIT_SECONDS) -> None:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if path.exists():
            return
        time.sleep(0.01)
    raise PairFailure(f"timed out waiting for {label}")


def snapshot_entries(connection: FrameConnection) -> list[dict]:
    return connection.subscribe_snapshot()


def find_message(entries: list[dict], sender: str, message_id: str) -> list[dict]:
    return [
        entry.get("message", {})
        for entry in entries
        if entry.get("message", {}).get("sender_id") == sender
        and entry.get("message", {}).get("message_id") == message_id
    ]


def expect_delivery(
    connection: FrameConnection, sender: str, message_id: str,
    timeout: float = WAIT_SECONDS,
) -> dict:
    frame = connection.next_frame(timeout)
    require(frame.get("type") == "delivery", f"expected delivery, received {frame}")
    message = frame.get("message", {})
    require(message.get("sender_id") == sender, f"unexpected delivery sender: {message}")
    require(message.get("message_id") == message_id, f"unexpected delivery id: {message}")
    return message


def expect_lifecycle(
    connection: FrameConnection, sender: str, message_id: str, state: str
) -> dict:
    frame = connection.next_frame()
    require(frame.get("type") == "lifecycle", f"expected lifecycle, received {frame}")
    require(frame.get("sender_id") == sender, f"unexpected lifecycle sender: {frame}")
    require(frame.get("message_id") == message_id, f"unexpected lifecycle id: {frame}")
    require(frame.get("state") == state, f"unexpected lifecycle state: {frame}")
    return frame


def expect_no_frame(connection: FrameConnection, seconds: float, label: str) -> None:
    try:
        frame = connection.next_frame(seconds)
    except TimeoutError:
        return
    raise PairFailure(f"unexpected frame while checking {label}: {frame}")


def create_message(sender: str, message_id: str) -> dict:
    return {
        "v": 1,
        "type": "message",
        "sender_id": sender,
        "message_id": message_id,
        "created_at": 1,
        "title": "foreign fixture",
    }


def feed_current(feed: pathlib.Path, value: dict) -> None:
    write_json_atomic(feed / "current.json", value)


def emit_snapshot(feed: pathlib.Path, stream_number: int, sequence: int, value: dict) -> None:
    stream = feed / f"stream-{stream_number}"
    path = stream / f"{sequence}.json"
    write_json_atomic(path, value)
    wait_path(stream / f"sent-{sequence}", f"snapshot {stream_number}/{sequence}")


def wait_stream(feed: pathlib.Path, stream_number: int) -> None:
    stream = feed / f"stream-{stream_number}"
    wait_path(stream / "started", f"Agentd watch stream {stream_number}")
    wait_path(stream / "sent-0", f"Agentd initial snapshot {stream_number}")


def stop_stream(feed: pathlib.Path, stream_number: int) -> None:
    touch(feed / f"stream-{stream_number}" / "stop")


def normalized_machine() -> str:
    machine = os.uname().nodename.lower()
    if machine.endswith("."):
        machine = machine[:-1]
    require(machine and not any(ch.isspace() or ch in "/:" or ord(ch) < 32 for ch in machine),
            "runner node name cannot be represented by the bridge sender id")
    return machine


def check_persistence(entries: list[dict], local_sender: str, local_id: str,
                      foreign_sender: str, foreign_id: str) -> None:
    require(len(find_message(entries, local_sender, local_id)) == 1,
            "expected exactly one persisted local message")
    require(len(find_message(entries, foreign_sender, foreign_id)) == 1,
            "foreign-host message was missing or duplicated")
    require(len(entries) == 2, f"unexpected open messages after reconciliation: {entries}")


def run_pair(args: argparse.Namespace, identities: dict) -> dict:
    require(sys.platform.startswith("linux"), "the pair fixture requires Linux")
    require(len(os.fsencode(args.scratch_parent / "ap-x" / "r" / "attention" / "attention.sock")) < 104,
            "scratch path is too long for a Unix socket; provide a shorter private parent")
    scratch = pathlib.Path(tempfile.mkdtemp(prefix="ap-", dir=args.scratch_parent))
    children: list[dict] = []
    report: dict[str, Any] = {
        "schema": SCHEMA,
        "status": "failed",
        "identities": {
            name: record for name, (_, record) in identities.items()
        },
        "runtime": {"private": True, "servicesUsed": False, "packageInstalled": False},
        "runtimeCompatibility": {
            "platform": sys.platform,
            "architecture": platform.machine(),
            "kernelRelease": platform.release(),
            "libc": platform.libc_ver(),
            "python": sys.version.splitlines()[0],
        },
        "invocation": sys.argv,
        "commands": [],
        "checks": [],
        "cleanup": [],
        "limits": [
            "Agentd snapshots are controlled published-schema fixtures, not produced by a running Agentd daemon.",
            "This harness does not prove a fresh-login or reboot path.",
            "This harness does not prove whole-host isolation or unobservable descendant state.",
        ],
    }

    observer: FrameConnection | None = None
    def launch(name: str, command: list[str]) -> subprocess.Popen:
        log_path = args.evidence_dir / f"{name}-{len(children) + 1}.log"
        process = start_child(name, command, environment, log_path, children)
        report["commands"].append({"name": name, "argv": command, "pid": process.pid,
                                   "log": str(log_path)})
        return process

    try:
        os.chmod(scratch, 0o700)
        runtime = scratch / "r"
        state = scratch / "s"
        home = scratch / "h"
        config = scratch / "c"
        data = scratch / "d"
        tmp = scratch / "t"
        bin_dir = scratch / "b"
        feed = scratch / "f"
        for directory in (runtime, state, home, config, data, tmp, bin_dir, feed):
            directory.mkdir(mode=0o700)
        report["runtime"].update({
            "runtimeDirectory": str(runtime),
            "stateDirectory": str(state),
            "scratchDirectory": str(scratch),
        })

        copied: dict[str, pathlib.Path] = {}
        for name, (source, record) in identities.items():
            destination = bin_dir / ("agentd-attention" if name == "agentd" else "attentiond")
            shutil.copyfile(source, destination)
            os.chmod(destination, 0o700)
            actual = sha256_file(destination)
            require(actual == record["binary"]["sha256"], f"copied {name} binary hash mismatch")
            copied[name] = destination

        stub = bin_dir / "agentd"
        write_agentd_stub(stub)
        environment = {
            "HOME": str(home),
            "TMPDIR": str(tmp),
            "XDG_RUNTIME_DIR": str(runtime),
            "XDG_STATE_HOME": str(state),
            "XDG_CONFIG_HOME": str(config),
            "XDG_DATA_HOME": str(data),
            "AGENTD_PAIR_FEED_DIR": str(feed),
            "PATH": f"{bin_dir}:/usr/bin:/bin",
            "LC_ALL": "C",
        }

        machine = normalized_machine()
        local_a = f"agentd://{machine}/0123456789abcdef0123456789abcdef/48211:9127734"
        local_b = f"agentd://{machine}/fedcba9876543210fedcba9876543210/48211:9127734"
        foreign_sender = "agentd://foreign-host/foreign-instance/9:10"
        foreign_id = "foreign-open"
        socket_path = runtime / "attention" / "attention.sock"
        daemon = launch("attentiond", [str(copied["attentiond"])])
        wait_socket(socket_path, daemon)
        observer = FrameConnection.connect(socket_path)
        initial = snapshot_entries(observer)
        require(initial == [], f"private store was not empty before the test: {initial}")

        poster = FrameConnection.connect(socket_path)
        foreign_reply = poster.request(create_message(foreign_sender, foreign_id))
        poster.close()
        require(foreign_reply.get("type") == "ok" and foreign_reply.get("duplicate") is not True,
                f"first post did not receive a successful acknowledgment: {foreign_reply}")
        foreign = expect_delivery(observer, foreign_sender, foreign_id)
        require(foreign.get("as") == "knock", "unsubscribed foreign message was not a knock")
        report["checks"].append("foreign message is retained as a knock")

        feed_current(feed, snapshot(1, "0123456789abcdef0123456789abcdef", "degraded",
                                    "needs_attention", 100))
        bridge = launch("agentd-attention", [str(copied["agentd"])])
        wait_stream(feed, 1)
        expect_no_frame(observer, 0.5, "waiting for a complete Agentd scan")
        entries = snapshot_entries(observer)
        require(len(find_message(entries, foreign_sender, foreign_id)) == 1,
                "bridge posted before Agentd supplied a complete scan")
        report["checks"].append("incomplete first scan causes no post")

        emit_snapshot(feed, 1, 1, snapshot(
            2, "0123456789abcdef0123456789abcdef", "complete", "needs_attention", 100
        ))
        first = expect_delivery(observer, local_a, "needs_attention@100")
        require(first.get("created_at") == 100 and first.get("title") == f"Pair target on {machine}",
                f"initial message identity/content mismatch: {first}")
        require("body" not in first, "bridge message unexpectedly contains a body")
        require(first.get("open") == {
            "argv": ["tmux", "attach-session", "-t", "=pair-target"], "terminal": True
        }, f"tmux open argv mismatch: {first}")
        require(first.get("as") == "knock", "empty subscription did not receive the local post as a knock")
        report["checks"].append("AB-1/2 identity, title, timestamp, tmux argv, and body omission")

        duplicate_poster = FrameConnection.connect(socket_path)
        duplicate_reply = duplicate_poster.request(create_message(local_a, "needs_attention@100"))
        duplicate_poster.close()
        require(duplicate_reply.get("duplicate") is True,
                f"duplicate post was not acknowledged as a duplicate: {duplicate_reply}")
        expect_no_frame(observer, 0.2, "duplicate post must not be delivered again")
        report["checks"].append("AD-6 duplicate post is acknowledged without redelivery")

        emit_snapshot(feed, 1, 2, snapshot(
            3, "0123456789abcdef0123456789abcdef", "degraded", "active", 101
        ))
        expect_no_frame(observer, 0.3, "degraded scan must not clear")
        emit_snapshot(feed, 1, 3, snapshot(
            4, "0123456789abcdef0123456789abcdef", "complete", "unknown", None
        ))
        expect_no_frame(observer, 0.3, "unknown activity must not clear")
        require(len(find_message(snapshot_entries(observer), local_a, "needs_attention@100")) == 1,
                "incomplete or unknown scan changed an open local message")
        report["checks"].append("AB-3 incomplete and unknown scans preserve open messages")

        emit_snapshot(feed, 1, 4, snapshot(
            5, "0123456789abcdef0123456789abcdef", "complete", "needs_attention", 200
        ))
        second = expect_delivery(observer, local_a, "needs_attention@200")
        require("body" not in second, "superseding message unexpectedly contains a body")
        expect_lifecycle(observer, local_a, "needs_attention@100", "superseded")
        report["checks"].append("AB-4 posts a new episode before superseding its prior episode")

        feed_current(feed, snapshot(
            5, "0123456789abcdef0123456789abcdef", "complete", "needs_attention", 200
        ))
        stop_stream(feed, 1)
        wait_stream(feed, 2)
        expect_no_frame(observer, 0.7, "Agentd watch reconnect must not redeliver the same id")
        report["checks"].append("AB-5 Agentd watch loss/reconnect preserves the same open episode")

        feed_current(feed, snapshot(
            1, "fedcba9876543210fedcba9876543210", "complete", "needs_attention", 300
        ))
        emit_snapshot(feed, 2, 1, snapshot(
            1, "fedcba9876543210fedcba9876543210", "complete", "needs_attention", 300
        ))
        new_instance = expect_delivery(observer, local_b, "needs_attention@300")
        require("body" not in new_instance, "instance-change message unexpectedly contains a body")
        require(new_instance.get("as") == "knock", "empty subscription did not receive the new local post as a knock")
        expect_lifecycle(observer, local_a, "needs_attention@200", "cleared")
        entries = snapshot_entries(observer)
        require(len(find_message(entries, local_b, "needs_attention@300")) == 1,
                "new Agentd instance message was not left open")
        require(len(find_message(entries, foreign_sender, foreign_id)) == 1,
                "instance reconciliation changed a foreign sender message")
        report["checks"].append("AB-5 Agentd instance change posts the new identity and clears the old one")

        observer.close()
        observer = None
        stop_child(children[0], timeout=8.0)
        stop_stream(feed, 2)
        wait_stream(feed, 3)
        require(bridge.poll() is None, "bridge exited while retrying a missing local daemon")
        feed_current(feed, snapshot(
            1, "fedcba9876543210fedcba9876543210", "complete", "needs_attention", 300
        ))
        daemon = launch("attentiond", [str(copied["attentiond"])])
        wait_socket(socket_path, daemon)
        observer = FrameConnection.connect(socket_path)
        entries = snapshot_entries(observer)
        check_persistence(entries, local_b, "needs_attention@300", foreign_sender, foreign_id)
        report["checks"].append("AD-11 daemon restart preserves the open local and foreign messages")

        reconnect_started = time.monotonic()
        emit_snapshot(feed, 3, 1, snapshot(
            2, "fedcba9876543210fedcba9876543210", "complete", "needs_attention", 400
        ))
        fourth = expect_delivery(observer, local_b, "needs_attention@400", timeout=5.8)
        reconnect_seconds = time.monotonic() - reconnect_started
        require(reconnect_seconds <= 5.8,
                f"bridge did not resume local posting within the AB-5 retry bound: {reconnect_seconds:.3f}s")
        report["daemonReconnectSeconds"] = round(reconnect_seconds, 3)
        require("body" not in fourth, "reconnected message unexpectedly contains a body")
        expect_lifecycle(observer, local_b, "needs_attention@300", "superseded")
        report["checks"].append("AB-5 daemon reconnect resumes within the capped retry interval")
        emit_snapshot(feed, 3, 2, snapshot(
            3, "fedcba9876543210fedcba9876543210", "complete", "active", 401
        ))
        expect_lifecycle(observer, local_b, "needs_attention@400", "cleared")
        final_entries = snapshot_entries(observer)
        require(find_message(final_entries, local_b, "needs_attention@300") == [],
                "superseded episode remained open")
        require(find_message(final_entries, local_b, "needs_attention@400") == [],
                "cleared episode remained open")
        require(len(find_message(final_entries, foreign_sender, foreign_id)) == 1,
                "local reconciliation changed a foreign-host message")
        report["checks"].append("AB-3/5 reconnect supersession, clear, and other-host isolation")
        report["status"] = "passed"
    except Exception as error:
        report["status"] = "failed"
        report["error"] = f"{type(error).__name__}: {error}"
    finally:
        if observer is not None:
            observer.close()
        for entry in reversed(children):
            status = stop_child(entry)
            report["cleanup"].append({"name": entry["name"], "pid": entry["process"].pid,
                                      "exitCode": status,
                                      "processGroupStopped": wait_process_group_stopped(
                                          entry["process"].pid
                                      )})
            entry["log"].close()
        report["cleanupOk"] = all(
            item["exitCode"] is not None and item["processGroupStopped"]
            for item in report["cleanup"]
        )
        if report["cleanupOk"]:
            try:
                shutil.rmtree(scratch)
                report["runtime"]["scratchRemoved"] = not scratch.exists()
            except OSError as error:
                report["cleanupOk"] = False
                report["cleanupError"] = str(error)
                report["runtime"]["scratchPreserved"] = str(scratch)
        else:
            report["runtime"]["scratchPreserved"] = str(scratch)
        if not report["cleanupOk"] or not report["runtime"].get("scratchRemoved", False):
            report["status"] = "failed"
            report.setdefault("error", "test-owned processes or scratch did not clean up")
        write_json_atomic(args.evidence_dir / "result.json", report)

    return report


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--manifest", type=pathlib.Path, required=True)
    parser.add_argument("--inputs-dir", type=pathlib.Path, required=True)
    parser.add_argument("--scratch-parent", type=pathlib.Path, required=True)
    parser.add_argument("--evidence-dir", type=pathlib.Path, required=True)
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    require(args.inputs_dir.is_absolute(), "input directory must be absolute")
    require(args.scratch_parent.is_absolute(), "scratch parent must be absolute")
    require(not args.inputs_dir.is_symlink(), "input directory must not be a symlink")
    require(not args.scratch_parent.is_symlink(), "scratch parent must not be a symlink")
    args.inputs_dir = args.inputs_dir.resolve(strict=True)
    args.scratch_parent = args.scratch_parent.resolve(strict=True)
    require(args.evidence_dir.is_absolute(), "evidence directory must be absolute")
    require(not args.evidence_dir.exists(), "evidence directory must not already exist")
    require(not args.evidence_dir.is_symlink(), "evidence directory must not be a symlink")
    ensure_private_directory(args.inputs_dir, "input directory")
    ensure_private_directory(args.scratch_parent, "scratch parent")
    args.evidence_dir.mkdir(mode=0o700, parents=False)
    ensure_private_directory(args.evidence_dir, "evidence directory")
    try:
        _manifest, identities = read_manifest(args.manifest, args.inputs_dir)
        report = run_pair(args, identities)
    except Exception as error:
        report = {
            "schema": SCHEMA,
            "status": "failed",
            "error": f"{type(error).__name__}: {error}",
            "limits": ["No product failure is inferred from a setup or harness failure."],
        }
        if args.manifest.is_file() and not args.manifest.is_symlink():
            report["manifestSha256"] = sha256_file(args.manifest.resolve())
        write_json_atomic(args.evidence_dir / "result.json", report)
    print(json.dumps(report, sort_keys=True))
    return 0 if report.get("status") == "passed" else 1


if __name__ == "__main__":
    raise SystemExit(main())
