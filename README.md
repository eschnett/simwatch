# SimWatch

A terminal user interface that watches the progress of HPC simulations.

Each simulation writes a small status file, `simwatch.toml`, into its run
directory about once a minute (see [FORMAT.md](FORMAT.md)). SimWatch finds
these files below the directories you give it, shows them as a list, as
cards, or in detail, checks them against Slurm's queue, and flags
simulations that are stale or whose job died. Simulations can attach small
images (e.g. black hole tracks), which SimWatch shows as sixel graphics.

SimWatch keeps no state of its own; it only displays what it finds. It runs
in a terminal and works over ssh.

## Building

SimWatch is written in Rust. Install Rust with [rustup](https://rustup.rs)
(no root access needed), then:

```bash
cargo install --path .
```

This builds an optimized `simwatch` binary in `~/.cargo/bin`.

On Symmetry, do this once on a login node; `~/.cargo/bin` must be in your
`PATH`. Alternatively, cross-compile a static binary on another machine
with [cargo-zigbuild](https://github.com/rust-cross/cargo-zigbuild) and copy
it over:

```bash
cargo zigbuild --release --target x86_64-unknown-linux-musl
```

## Usage

```bash
simwatch /mnt/beegfs/$USER/runs ~/runs
```

- With no directories, SimWatch uses `roots` from the configuration file,
  or else the current directory.
- `simwatch --print DIR` prints the list once as plain text and exits.
- `simwatch --images none` turns off images. Sixel images need a terminal
  that supports sixel, such as WezTerm.
- `simwatch --help` lists all options.

### Keys

| Key | Action |
|---|---|
| `↑` `↓` / `j` `k` | Select a simulation (scroll in the detail view) |
| `PgUp` `PgDn` `g` `G` | Page, first, last |
| `Enter` / `→` | Detail view |
| `Esc` / `←` | Back from the detail view |
| `n` `p` | Next / previous simulation in the detail view |
| `[` `]` | Previous / next image |
| `Tab`, `1` `2` `3` | List, cards, detail view |
| `s` | Change the sort order (newest, name, state, last update) |
| `f` | Hide or show finished and failed simulations |
| `/` | Filter by name (`Enter` keeps the filter, `Esc` clears it) |
| `r` | Re-read the status files now |
| `R` | Scan the directories for new simulations now |
| `Ctrl-L` | Repaint the screen |
| `?` | Help |
| `q` | Quit |

The status bar shows a spinner while a directory scan, a re-read of the
status files, or an `squeue` call is running. Otherwise it shows how long
ago each one last finished.

### States

| State | Meaning |
|---|---|
| ▶ running | The status file is up to date |
| ◷ queued | Waiting in the Slurm queue |
| ? stale | The status file has not been updated for 3 × its `update_interval` |
| ✗ lost | The Slurm job is gone, but the simulation did not report that it ended |
| ‖ stopped | Stopped on purpose, e.g. at a wall time limit, to be continued |
| ✓ finished | Finished |
| ✗ failed | The simulation reported a failure |
| ! unreadable | The status file cannot be read |

A `!` after the state in the list (e.g. `running!`) means that the latest
version of the status file could not be read, and the previous one is shown.

## Configuration

`~/.config/simwatch/config.toml` (all settings optional):

```toml
roots = ["/mnt/beegfs/eschnetter/runs", "~/runs"]

refresh_interval = 60      # seconds between re-reading status files
scan_interval = 300        # seconds between scans for new simulations
squeue_interval = 120      # seconds between squeue calls
squeue_timeout = 20        # kill squeue after this many seconds
slurm = true               # false: never call squeue
squeue_program = "squeue"  # path to squeue, if not in PATH

stale_factor = 3           # stale after stale_factor × update_interval ...
stale_floor = 180          # ... but never before this many seconds
default_update_interval = 60

max_depth = 5              # how deep to search below each root
max_dirs = 5000            # directories visited per scan, at most
max_entries_per_dir = 2000 # entries read per directory, at most
max_sims = 500
skip_dirs = [".git", "target", "node_modules", "__pycache__"]

images = "sixel"           # or "none"
# font_size = [9, 18]      # terminal cell size in pixels, if not detected
```

## Robustness

SimWatch is meant to run for days on a shared login node:

- **Bounded scanning.** Directory scans are bounded in depth, number of
  directories, and entries per directory. Hidden directories are skipped and
  symbolic links to directories are not followed.
- **Cheap re-reads.** Between scans only the known status files are
  `stat`ed, and a file is read only when its modification time or size
  changed.
- **No blocking.** All file system access happens on one background thread,
  so a hanging file system cannot freeze the display or pile up threads.
- **One `squeue` call.** Slurm is asked with a single `squeue --user=$USER`
  call at most every two minutes, and that call is killed after a timeout.
- **Size limits.** Status files over 64 KiB are not read. Images over 1 MiB
  or 1024×1024 pixels are not read, and images are loaded only for the
  simulation shown in the detail view.

## Writing status files

- [FORMAT.md](FORMAT.md): the file format, and step-by-step instructions for
  adding status output to a simulation code (written for AI agents as well
  as people).
- [writers/julia/SimWatchStatus.jl](writers/julia/SimWatchStatus.jl): a
  drop-in Julia writer that depends only on the standard library.
- [writers/simwatch.sh](writers/simwatch.sh): shell helpers to mark a job as
  queued right after `sbatch`, or as failed when it exits with an error.

## Trying it out

```bash
cargo run --example fake_sims -- /tmp/simwatch-demo &   # keeps updating a few fake runs
cargo run -- /tmp/simwatch-demo
```

[IDEAS.md](IDEAS.md) collects ideas for later; [CODE.md](CODE.md) describes the code internals.
