# Optional local attention bridge

The release archive carries `agentd-attention` as a separate optional program.
It reads the local `agentd watch --json` stream and posts `needs_attention`
messages to `$XDG_RUNTIME_DIR/attention/attention.sock`. It uses a local Unix
socket only, sends no message body, and does not change Agentd's daemon or watch
protocol.

The bridge uses the Agentd display name, then the tmux session, then the harness
as its title. When a tmux session is present, the message includes an argv to
attach to that session. It does not read prompts, pane contents, terminal
output, or transcripts.

The archive also carries `packaging/systemd/agentd-attention.service` for a
downstream installer that has chosen the optional component. The unit and
binary are packaged but are not installed or enabled by Agentd's standard
installation flow. The bridge expects `agentd` on `PATH` and the local
attention daemon to provide its socket; it retries both local connections with
a delay capped at five seconds.

For a foreground invocation in a user session with both local daemons
available:

```sh
agentd-attention
```

The process stays in the foreground for its service manager. `--version`
prints the bridge version. It accepts no remote endpoint or socket override.
