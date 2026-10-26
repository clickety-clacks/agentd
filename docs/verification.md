# Verification map

Run these pre-review commands from a clean Linux checkout:

```sh
cargo test --lib
cargo test --test integration
cargo test --test release
```

`scripts/real-smoke.sh` is an optional diagnostic on an unchanged commit, not a
product acceptance or release gate. It requires authenticated Codex and Claude
installations, `strace`, and a systemd user manager.

| Acceptance | Proof |
| --- | --- |
| A1 | The integration suite checks four sibling procfs roots, helper collapse, working-directory filtering, UID filtering, cross-UID ancestry, root promotion, and activity reset. The optional smoke can supply three genuine Codex processes and one genuine Claude process in one directory. |
| A2 | State and protocol unit tests check hook-only enrichment, unknown identities, equal-time no-op claims, removal ordering, and scan/activity carry. The integration suite exercises the silent activity CLI. |
| A3 | The integration suite measures real procfs addition and removal below two seconds. The optional smoke records genuine-harness discovery and exit milliseconds. |
| A4 | The PID-reuse unit test replaces one start time with another in one commit and proves that the activity claim is discarded. |
| A5 | Scanner unit tests inject enumeration, status, cwd, and ancestry failures and check total typed uncertainty. |
| A6 | The state seam owns snapshot replacement, subscriber registration, and activity mutation under one lock. Unit tests check timestamp-only scans, scan/activity interleaving, reason precedence, and removal/activity ordering. |
| A7 | A subscriber slot is structurally one optional pending frame. One unit test offers 1,000 frames and observes only the newest. A server-lifetime regression proves that the already-sent initial frame is released before the changed-frame loop. The server configures one fixed socket send-buffer bound. |
| A8 | Strict-parser unit tests cover exact fields and duplicate keys. Integration tests cover every closed error class and both byte-limit boundaries. |
| A9 | Unit tests check exact degraded-empty human output. Integration tests execute JSON inspection and silent successful activity. |
| A10 | Integration tests check the fixed unit contract and graceful SIGTERM cleanup. The optional smoke can inspect systemd's expanded `ExecStart`, mode `0600`, restart behavior, and stop cleanup. |
| A11 | Integration tests exercise stale-socket replacement and refusal without mutation for regular files and live listeners. The owner check is enforced at the same startup seam. |
| A12 | The runtime has one Unix listener and reads only procfs stat, status, and cwd. The optional smoke can capture socket inventories and a `strace` of the daemon, then check transport and private-data sentinels. |
| A13 | A committed fixture replays stat, status UID, harness, parent, start-time, and cwd fields captured from real Codex and Claude processes. The optional smoke can capture comparable fixture input and run the ignored replay test. |
| A14 | The README defines the unit and integration commands. Optional smoke output records its command, identities, measurements, logs, and teardown for diagnosis. |
| A15 | Injected-view and cycle unit tests cover changed parent IDs, finite repeated-PID traversal, typed `process_raced` issues, retained unknown identities, and omitted new identities. |

## Version 1.1 amendment

The v1.1 amendment preserves every base gate and adds these proofs:

| Acceptance | Proof |
| --- | --- |
| A1 | Library tests run install and uninstall twice against Claude and Codex fixtures with unrelated root values, events, groups, handlers, and ordered arrays. They prove exact-one canonical declarations after install, byte identity on the second operation, exact-owned removal after an executable move, preserved unrelated order, retained hook files and root `hooks` objects, and byte-identical Codex `config.toml`. The no-follow regression proves that a symlink target is refused without changing its referent. |
| A2 | An optional post-review diagnostic can follow the release README in a real tmux session on an unchanged candidate. It records observed versions, process identities, activity frames, socket-unavailable behavior, and teardown without retaining hook payloads or credentials. |

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
registry privacy. An optional real-host diagnostic can supplement these checks;
it is not a required acceptance matrix.

The optional post-review diagnostic can record the candidate commit and binary
hash, kernel, tmux version, boot ID, tick rate, instance IDs, and process
identities. It can observe tmux and non-tmux roots, name lifecycle, restart
behavior, socket/service contracts, private sentinel absence, and teardown.
These observations supplement the automated gates and do not install or release
the candidate.

The real smoke writes under `target/agentd-smoke/<UTC-run-id>/`. Its key files
are `commit.txt`, `command.txt`, `socket-path.txt`, `exec-start.txt`, `instance-ids.txt`,
`four-agents.json`, `watch.ndjson`, `fixtures/`, `fixture-replay.log`,
`discovery-ms.txt`, `exit-ms.txt`, `stop-ms.txt`, the socket inventories, the
dynamic `network-trace.log`, `network-trace-window.txt`,
`network-trace-activity.json`, the service journal, and `teardown-result.txt`.

