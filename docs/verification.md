# Verification map

Run these pre-review commands from a clean Linux checkout:

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --lib
cargo test --test integration
cargo test --test release
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

## Version 0.3 display identity amendment

The v0.3 unit suite proves the additive snapshot fields and v0.2 missing-field
decode rule; signed procfs `tty_nr` parsing and two-read disagreement; exact
boot-time/tick arithmetic and overflow; one reusable tmux index per scan;
byte-length framing with delimiter and multibyte values; malformed, invalid UTF-8,
semantic-control, absent, nonmatching, and ambiguous tmux cases; exact display-name
validation; atomic name set/no-op/clear/stale mutation; scan/name interleaving;
registry mode, schema, same-boot recovery, cleanup, and PID reuse; deterministic
human rendering; and privacy-safe Claude SessionStart fail-open input.

The Linux integration suite starts an isolated daemon and real procfs-discovered
process. It proves all four JSON fields, exact name set and identical-set no-op,
mode-`0600` persistence, same-boot daemon-restart retention with revision 1 and
unknown activity, stale-identity refusal, clear and identical-clear no-op, and
registry privacy. It does not replace the post-review real-host matrix.

After exact-commit code review, the unchanged candidate runs the full v0.2 gates
and a Gibson/Osanwe matrix. Each host records the candidate commit and binary
hash, kernel, tmux version, boot ID, tick rate, instance IDs, and exact process
identities. It proves tmux and non-tmux roots, independent timestamp arithmetic,
name lifecycle, an isolated PATH-without-tmux daemon, restart behavior, retained
socket/service contracts, private sentinel absence, and teardown. This repository
stage does not install or release the candidate.

The real smoke writes under `target/agentd-smoke/<UTC-run-id>/`. Its key files
are `commit.txt`, `command.txt`, `socket-path.txt`, `exec-start.txt`, `instance-ids.txt`,
`four-agents.json`, `watch.ndjson`, `fixtures/`, `fixture-replay.log`,
`discovery-ms.txt`, `exit-ms.txt`, `stop-ms.txt`, the socket inventories, the
dynamic `network-trace.log`, `network-trace-window.txt`,
`network-trace-activity.json`, the service journal, and `teardown-result.txt`.

## Version 0.3.1 operator skill and package amendment

The release test checks the operator skill's exact frontmatter, the two
contract corrections to its authoritative source, the site-data denylist, the
explicit installation-authorization rule, and consistent v0.3.1 metadata. It
also packages the test binary twice with one fixed source epoch and proves
identical archive bytes, the complete manifest, fixed directory and file
modes, the skill bytes in the archive, and a matching `SHA256SUMS` receipt.

Before review, run the skill creator validator directly on
`skills/agentd`, run `scripts/package-release.sh --dry-run` against the locked
release binary, and reproduce the package into two separate output
directories. This stage creates local candidate assets only. It does not
install the skill or publish a release.

## Optional Agentd attention bridge

The bridge unit tests exercise the rev4 N4 contract without changing Agentd's
daemon or watch protocol:

| Acceptance | Proof |
| --- | --- |
| AB-1, AB-2 | Exact normalized sender and episode ids, observed timestamp, title fallback order, and tmux argv. |
| AB-3, AB-4 | Complete-scan clear, degraded/unknown/null-time no-op, and new message posting before superseding the prior open episode. |
| AB-5 | Startup posts current claims before the empty subscription snapshot, preserves answered duplicates, and reconciles old local messages. |
| AB-6 | Empty local subscription; foreign senders and answer frames do not change bridge state. |
| AB-7 | The bridge projection reads only snapshot identity, activity, name, harness, and tmux session fields; outgoing messages omit body and pane data. |
| AP-1, AP-2 | The socket peer verifies versioned newline-delimited frames, request references, acknowledgment matching, and empty subscription framing. |
| Packaging | The release test checks both executable versions, fixed archive modes and entries, and reproducible archive bytes. |

These tests use published-snapshot fixtures and a protocol peer. They do not
claim interoperability with the separately maintained attention daemon; that
is verified against the core owner's reviewed implementation on its approved
runner before release acceptance.

For the authorized Linux candidate route, build test artifacts with
`cargo test --no-run --locked --all-targets --all-features` in an isolated
Gibson worktree, then transfer the exact binaries, candidate files, test plan,
Rust host triple and SHA256 manifest to Racter. Use the same absolute candidate
root on both hosts because the release test embeds `CARGO_MANIFEST_DIR`. Run
only through `scripts/run-candidate-tests.sh`. It verifies the manifest, executes test
harnesses serially with a private home and temporary directory, clears the
inherited `XDG_RUNTIME_DIR` and keeps installed Agentd off `PATH`. The wrapper
uses the recorded build-host triple only for the packaging script's `rustc -vV`
identity query; it cannot compile code. Do not use this route for
`scripts/real-smoke.sh` or claim daemon interoperability until the matching N2
implementation is available and tested.
