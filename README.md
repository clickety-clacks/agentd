# agentd

`agentd` is a planned Linux service for a truthful local registry and event
stream of running coding agents.

## Version 1 contract

Version 1 will treat `/proc` process existence as authoritative. Lifecycle
hooks may enrich status but will not override process truth. Unknown values
will remain explicit. A process will be identified by its PID and process start
time.

Consumers will receive one atomic roster snapshot and updates through one local
Unix event socket. Version 1 will also include a command-line inspection tool
and systemd user-service lifecycle support.

Acceptance will use real Linux processes: three Codex processes and one Claude
process sharing one working directory.

## Non-goals for version 1

Version 1 will not provide remote aggregation, a user interface, transcript
display, LLM calls, agent steering, a plugin framework, macOS support, or
Windows support.

This repository currently contains the project contract only. It does not yet
contain an implementation.
