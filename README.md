# agentd

`agentd` is a Linux user service that reports a truthful local roster of running
Codex and Claude coding-agent processes. It observes process existence through
`/proc`. Optional activity messages enrich an existing record but never create
or preserve one.

The daemon keeps one atomic in-memory snapshot. Local clients read or subscribe
to complete snapshots through `$XDG_RUNTIME_DIR/agentd.sock`. Process identity
is the pair of PID and Linux process start-time ticks. Unknown presence, working
directory, and activity values stay explicit.

## Build

Rust 1.97 or later is required.

```sh
cargo build --release
```

The build produces `target/release/agentd`.

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
```

An activity command is silent when the daemon acknowledges it. The daemon
rejects a PID that is absent, uncertain, or reused with a different start time.

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

The real smoke requires authenticated `codex` and `claude` commands, `strace`,
a working systemd user manager, and provider access. It starts three Codex
processes and one Claude process in one dedicated directory. It captures their
real procfs fixtures, checks hookless discovery and activity enrichment,
records the expanded installed service command, and dynamically traces the
installed daemon's local-only transport. It tears down the processes and
service. It writes its evidence directory path on success.

[The verification map](docs/verification.md) links each acceptance case to its
automated or real-host proof and lists the real-smoke evidence files.

## Version 1 boundaries

Agentd does not provide remote or multi-host aggregation, a graphical or web
interface, transcript handling, LLM calls of its own, agent steering, a plugin
or provider framework, macOS support, or Windows support. It does not
authenticate process vendors, discover other users' processes, persist a
registry, replay events, infer progress, or turn elapsed time into activity.
