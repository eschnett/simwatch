# CLAUDE.md

SimWatch is a Rust terminal UI that watches HPC simulations. Each simulation
writes a `simwatch.toml` status file; SimWatch finds and displays these
files, checks them against `squeue`, and shows small images as sixel
graphics. The first user is TreeGeneralizedHarmonic (a Julia code) on the
Symmetry cluster at Perimeter Institute.

## Documents

- [GOALS.md](GOALS.md): the original goals, written by the user. Do not edit
  unless asked.
- [README.md](README.md): users' guide (building, keys, config, states).
- [FORMAT.md](FORMAT.md): the status file format, including instructions for
  AI agents that add status output to a simulation code.
- [CODE.md](CODE.md): code internals: threads, data flow, modules, pitfalls.
- [IDEAS.md](IDEAS.md): ideas deliberately postponed. Add to it rather than
  implementing them unasked.

## Commands

```bash
cargo build --release
cargo test
cargo clippy --all-targets          # must be warning-free
cargo run -- --print DIR            # one-shot plain-text list, no TTY needed
cargo run --example fake_sims -- DIR [--once]   # demo simulations in every state
cargo run -- DIR                    # interactive (needs a real terminal)
```

`sed` on this machine is GNU sed: use `sed -i 's/…/…/'`, not `sed -i ''`.

## Design principles

- **Opportunistic, not a protocol.** Every key in `simwatch.toml` is
  optional. No format version, no required keys, no rejecting files: missing
  values become placeholders such as "(missing simulation name)", values of
  unexpected type are shown generically, and unknown keys are listed as
  problem-specific entries. Keep it that way.
- **Stateless.** SimWatch only displays what it finds on disk; it keeps no
  history or database.
- **Robust on a shared login node.** Bounded directory scans, no following
  symlinked directories, size limits on every file read, at most one
  `squeue` call at a time with a timeout, and no file system access on the UI
  thread. Any new I/O must respect these rules (see CODE.md).
- **Sixel only for images**, forced regardless of what the terminal
  advertises. The user runs WezTerm over ssh, without tmux. Other protocols
  and text fallbacks are in IDEAS.md.

## When changing things

- A change to the status file format touches, together: `src/format.rs`
  (and its tests), the UI that displays it, `FORMAT.md`,
  `writers/julia/SimWatchStatus.jl`, and `examples/fake_sims.rs`.
- A new configuration key goes into `FileConfig` and `Config` in
  `src/config.rs` and into the example in README.md. `FileConfig` rejects
  unknown keys, so the README example must stay valid.
- A new key binding goes into `handle_key` in `src/ui/mod.rs`, the help
  overlay (`src/ui/help.rs`), and README.md.
- Code style: match the surrounding code; short doc comments on items,
  comments only where the reason is not obvious. Tests live in `#[cfg(test)]`
  modules next to the code.
- Do not call `Terminal::clear()`, and do not use
  `Picker::from_query_stdio()` without trying the ioctl first; CODE.md
  explains why.
