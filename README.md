# agentd

`agentd` is a Linux user service that reports a truthful local roster of running
Codex and Claude coding-agent processes. It observes process existence through
`/proc`. Optional activity messages enrich an existing record but never create
or preserve one.

The current Agentd product release is v0.3.0.

The daemon keeps one atomic in-memory snapshot. Local clients read or subscribe
to complete snapshots through `$XDG_RUNTIME_DIR/agentd.sock`. Process identity
is the pair of PID and Linux process start-time ticks. Unknown presence, working
directory, activity, terminal, tmux location, display name, and process start
time values stay explicit.

## Build

Rust 1.97 or later is required.

```sh
cargo build --release
```

The build produces `target/release/agentd`.

Print the Agentd product version:

```sh
agentd --version
```

The v0.3.0 release prints `agentd 0.3.0`.

## Install the user service

```sh
install -Dm755 target/release/agentd "$HOME/.local/bin/agentd"
install -Dm644 packaging/systemd/agentd.service \
  "${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user/agentd.service"
systemctl --user daemon-reload
systemctl --user enable --now agentd.service
```

The systemd user manager supplies `XDG_RUNTIME_DIR`. The service creates the
socket at `$XDG_RUNTIME_DIR/agentd.sock` with mode `0600`.

## Inspect and enrich the roster

Print one human-readable snapshot:

```sh
agentd list
```

Print the unchanged JSON snapshot frame:

```sh
agentd list --json
```

Watch complete current-state snapshots:

```sh
agentd watch
agentd watch --json
```

Add an activity claim to the current identity for a PID:

```sh
agentd activity --pid 930481 --state active
agentd activity --pid 930481 --state idle
agentd activity --pid 930481 --state needs_attention
```

An activity command is silent when the daemon acknowledges it. The daemon
rejects a PID that is absent, uncertain, or reused with a different start time.

Set or clear a display name for the current exact process identity:

```sh
agentd name --pid 930481 "Agentd spec"
agentd name --pid 930481 --clear
```

A successful name command is silent. A name is 1 through 64 UTF-8 bytes and
contains no Unicode control character. Agentd preserves the accepted bytes. It
binds the name to both PID and Linux start-time ticks, so PID reuse cannot inherit
it. The name survives an Agentd restart in the same Linux boot while that exact
process stays live.

Agentd stores names in `$XDG_STATE_HOME/agentd/names.json` when
`XDG_STATE_HOME` is an absolute path. It uses
`$HOME/.local/state/agentd/names.json` when `XDG_STATE_HOME` is unset. The state
directory is mode `0700`, and the file is mode `0600`. Agentd refuses unsafe or
malformed state for name mutation while roster service continues with null names.

A user-authored Claude `SessionStart` hook can supply one constant name as an
argument:

```sh
agentd name --from-claude-session-start "Review lane"
```

This optional command discards hook stdin without parsing it. It sends only the
argument name and the exact mapped Claude root identity. Agentd does not install
or remove this optional hook. The automatic Claude integration still owns only
its four activity mappings.

Each JSON agent record also includes:

- `tty`: the controlling terminal from `/proc/<pid>/stat`, normalized as
  `pts/<index>` or `dev/<major>:<minor>`, or null.
- `tmux`: one best-effort per-scan mapping with `session`, `windowIndex`,
  `windowName`, and `paneId`, or null. Agentd runs at most one bounded
  `tmux list-panes` command per procfs scan. Missing, malformed, ambiguous, or
  unavailable tmux data fails open and does not degrade the scan.
- `name`: the exact eligible user-set display name, or null.
- `startedAtUnixMs`: Linux boot time plus process start-time ticks, or null when
  the integer conversion inputs are unavailable or invalid.

The schema marker remains `agentd.snapshot.v1`. These four fields are additive
and always present in v0.3 frames. A consumer that ignores unknown agent fields
remains compatible. A v0.3 consumer treats their absence in a v0.2 frame as null.
Strict consumers that reject unknown fields must upgrade.

Human `agentd list` and `agentd watch` lines lead with JSON-encoded name, tmux
location, and cwd basename. They retain the legacy raw full path or literal
`unknown` in the trailing `cwd` field.

## Install harness activity integrations

Agentd can install user-level command hooks for Claude Code and Codex. These
hooks enrich a procfs-discovered roster entry. They never create a roster entry.
They discard the complete hook payload and send only the mapped activity claim
for the exact process identity. If Agentd is unavailable, a hook prints one
typed diagnostic, exits successfully within its one-second harness timeout, and
does not block or fail the harness action.

`needs_attention` means that the latest accepted hook claim came from an event
mapped to user attention. The claim does not prove that the need still exists.
Agentd does not infer activity from time, output, terminal content, or process
behavior.

### Claude Code

The verified baseline is Claude Code `2.1.247`. Agentd writes only the Claude
user settings target: `$CLAUDE_CONFIG_DIR/settings.json` when
`CLAUDE_CONFIG_DIR` is an absolute path, or `$HOME/.claude/settings.json` when
the variable is unset.

Install the declarations:

```sh
agentd integrate install claude
```

Install does not change the roster or current activity. Procfs keeps an
already-running Claude process in the roster. Activation is restart-only for a
new or changed declaration. Exit the existing process and start a replacement
with the applicable conversation-preserving command:

```sh
claude --continue
claude --resume
```

Agentd does not document or rely on an in-session reload action. A process that
already loaded the same unchanged declaration can report its next mapped event
without another restart, including after an Agentd restart or an idempotent
reinstall. A replacement process enters the roster with activity `unknown`.
The first later accepted mapped event changes its claim.

| Claude event | Agentd activity claim |
| --- | --- |
| `UserPromptSubmit` | `active` |
| `PreToolUse` | `active` |
| `Stop` | `idle` |
| `Notification` | `needs_attention` |

