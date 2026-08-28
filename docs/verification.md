# Verification map

Run these pre-review commands from a clean Linux checkout:

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --lib
cargo test --test integration
```

After code review, run `scripts/real-smoke.sh` on the unchanged commit. The
script requires real authenticated Codex and Claude installations, `strace`,
and a systemd user manager.

| Acceptance | Proof |
| --- | --- |
| A1 | The integration suite checks four sibling procfs roots, helper collapse, working-directory filtering, UID filtering, cross-UID ancestry, root promotion, and activity reset. The real smoke supplies three genuine Codex processes and one genuine Claude process in one directory. |
| A2 | State and protocol unit tests check hook-only enrichment, unknown identities, equal-time no-op claims, removal ordering, and scan/activity carry. The integration suite exercises the silent activity CLI. |
| A3 | The integration suite measures real procfs addition and removal below two seconds. The real smoke records genuine-harness discovery and exit milliseconds. |
| A4 | The PID-reuse unit test replaces one start time with another in one commit and proves that the activity claim is discarded. |
| A5 | Scanner unit tests inject enumeration, status, cwd, and ancestry failures and check total typed uncertainty. |
| A6 | The state seam owns snapshot replacement, subscriber registration, and activity mutation under one lock. Unit tests check timestamp-only scans, scan/activity interleaving, reason precedence, and removal/activity ordering. |
| A7 | A subscriber slot is structurally one optional pending frame. One unit test offers 1,000 frames and observes only the newest. A server-lifetime regression proves that the already-sent initial frame is released before the changed-frame loop. The server configures one fixed socket send-buffer bound. |
| A8 | Strict-parser unit tests cover exact fields and duplicate keys. Integration tests cover every closed error class and both byte-limit boundaries. |
| A9 | Unit tests check exact degraded-empty human output. Integration tests execute JSON inspection and silent successful activity. |
| A10 | Integration tests check the fixed unit contract and graceful SIGTERM cleanup. The real smoke installs and enables the unit, records systemd's expanded `ExecStart`, asserts the absolute binary followed by `daemon` with no socket argument, checks mode `0600`, forces an unsuccessful daemon exit, observes systemd restart with a new instance, and measures stop cleanup. |
| A11 | Integration tests exercise stale-socket replacement and refusal without mutation for regular files and live listeners. The owner check is enforced at the same startup seam. |
| A12 | The runtime has one Unix listener and reads only procfs stat, status, and cwd. The real smoke keeps point-in-time socket inventories and also runs the installed binary under `strace` across a scheduled procfs scan and an accepted activity request. It retains the trace, asserts Unix transport and procfs access, and rejects any `AF_INET` or `AF_INET6` socket or connect. It also searches frames, CLI output, stderr, and the service journal for command-line and environment sentinels. |
| A13 | A committed fixture replays stat, status UID, harness, parent, start-time, and cwd fields captured from three real Codex and one real Claude process. The real smoke captures the shared-cwd processes and runs the ignored captured-procfs replay test against those exact bytes. |
| A14 | The README defines the format, static-analysis, unit, integration, and real-smoke commands. The real smoke records the commit, command, socket path, instance IDs, identities, deadline measurements, logs, and teardown. |
| A15 | Injected-view and cycle unit tests cover changed parent IDs, finite repeated-PID traversal, typed `process_raced` issues, retained unknown identities, and omitted new identities. |

## Version 1.1 amendment

The v1.1 amendment preserves every base gate and adds these proofs:

| Acceptance | Proof |
| --- | --- |
| A1 | Library tests run install and uninstall twice against Claude and Codex fixtures with unrelated root values, events, groups, handlers, and ordered arrays. They prove exact-one canonical declarations after install, byte identity on the second operation, exact-owned removal after an executable move, preserved unrelated order, retained hook files and root `hooks` objects, and byte-identical Codex `config.toml`. The no-follow regression proves that a symlink target is refused without changing its referent. |
| A2 | After independent review, one real Gibson tmux acceptance follows the release README on the unchanged candidate. It records the exact product and spec commits, README lines, Claude and Codex versions, Codex feature output, tmux sessions, process identities, install output, hook-file hashes, `active`/`needs_attention`/`idle` frames, socket-unavailable diagnostics and elapsed times, repeated uninstall hashes, Codex trust ownership, and teardown. It retains no hook payload or credential. |

Unit and integration regressions also prove the closed `needs_attention` wire and
CLI value, exact Claude and Codex event mappings, same-harness ancestor collapse
through nested and mixed-harness trees, payload-discarding typed fail-open output,
the 750 ms acceptance bound, Codex hooks-feature gating, unverified-version warning,
restart-only install guidance, and no Codex resolution during uninstall.

The real smoke writes under `target/agentd-smoke/<UTC-run-id>/`. Its key files
are `commit.txt`, `command.txt`, `socket-path.txt`, `exec-start.txt`, `instance-ids.txt`,
`four-agents.json`, `watch.ndjson`, `fixtures/`, `fixture-replay.log`,
`discovery-ms.txt`, `exit-ms.txt`, `stop-ms.txt`, the socket inventories, the
dynamic `network-trace.log`, `network-trace-window.txt`,
`network-trace-activity.json`, the service journal, and `teardown-result.txt`.