## Optional bridge package behavior

The package behavior tests prove that a core archive succeeds and remains
usable when the optional bridge binary is absent, with a warning and no bridge
files. When the bridge binary, matching version, invocation documentation, and
service unit are available, packaging validates and includes those files and
the test runs the extracted bridge to confirm its `--version` output. The
packager emits `SHA256SUMS` for downstream artifact-integrity checks; that
receipt and archive byte reproducibility are not product acceptance gates.

The release workflow also requires the pushed tag, crate version, and package
version to agree. This is a publication identity safeguard against mislabeled
artifacts, not an application acceptance test.

Before review, run `scripts/package-release.sh --dry-run` against the locked
release binaries, then package once into a local candidate output directory.
This stage creates local candidate assets only. It does not install the skill
or publish a release.

## Optional Agentd attention bridge

The bridge unit tests exercise the rev4 N4 contract without changing Agentd's
daemon or watch protocol:

| Acceptance | Product behavior proved | Test evidence |
| --- | --- | --- |
| AB-1 | Sender identity combines normalized machine, bridge instance, and Agentd process identity; unrepresentable machine segments are rejected. | `machine_name_normalizes_once_and_rejects_unrepresentable_segments`; `message_uses_exact_identity_title_priority_and_open_command_without_body` |
| AB-2 | Episode ids and creation times use the observed timestamp; title priority is display name, tmux session, then harness; tmux targets become argv vectors. | `message_uses_exact_identity_title_priority_and_open_command_without_body` |
| AB-3 | Degraded scans, unknown activity, and missing timestamps preserve open messages; complete `active`/`idle` states or complete absence clear only local messages. | `incomplete_unknown_and_null_timestamp_do_not_change_open_messages`; `active_idle_and_complete_absence_clear_local_messages_only` |
| AB-4 | Startup posts current claims before requesting an empty-sender subscription snapshot, then marks prior local episodes superseded. | `initial_sync_posts_before_subscribing_and_superseding_old_episode` |
| AB-5 | Startup keeps answered duplicates closed; answer frames and foreign lifecycle frames do not mutate local bridge state. | `answered_duplicate_stays_closed_and_answer_frames_are_ignored` |
| AB-6 | Subscription sends an empty sender set and consumes the daemon's current message snapshot, including foreign messages for local reconciliation. | `subscribe_uses_empty_set_and_reads_the_daemon_snapshot` |
| AB-7 | The bridge uses published process identity/activity/name/harness/tmux fields and emits no message body or pane data. | `message_uses_exact_identity_title_priority_and_open_command_without_body` |
| AP-1 | A post is sent as a v1 newline-delimited message frame with a request reference and no body. | `attention_transport_sends_versioned_line_and_waits_for_matching_ack` |
| AP-2 | The client accepts a matching acknowledgment; subscription also reads the initial message snapshot after its acknowledgment. | `attention_transport_sends_versioned_line_and_waits_for_matching_ack`; `subscribe_uses_empty_set_and_reads_the_daemon_snapshot` |
| Packaging | A core archive is usable with the bridge omitted; when the bridge payload is available and matches the release version, packaging includes its executable, invocation docs, and service unit. | `core_archive_succeeds_when_optional_attention_binary_is_absent`; `optional_attention_archive_contains_binary_docs_and_service_unit` |

These tests use published-snapshot fixtures and a protocol peer. They do not
claim interoperability with the separately maintained attention daemon; that
is verified against the core owner's reviewed implementation on its approved
runner before release acceptance.

`scripts/run-candidate-tests.sh` is an optional helper for already-built Linux
test artifacts; Linux procfs is a product capability exercised by Agentd's
process-discovery tests, and the runner architecture is informational rather
than an admission condition. The helper checks the recorded SHA256 manifest
and runs only listed test executables serially with private home, temporary,
and XDG state. It clears inherited `XDG_RUNTIME_DIR`, keeps installed Agentd
off `PATH`, and terminates the test process group before removing its scratch
directory. These manifest, private-state, and cleanup checks safeguard test
execution; they are not product acceptance cases. Integration and release
tests resolve candidate binaries and repository files from the runner's
current candidate root, so that root need not match the build host's absolute
path. The `rustc` adapter provides only the recorded build-host identity to
the package test; it cannot compile code. No particular runner hostname is
part of the bridge contract. This helper does not run
`scripts/real-smoke.sh` or establish interoperability with the matching N2
implementation.