Observe activation and later event claims in another terminal:

```sh
agentd watch
```

Uninstall the Claude declarations, then repeat the command as the teardown
check. The first command reports `result=changed` when entries existed. The
second reports `result=unchanged` and changes no bytes.

```sh
agentd integrate uninstall claude
agentd integrate uninstall claude
```

### Codex

The verified baseline is Codex CLI `0.149.1`. Agentd writes only the Codex user
hook target: `$CODEX_HOME/hooks.json` when `CODEX_HOME` is an absolute path, or
`$HOME/.codex/hooks.json` when the variable is unset. An enabled stable `hooks`
feature is required. Install runs `codex --version` and
`codex features list`. A missing or disabled feature returns
`unsupported_codex_hooks` and changes no target file.

Install the declarations:

```sh
agentd integrate install codex
```

The feature gate does not refuse another exact version. A hooks-enabled version
other than `codex-cli 0.149.1` proceeds with
`warning=unverified_codex_version`; that warning does not claim compatibility.

Install does not change the roster or current activity. Procfs keeps an
already-running Codex process in the roster. Activation is restart-only for a
new or changed declaration. Exit the existing process and start the replacement
with the conversation-preserving command:

```sh
codex resume
```

The next interactive startup reviews each new or changed Agentd hook. Continuing
without trust leaves that hook non-runnable. Trust approval makes only the
approved current definition runnable. Codex computes and stores the hook hash.
Agentd never reads or writes `config.toml`, `hooks.state`, or a trust hash, and it
never enables a trust bypass. Agentd does not document or rely on an in-session
reload action. A process that already loaded and trusted the same unchanged
declaration can report its next mapped event without another restart. A
replacement process enters the roster with activity `unknown`; the first later
accepted mapped event changes its claim.

| Codex event | Agentd activity claim |
| --- | --- |
| `UserPromptSubmit` | `active` |
| `PreToolUse` | `active` |
| `PermissionRequest` | `needs_attention` |
| `Stop` | `idle` |

Observe the replacement identity and later event claims in another terminal:

```sh
agentd watch
```

Uninstall the Codex declarations, then repeat the command as the teardown check.
Uninstall does not resolve Codex or apply its feature gate. The first command
reports `result=changed` when entries existed. The second reports
`result=unchanged` and changes no bytes.

```sh
agentd integrate uninstall codex
agentd integrate uninstall codex
```

### Mutation and uninstall guarantees

Install and uninstall preserve unrelated root values, hook events, matcher
groups, handlers, and array order. Install replaces only exact Agentd-owned
declarations and appends missing groups. Uninstall removes only command handlers
with the complete `--integration agentd-v1.1` marker and supported harness/event
shape, even if the absolute Agentd executable path changed. A command that only
contains the word `agentd`, has extra arguments, or uses an unsupported event is
reported as not removed and remains in place.

Each command refuses a symlink, non-regular target, or file not owned by the
invoking user without following its referent. A changed file is reread and merged
once. A second observed concurrent change returns `configuration_changed` and
does not replace the target. Changed content is flushed through an exclusively
created same-directory temporary file with initial mode `0600`, then atomically
renamed. Agentd creates no lock, journal, backup, receipt, or integration registry.

Uninstall keeps the user hook file and its root `hooks` object, including when
the object becomes empty. It preserves Codex-owned trust state unchanged. Codex
can reconcile its own trust metadata after declaration removal. A repeated
install or uninstall is byte-idempotent.

## Operate the service

Inspect status and logs:

```sh
systemctl --user status agentd.service
journalctl --user -u agentd.service
```

Stop or restart it:

```sh
systemctl --user stop agentd.service
systemctl --user restart agentd.service
```

Uninstall it:

```sh
systemctl --user disable --now agentd.service
rm "${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user/agentd.service"
systemctl --user daemon-reload
rm "$HOME/.local/bin/agentd"
```

The service stores no roster, revision, event, or activity history.

## Verification

Run each gate from a clean Linux checkout:

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --lib
cargo test --test integration
scripts/real-smoke.sh
```

The base real smoke requires authenticated `codex` and `claude` commands, `strace`,
a working systemd user manager, and provider access. It starts three Codex
processes and one Claude process in one dedicated directory. It captures their
real procfs fixtures, checks hookless discovery and activity enrichment,
records the expanded installed service command, and dynamically traces the
installed daemon's local-only transport. It tears down the processes and
service. It writes its evidence directory path on success.

After independent review of an unchanged v0.3.0 release candidate, the release
acceptance also follows both integration procedures above in real Gibson tmux
sessions. It
captures the replacement process identities and the `active`, `needs_attention`,
and `idle` sequence for each harness. It also checks fail-open behavior with the
socket unavailable, before-and-after hook-file hashes, Codex feature output and
trust ownership, repeated-uninstall byte identity, and complete teardown. The
acceptance capture retains no hook payload or credential.

The v0.3 acceptance additionally checks tmux and non-tmux records, exact name
set/no-op/restart/clear/stale behavior, independent start-time arithmetic,
missing-tmux fail-open behavior, additive JSON fields, human rendering, and
absence of private prompt, command, environment, screen, and transcript
sentinels on Gibson and Osanwe.

[The verification map](docs/verification.md) links each acceptance case to its
automated or real-host proof and lists the real-smoke evidence files.

## Version 1 boundaries

Agentd does not provide remote or multi-host aggregation, a graphical or web
interface, transcript handling, LLM calls of its own, agent steering, a plugin
or provider framework, macOS support, or Windows support. It does not
authenticate process vendors, discover other users' processes, replay events,
infer progress, or turn elapsed time into activity. Its only persistent state is
the bounded same-boot exact-identity display-name registry.
