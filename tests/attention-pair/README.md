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
runner and removes its own scratch directory after all owned processes stop.

The controlled stream covers the initial incomplete scan, post identity and
privacy, duplicate acknowledgment without redelivery, incomplete/unknown no-op
behavior, episode supersession, Agentd watch loss and recovery, instance
replacement, daemon restart and persisted state, reconnect supersession within
the five-second retry cap plus an 800 ms observation tolerance, clearing, and
preservation of another machine's open message. A focused unit test checks the
retry sequence and exact cap. The fixture uses no prompt, pane, terminal output,
or transcript data.
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
