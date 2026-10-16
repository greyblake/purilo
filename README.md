# Purilo

(from Esperanto "a cleaning tool")

Frees the disk space taken by build outputs and caches:

- `target/` of Rust projects under `~` (through `cargo clean`)
- `node_modules/` of JavaScript projects under `~`
- the [kache](https://github.com/kunobi-ninja/kache) build cache
- temporary files of finished Claude Code sessions

It leaves alone anything that may still be in use:

- projects that a running process uses (a dev server, a language server, a shell in them);
- projects changed in the last 24 hours, or in the last 2 hours when their `target/` or
  `node_modules/` takes more than 5 GiB;
- Claude Code sessions that are running or changed in the last 24 hours.

## Usage

```
purilo
```

It prints what was cleaned and kept, and how much space was freed. Ctrl-C stops before the
next item and still prints the report.
