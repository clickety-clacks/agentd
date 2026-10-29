# Optional bridge/daemon pair check

`run.py` is a separate, opt-in interoperability fixture for a built
`agentd-attention` binary and a built `attentiond` binary. It uses the real local
attention daemon socket and a controlled `agentd.snapshot.v1` stream. It does
not change either component's source, install a package, or use a user service.

The input directory contains copied executable artifacts and a JSON identity
manifest. The manifest schema is `agentd-attention-pair/v1`:

```json
{
  "schema": "agentd-attention-pair/v1",
  "agentd": {
    "source": { "commit": "<40 hex>", "tree": "<40 hex>" },
    "build": {
      "command": ["cargo", "build", "--release", "--locked", "--bin", "agentd-attention"],
      "target": "<target triple>",
      "profile": "release",
      "features": [],
      "rustc": "<exact version>",
      "cargo": "<exact version>"
    },
    "binary": { "path": "<absolute path under inputs-dir>", "sha256": "<64 hex>" }
  },
  "attentiond": {
    "source": { "commit": "<40 hex>", "tree": "<40 hex>" },
    "build": {
      "command": ["<exact build command>"],
      "target": "<target triple>",
      "profile": "<profile>",
      "features": [],
      "rustc": "<exact version>",
      "cargo": "<exact version>"
    },
    "binary": { "path": "<absolute path under inputs-dir>", "sha256": "<64 hex>" }
  }
}
```

The runner checks both executable hashes, requires the Agentd source commit and
tree to match its clean checkout, copies both binaries into private scratch,
and launches them directly. It creates private `HOME`, `TMPDIR`, XDG runtime,
config, data, and state directories; the attention daemon's socket and store
remain within that per-run tree. It records both source and build identities,
runtime compatibility, invocation, process commands, checks, reconnect timing,
process exit codes, and limits in `result.json`, and retains process logs in
the evidence directory. Cleanup only signals process groups started by this
runner and removes its own scratch directory only after every owned process
group has stopped. SIGTERM or SIGINT unwinds through the same cleanup path; if
the runner leader has exited but a process in its group remains, cleanup still
signals that group. Cleanup attempts every owned group and retains scratch when
stopping a group fails.

The controlled stream covers the initial incomplete scan, post identity and
privacy, duplicate acknowledgment without redelivery, incomplete/unknown no-op
behavior, episode supersession, Agentd watch loss and recovery, instance
replacement, and clearing. It separately stops the attention daemon while
leaving the controlled Agentd input unstopped, observes the bridge's disconnect
and retry logs, then checks persisted open state and a new local post after the
private socket returns. It answers a bridge episode, restarts the actual daemon,
checks that the answered id remains closed, verifies duplicate acknowledgment
without redelivery, and checks a subsequent local post and clear. The report
measures delivery time from observed socket readiness against the five-second
backoff cap plus an 800 ms fixture tolerance; it does not assert a fixed retry
schedule. A focused unit test checks that retry delays are nonzero, grow, and
stay within the cap. The fixture uses no prompt, pane, terminal output, or
transcript data.
The Agentd watch stream is controlled rather than produced by a running Agentd
daemon; the bridge executable and attention daemon are the exact binaries named
in the manifest. The result does not prove fresh-login or reboot behavior,
whole-host isolation, or unobservable descendant state.

Run only in a private test environment after the exact component build records
and pair-specific test admission are available:

```sh
python3 tests/attention-pair/run.py \
  --manifest /absolute/private/inputs/pair.json \
  --inputs-dir /absolute/private/inputs \
  --scratch-parent /absolute/private/scratch-parent \
  --evidence-dir /absolute/private/evidence
```

The script refuses relative paths, mismatched source or binary hashes, dirty
Agentd source, symlinked binaries, and non-private input/scratch directories.
It never invokes a service manager or relies on an installed `agentd` or
`attentiond` executable.
