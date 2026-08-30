---
name: agentd
description: Operate Agentd, the local daemon that publishes a truthful roster of running coding agents and their status, from an external agent session on any machine. Use when asked which agents are running, what state they are in, whether Agentd is healthy, or to wire harness hooks into it.
---

# Agentd operator guide

Agentd is a per-user daemon that keeps one truthful roster of the coding-agent
processes (Claude Code, Codex CLI) running under the same user, and publishes it
over a local socket. It tells you that an agent exists and, when hooks are
installed, whether it is working, idle, or waiting on a person. It never reads,
stores, or forwards prompts, transcripts, command lines, or environment values.

You are normally an external operator: a separate agent session asked to read
or act on Agentd for a user. This guide separates what Agentd guarantees
(marked **contract**) from what depends on the installation in front of you
(marked **local**). When the two conflict, the live binary and its README win.

## 1. Discover the installation first

Never assume where Agentd lives or how it runs.

```sh
command -v agentd            # on PATH?
agentd --version             # release in use
agentd 2>&1 | head           # usage line: the live command contract
```

If `agentd` is not on PATH, look in the user's local binary directory, then ask.
Do not build from source, download a build, or copy a binary from another machine
on your own initiative; installs are the user's call (see §4).

**Contract.** The usage line lists every command the release supports. Commands
established across releases:

| Command | What it does |
| --- | --- |
| `agentd list [--json]` | One complete snapshot of the roster, then exit 0. |
| `agentd watch [--json]` | Subscribe; print the current snapshot and every change. |
| `agentd activity --pid <pid> --state <state>` | Attach an activity claim to a present agent. |
| `agentd integrate install\|uninstall <harness>` | Wire or remove the harness hooks (0.2.0 and later). |
| `agentd daemon` | Run the daemon itself; the service manager's job, not yours. |

`--json` prints the wire frame unchanged. Use it for anything you parse.

**Local.** The socket path, the service manager (systemd user unit, launchd
agent, a supervisor, a manual process), the install directory, and which
harness versions are present all vary. Read the README shipped with the
release for this machine before advising on any of them.

## 2. Authority and safety boundaries

- Work as the user who owns the agents. Agentd only sees processes of the user
  running it, and its socket admits only that user (**contract**).
- Reading is free: `list`, `watch`, `--version`, the README.
- Anything that changes state needs the user's explicit instruction, given in
  this conversation: installing or upgrading the daemon, starting or stopping
  the service, `integrate install` or `uninstall`, `activity` claims on someone
  else's session, restarting a running agent so hooks take effect, or editing a
  harness settings file by hand.
- Never start, stop, or restart the daemon or a running agent because a reading
  looks wrong. A degraded scan, an `unknown` status, or a missing socket may be
  a deliberate state or a symptom of something else. Report it.
- Never edit harness configuration files directly when `integrate` exists. The
  installer merges without disturbing other tools' hooks; a hand edit can.

## 3. Read-only inspection first

Start every task with one snapshot:

```sh
agentd list
```

```text
instance=<32 hex> revision=<n> scan=complete agents=<count>
agent pid=<pid> startTimeTicks=<ticks> harness=claude presence=present cwd=<dir> activity=idle
```

For a stream, use `agentd watch`. It prints complete snapshots, never diffs;
reconnecting gives you a fresh complete snapshot. Do not try to replay history:
Agentd keeps none (**contract**).

The JSON frame (`agentd list --json`) has this shape:

```json
{
  "type": "snapshot",
  "reason": "roster_changed",
  "schema": "agentd.snapshot.v1",
  "instanceId": "<32 hex>",
  "revision": 42,
  "observedAtUnixMs": 1700000000000,
  "scan": { "state": "complete", "issues": [] },
  "agents": [
    {
      "id": { "pid": 12345, "startTimeTicks": 987654 },
      "harness": "codex",
      "detectedBy": "proc_comm",
      "presence": { "state": "present", "cause": null },
      "cwd": { "state": "known", "value": "/path/to/project", "cause": null },
      "activity": { "state": "unknown", "source": "none", "observedAtUnixMs": null }
    }
  ]
}
```

Later releases may add optional fields (a display name, terminal location, a
wall-clock start time). Treat unknown fields as additive; do not fail on them.

## 4. Interpreting identity and status

**Identity (contract).** An agent is `{pid, startTimeTicks}`. The same pid with
a different start time is a different agent. Identities mean nothing across a
reboot or across machines. `harness` says which command-name rule matched; it
does not prove who made the binary.

