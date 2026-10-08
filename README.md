# SimWatch

A terminal user interface that watches the progress of HPC simulations.

Each simulation writes a small status file, `simwatch.toml`, into its run
directory about once a minute (see [FORMAT.md](FORMAT.md)). SimWatch finds
these files below the directories you give it, shows them as a list, as
cards, or in detail, checks them against Slurm's queue, and flags
simulations that are stale or whose job died. Simulations can attach small
images (e.g. black hole tracks), which SimWatch shows as sixel graphics.

SimWatch keeps no state of its own; it only displays what it finds. It runs
in a terminal and works over ssh. It can also run on your laptop and watch
directories on one or more clusters, logging in to each only once (see
[Remote hosts](#remote-hosts)).

## Building

SimWatch is written in Rust. Install Rust with [rustup](https://rustup.rs)
(no root access needed; Rust 1.90 or later), then:

```bash
cargo install simwatch
```

This downloads SimWatch from [crates.io](https://crates.io/crates/simwatch)
and builds an optimized `simwatch` binary in `~/.cargo/bin`. From a
checkout of the repository, use `cargo install --path .` instead.

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
- `HOST:DIR` watches a directory on a remote host over ssh, e.g.
  `simwatch symmetry:/mnt/beegfs/$USER/runs ~/runs`.
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
| `s` | Change the sort order (newest, name, state, last update, group) |
| `f` | Hide or show finished and failed simulations |
| `/` | Filter by name, directory or group (`Enter` keeps the filter, `Esc` clears it) |
| `r` | Re-read the status files now |
| `R` | Scan the directories for new simulations now |
| `c` | Reconnect to remote hosts whose connection was lost |
| `Ctrl-L` | Repaint the screen |
| `?` | Help |
| `q` | Quit |

The status bar shows a spinner while a directory scan, a re-read of the
status files, or an `squeue` call is running. Otherwise it shows how long
ago each one last finished. With remote hosts it shows this for each host,
and which hosts are disconnected.

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

A simulation that has been resubmitted is judged by the job it is waiting
for. The Job column then shows that job (e.g. `1234602 PD #4`: the
simulation's fourth job, pending).

### What the views show

- **List:** one line per simulation. A *Host* column appears when the
  simulations come from more than one host, a *Group* column when some
  simulation has a `group`, and a *Summary* column with its headline values
  when some simulation names them in `summary`; an arrow (↑ ↓ →) shows
  their trend when the simulation records a `[history]`. On narrow
  terminals the less important columns (resources, wall time, speed, …)
  are left out.
- **Cards:** a few lines per simulation, with the summary values and their
  recent history as sparklines.
- **Detail:** everything, including a *History* section with a sparkline
  and the exponential growth rate of each recorded series, the earlier
  Slurm jobs of the simulation, and its images.

## Configuration

`~/.config/simwatch/config.toml` (all settings optional):

```toml
roots = ["/mnt/beegfs/eschnetter/runs", "~/runs"]   # "HOST:DIR" for remote hosts

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

ssh = ["ssh"]              # ssh command and options, e.g. ["ssh", "-C"]
remote_program = "simwatch" # simwatch on remote hosts, e.g. "~/.cargo/bin/simwatch"
auto_reconnect = true      # reconnect lost hosts if no password is needed
```

## Remote hosts

SimWatch can run on your own computer and watch directories on clusters:

```bash
simwatch symmetry:/mnt/beegfs/$USER/runs graham:~/runs ~/local-runs
```

A root `HOST:DIR` (a colon before any `/`, as for scp) is on a remote host.
`HOST` is passed to ssh as given, so `user@host` and aliases from
`~/.ssh/config` work.

- **One login per host.** For each host SimWatch runs
  `ssh HOST simwatch --serve` once and keeps that connection for the whole
  session. The remote simwatch scans, reads the status files and calls
  `squeue` there, with all the limits below, and sends back what changed.
  Images are fetched over the same connection.
- **simwatch must be installed on the remote host**, in the same version
  (see [Building](#building)). If it is not in the `PATH` of a
  non-interactive ssh session, set `remote_program`, e.g.
  `"~/.cargo/bin/simwatch"`. The remote host's own configuration file is
  only used for `squeue_program` and `slurm = false`; everything else comes
  from your local configuration.
- **Passwords and MFA.** SimWatch connects to each host before the display
  starts, so ssh can ask for passwords or MFA codes as usual.
- **Lost connections.** ssh notices a dead connection within about two
  minutes. The host's simulations stay in the list (and turn *stale*), and
  the status bar shows the host as disconnected. With `auto_reconnect`,
  SimWatch tries again now and then, but only without prompting (ssh's
  `BatchMode`), which works with keys or an ssh agent but not with MFA.
  Press `c` to reconnect: SimWatch gives the terminal back to ssh for the
  password, then returns to the display.
- **Slurm jobs** are looked up in the `squeue` output of their own host.

If you use ssh connection sharing, SimWatch reuses an existing login and
needs no password at all, and its own connection serves your other ssh
sessions:

```
# ~/.ssh/config
Host symmetry
    ControlMaster auto
    ControlPath ~/.ssh/control-%C
    ControlPersist 8h
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
- **The same on remote hosts.** `simwatch --serve` follows the same rules on
  the remote login node, and exits when its ssh connection ends.
- **Size limits.** Status files over 128 KiB are not read. Images over 1 MiB
  or 1024×1024 pixels are not read, and images are loaded only for the
  simulation shown in the detail view.

## Writing status files

- [FORMAT.md](FORMAT.md): the file format, and step-by-step instructions for
  adding status output to a simulation code (written for AI agents as well
  as people).
- [writers/julia/SimWatchStatus.jl](writers/julia/SimWatchStatus.jl): a
  drop-in Julia writer that depends only on the standard library.
- [writers/simwatch.sh](writers/simwatch.sh): shell helpers to mark a job as
  queued right after `sbatch` (also for resubmissions, keeping the previous
  job's diagnostics), or as failed when it exits with an error.

## Trying it out

```bash
cargo run --example fake_sims -- /tmp/simwatch-demo &   # keeps updating a few fake runs
cargo run -- /tmp/simwatch-demo
```

[IDEAS.md](IDEAS.md) collects ideas for later; [CODE.md](CODE.md) describes the code internals.