One roster entry is one agent root. A harness process nested under another
process of the same harness collapses into its parent's entry. In-process
subagents, and agents that run inside some other host program rather than as a
`claude`/`codex` process, are not visible. Say so when a user asks why a count
looks low; it is the design, not a fault.

**Presence (contract).**

| presence | Meaning |
| --- | --- |
| `present` | Proven alive in the latest complete scan. |
| `unknown` | Previously proven; the latest scan could not prove or disprove it. `cause` names why. |
| (removed) | Absence is proven by removal from the roster, never by a flag. |

**Activity (contract).** Activity comes only from hooks; Agentd never infers it
from CPU, elapsed time, or output.

| activity | Meaning |
| --- | --- |
| `unknown` | No hook claim for this daemon instance and identity. Not "idle". |
| `active` | The harness reported it started work (a prompt was submitted or a tool is running). |
| `idle` | The harness reported its turn ended. |
| `needs_attention` | The harness reported it is waiting on a person: a permission prompt, a question, an idle notification. |

`activity.observedAtUnixMs` is when the claim was accepted. Report claim age;
do not invent a staleness threshold. `activity.source=none` means no claim.

**Scan (contract).** `scan.state=degraded` means at least one retained identity
could not be resolved, or the process table could not be read. It is not an
error exit; `list` still returns 0. Each issue names a pid (or none), a field,
and a cause (`permission_denied`, `process_raced`, `io_error`,
`proc_unavailable`). A `process_raced` issue for a pid that is not in the roster
is normally a process that started or exited mid-scan; mention it only if it
persists across scans.

**Why a status is `unknown` (local).** The usual reasons, in order:

1. Hooks are not installed for that harness (`integrate install` not run).
2. The agent started before the hooks were installed and the harness loads hook
   configuration at startup. Some harnesses pick up changes live; some need a
   restart with conversation resume. Check the harness's own docs.
3. The harness requires the user to trust new hooks at an interactive start
   (Codex does). Until trusted, the hooks do not run.
4. The agent has simply not emitted an event since the daemon started; a daemon
   restart resets every claim to `unknown` (**contract**).

Never fill these gaps by reading the agent's screen, transcript files, or
prompts. Report `unknown` as `unknown`, with the likely reason.

## 5. Mutations that need explicit authorization

### Activity claims

`agentd activity --pid <pid> --state active|idle|needs_attention` attaches a
claim to a present identity. It exits 0 and prints nothing on success; it
rejects an absent pid, a reused pid, or an unknown state with one stderr line
and exit 1. Use it only when the user asks you to mark a session, or when you
are the harness-side hook. Do not "correct" another session's status.

### Hook integration (0.2.0 and later)

```sh
agentd integrate install <harness>     # e.g. claude, codex
agentd integrate uninstall <harness>
```

**Contract.**
- Install edits only the harness's user-level hook configuration, adds Agentd's
  own entries after any existing ones, and leaves every other entry untouched.
- Install is idempotent: a repeat changes no bytes and reports `unchanged`.
  Prove it when asked: hash the target file, run install again, hash again.
- Uninstall removes only Agentd's own entries (identified by their exact
  command, not by the word "agentd"), leaves the file and other hooks in place,
  and is also idempotent.
- The installer prints one status line naming the target file, the result, and
  what the user must do next (restart running sessions, trust the hooks).
- Agentd never writes a harness's trust state. Where a harness requires trust
  for new hooks, the user grants it in the harness at their next interactive
  start.
- The hook command itself has a short deadline and always exits 0 on
  operational failure, so a stopped daemon can never block or fail the harness.

**Local.** Which harness versions are supported, whether the installer warns on
an unverified version, and whether running sessions need a restart all depend
on the release and the harnesses installed. Read the installer's output and the
README; relay both to the user verbatim where they matter.

Run install or uninstall only on the user's instruction. After install, tell
the user which running sessions will stay `unknown` until restarted, and how to
restart without losing the conversation (the harness's resume command).

### Daemon and service lifecycle

Starting, stopping, enabling, upgrading, or reinstalling the daemon is a
service-manager operation on the user's machine. Do it only when asked, using
the mechanism the README for that release documents, and verify afterwards
with `agentd list`. A restart creates a new `instanceId` and resets every
activity claim to `unknown` (**contract**); say so before doing it.

## 6. Errors and what they mean

| Symptom | Reading | Action |
| --- | --- | --- |
| `agentd` not found | Not installed, or not on PATH for this user. | Check the user's local bin dir; ask. Do not install. |
| Error naming the runtime directory variable | The daemon resolves its socket from the user runtime directory, which is unset in this shell. | Run from a login shell for the user, or as the service manager does. |
| Error naming the socket path | Daemon not running, or running as another user. | Report; check the service state read-only; do not start it unasked. |
| `scan=degraded` | See §4. | Report the issues; do not restart. |
| `unknown_agent` on `activity` | Pid absent, reused, or presence unknown. | Re-list; the identity changed. |
| `request_too_large`, `malformed_request`, `unsupported_version` | A client (yours or another) sent a bad frame. | Fix the client; the daemon is fine. |
| Hook prints `agentd hook: <code>` in harness logs | The hook could not reach the daemon in time. | Harmless to the harness; check the daemon. |

Exit codes (**contract**): `list`/`watch` return 0 after any valid snapshot,
degraded included; usage, socket, protocol, and daemon errors return 1 with one
stderr line naming the operation and cause.

## 7. Privacy and secrets

- Agentd carries no prompt, transcript, tool input, command line, or
  environment value, in any frame, log, or CLI output (**contract**). Keep your
  reporting at the same level: pid, harness, cwd, status, timestamps, names.
- Do not open harness transcript or session files to "fill in" what an agent is
  doing. If the user wants that, it is a request to the harness, not to Agentd.
- A working directory is a path; it may reveal project names. Report it to the
  owning user only.
- Hook configuration files can contain other tools' entries and, for some
  harnesses, trust hashes. Read them only to verify Agentd's own entries, and
  never paste their full contents into a durable record.
- Never pass secrets on a command line. Agentd needs none.

## 8. Agentd on more than one machine

A user may run Agentd on several machines and ask for a reading across them.
The method is the same on every machine: get onto it, then use its local
`agentd` CLI. Do not scan a network for Agentd.

**Contract, current releases.** The daemon listens on one local Unix socket and
opens no IPv4 or IPv6 listener. There is no network port to find. A remote
roster is read by running the CLI on the remote machine.

**The procedure.**

1. The user names the machines. Read only the hosts the user has named in this
   conversation, or a list the user has pointed you to. Do not add a host
   because it appears in a config, a peer list, or a known-hosts file.
2. The user authorizes the access. Use only SSH access the user has already set
   up and explicitly told you to use: their host alias or address, their user
   account, their key or agent. Never create keys, copy credentials, prompt for
   or store passwords, or reuse access you found rather than were given.
3. On the remote machine, run the same read-only sequence as locally:

```sh
ssh <authorized-host> 'command -v agentd && agentd --version && agentd list --json'
```

   Read the remote release's usage line and README as you would locally; the
   remote install may be a different version with different commands.
4. Mutations on a remote machine follow §5 exactly: only on the user's
   instruction, naming that host, and only through that machine's own CLI and
   documented interface.

**Local.** A deployment may add its own way to reach a roster: a documented
network front, a tunnel, an aggregator. If the release README or the deployment
documents one, use it as documented, and treat its remote access, authentication,
encryption, and firewall behavior as that deployment's responsibility, not
Agentd's. Assume no authentication and no encryption until the documentation
says otherwise; a snapshot carries working-directory paths and names. If nothing
is documented, the only path is SSH to the machine and the local CLI.

**Reporting.** Fold results into one report with a machine column. Keep each
machine's identities separate: a pid means nothing across hosts. If a host was
unreachable or refused access, say "not reached" for that host; do not try
another route.

## 9. Building a UI or tool on Agentd

Agentd pushes; consumers subscribe. A display that updates by itself needs no
timer and no polling. Two ways to consume, both **contract**:

**A. Child process.** Run `agentd watch --json` and read its stdout line by
line. Each line is one complete snapshot frame. Simplest; works from any
language; the CLI resolves the socket for you.

**B. The socket directly.** The daemon listens on a Unix stream socket named
`agentd.sock` in the user's runtime directory (the `XDG_RUNTIME_DIR` variable
on Linux; **local**: confirm the resolved path from the CLI's error text or the
README). The protocol is one UTF-8 JSON object per line, LF-terminated, in each
direction. The request set is closed:

```json
{"version":1,"op":"snapshot"}
{"version":1,"op":"subscribe"}
{"version":1,"op":"activity","agent":{"pid":12345,"startTimeTicks":987654},"state":"active"}
{"version":1,"op":"name","agent":{"pid":12345,"startTimeTicks":987654},"name":"Review lane"}
```

- `snapshot`: one complete frame, then the daemon closes the connection.
- `subscribe`: one complete frame immediately (the current state), then a new
  complete frame after every commit that changes roster, scan, or activity
  data. Timestamp-only changes emit nothing. Revisions increase within one
  `instanceId`.
- `activity`: one acknowledgement `{"type":"ack","instanceId":…,"revision":…}`
  or one error frame, then close.
- `name`: set a display name with a string or clear it with `null`; one
  acknowledgement or one error frame, then close.
- Errors are `{"type":"error","code":…,"message":…}` with a closed code set
  (`unsupported_version`, `unknown_operation`, `malformed_request`,
  `request_too_large`, `invalid_activity`, `invalid_name`, `unknown_agent`,
  `name_store_unavailable`); the message never
  echoes your bytes. A request over 65,536 bytes is refused unparsed.

Illustrative subscriber (any language with Unix sockets works the same way):

```python
import json, os, socket
s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
s.connect(os.path.join(os.environ["XDG_RUNTIME_DIR"], "agentd.sock"))
s.sendall(b'{"version":1,"op":"subscribe"}\n')
for line in s.makefile("r"):
    snapshot = json.loads(line)   # complete state; redraw from it
```

**Rules that shape a correct consumer (contract).**

- Every frame is the whole truth. Redraw from the frame; never accumulate.
  Diffing two frames by `{pid, startTimeTicks}` is fine for animations, but the
  frame, not your diff, is the state.
- Slow readers lose nothing that matters. The daemon keeps at most one pending
  frame per subscriber and replaces it with the newest, so a consumer that
  stalls skips intermediate revisions and still receives the current roster.
  There is no replay; do not ask for one.
- On disconnect (daemon restart, socket error), reconnect and subscribe again.
  Expect a new `instanceId`, revision starting over, and every activity back
  at `unknown`. Show that honestly rather than keeping stale status.
- Order is stable: agents are sorted by `(harness, pid, startTimeTicks)` and
  issues by `(pid, field, cause)`; you can rely on it for stable rows.
- `reason` tells you why the frame came (`initial`, `roster_changed`,
  `activity_changed`, `scan_changed`); use it to pick a sound or a flash, not
  to decide what to render.
- Render the four activity states distinctly, including `unknown`, and show
  `scan=degraded` when it happens. A UI that folds `unknown` into idle or
  hides degraded scans defeats the product's one promise.

**Local.** Browsers cannot open Unix sockets; a web UI needs a small local
bridge process that subscribes and forwards frames (WebSocket or
server-sent events). Desktop shells and menu-bar apps can usually open the
socket directly from their runtime. Whether a network front exists is a
deployment matter (§8); Agentd itself serves only the local socket.

## 10. Reporting findings

Lead with the answer, then the evidence.

1. One line: how many agents, how many with real status, daemon healthy or not.
2. The roster, one line per agent, in the daemon's order, translated:
   harness, a human location (cwd basename, terminal or window if the release
   reports it), status in plain words, claim age. Keep pids; they are how the
   user acts on a row.
3. Anything `unknown` or `degraded`, with the most likely local reason from §4,
   labeled as a likely reason, not a fact.
4. What the user must do, if anything: trust hooks, restart a session, ask for
   an install. Do not do these yourself.

Example:

```text
6 agents; 4 reporting status; daemon healthy (revision 1204, scan complete).
claude   ~/project-a   working          12s ago    pid 4021
claude   ~/project-a   needs attention  3m ago     pid 4108
codex    ~/project-b   idle             40s ago    pid 4390
claude   ~             unknown          (started before hooks; restart to report)  pid 3877
...
```

Distinguish the three kinds of "not working" every time: idle (turn finished),
needs attention (waiting on a person), unknown (nobody has told Agentd).

## 11. Quick checklist

1. `command -v agentd && agentd --version`, read the usage line.
2. `agentd list --json` once; note instance, revision, scan state.
3. Translate the roster; flag unknowns with likely reasons.
4. Mutations only on instruction: install, integrate, activity, restarts.
5. Never scrape screens or transcripts; never touch trust state; never start or
   stop the daemon unasked.
6. Multi-machine: SSH the user authorized, to hosts the user named, then the
   local CLI there. Never scan.
7. Report: answer first, roster second, actions for the user last.
