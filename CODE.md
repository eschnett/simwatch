# SimWatch code internals

## Overview

SimWatch is a single binary with three kinds of threads:

```
            ┌──────────────── simwatch-fs ─────────────────┐
            │ scan roots (discover) → merge → reread files │──┐
            └──────────────────────────────────────────────┘  │ Update::*
            ┌──────────────── simwatch-slurm ──────────────┐  │ (mpsc)
            │ squeue every 120 s, killed after 20 s        │──┤
            └──────────────────────────────────────────────┘  ▼
  keys ──▶  ┌──────────────── main (UI) ───────────────────┐
            │ App state, derive health, draw with ratatui  │
            └──────────────────────────────────────────────┘
                     ▲  (dir, file)          │ decoded image
            ┌────────┴─────── simwatch-images ┴────────────┐
            │ confine path, check size, decode with limits │
            └──────────────────────────────────────────────┘
```

The UI thread never touches the file system or runs processes. Workers do
one job at a time and coalesce requests that arrive while they are busy, so
a hanging file system or Slurm cannot pile up threads.

## Modules

| File | Purpose |
|---|---|
| `src/main.rs` | CLI entry: load config, `--print` mode, choose the image picker, start workers, run the UI |
| `src/config.rs` | `Cli` (clap), `FileConfig` (TOML, `deny_unknown_fields`), merged `Config` with defaults |
| `src/format.rs` | Parse `simwatch.toml` into `Status` |
| `src/model.rs` | `Sim` (one directory), derived numbers, `Health` and `health()` |
| `src/discover.rs` | Bounded breadth-first directory scan; `confined_path` for image paths |
| `src/monitor.rs` | File system and Slurm worker threads, `Request`/`Update` messages, `collect_once` for `--print` |
| `src/slurm.rs` | Run and parse `squeue`; `Snapshot` with job lookup |
| `src/images.rs` | Image size limits and the image loader thread |
| `src/ui/mod.rs` | `App` state, key handling, header and status bar, the event loop `run` |
| `src/ui/list.rs` | List view: the `Col` enum, `columns()` (which columns to show), `cell()` (one cell's text), shared with the cards and `--print` (`text()`) |
| `src/ui/cards.rs` | Card view |
| `src/ui/detail.rs` | Detail view: all keys on the left, the current image on the right |
| `src/ui/help.rs` | Help overlay |
| `src/ui/fmt.rs` | Number, duration, byte and value formatting |

## Status file parsing (`format.rs`)

The file is parsed into a `toml::Table`. Well-known keys are then *taken out*
of the table with typed helpers (`take_string`, `take_number`, `take_int`,
`take_id`, `take_vector`, `take_time`, …). A helper removes a key only if
its value has a usable type, so a well-known key with an unexpected type
stays in the table. Partly consumed sub-tables (`[progress]`,
`[resources]`, `[slurm]`, black hole tables) are put back if anything is
left. Whatever remains is flattened into `Status::extra` as dotted keys
(`constraints.ham_l2`, `black_holes[1].color`).

Before anything is taken out, `[history]` and `summary` are removed and the
rest of the document is flattened once more into `Status::values`. That is
where `summary` keys are looked up, so they can name well-known keys too
(`progress.iteration`). `take_history` keeps arrays of numbers as series
(the dotted names of nested tables, `time` or `t` as the x axis) and returns
everything else as generic entries, which end up in `extra` as `history.…`.

Other details:
- Annotated values `{ value = …, unit = …, label = … }` are recognized by
  `is_annotated`. Typed helpers unwrap them (`plain`). `flatten` keeps the
  unit and label.
- Times (`as_time`) can be a TOML datetime, an RFC 3339 string, or Unix
  seconds. A datetime without an offset is read in local time. The Julia
  writer writes strings ending in `Z`, because Julia's TOML writer cannot
  write offsets.
- Limits: 128 KiB per file (`read_status_file` reads at most one byte more
  than that), 10 images, 1000 extra entries, 6 summary keys, 32 history
  series of at most 200 points (the last ones are kept).

## Simulations and health (`model.rs`)

A `Sim` holds the directory, the status file's mtime and size, the last
successfully parsed `Status`, and the latest error. A failed read or parse
sets `error` but keeps the previous `status`, so a half-written file causes
no flicker. `Sim::st()` returns an empty `Status` when nothing was parsed
yet, which keeps callers simple.

Derived values:
- `last_update` is `updated` if present, else the file mtime.
- `speed` is the reported `speed` if present. Otherwise it is the average
  `(time - time_start) / walltime` per hour, and is flagged as derived.
- `fraction` is `progress.fraction` if given, else `time / time_end`.
- `eta` uses the remaining simulation time and the speed when there is no
  `progress.fraction`, else the wall time so far scaled by the fraction left.
  It returns whether it comes from an average, shown as `~`.
- `summary()` resolves the `summary` keys in `values`, with their labels and
  any history series of the same name.
- `shown_job()` is the job the list shows: `next_job_id` while waiting for
  it, else `job_id`, with its number among the simulation's jobs.
- `growth_rate` (least-squares slope of ln y against t) and `trend` are free
  functions over history series.

`health()` combines three inputs:
1. the reported `status`, normalized to lowercase, with synonyms;
2. freshness: age ≤ max(`stale_factor` × `update_interval`, `stale_floor`);
3. the Slurm job, if both a job id and a snapshot exist. A job that is
   missing from a successful snapshot means *lost*. For a `queued`,
   `stopped` or `failed` simulation with a `next_job_id`, that job is
   checked instead of `job_id`: the simulation is waiting for it.

The order of the `Health` enum variants is the sort order for "sort by
state" (most urgent first). Keep that in mind when adding variants.

## Discovery (`discover.rs`)

`scan` is a breadth-first walk from the canonicalized roots, with a
`visited` set so that overlapping roots do not produce duplicates.
- It uses `DirEntry::file_type()`, which comes from `d_type` and does not
  follow symlinks. Symlinked directories are therefore never entered, which
  rules out loops. A symlinked `simwatch.toml` is accepted.
- Hidden directories and `skip_dirs` are skipped.
- Caps: `max_depth`, `max_dirs`, `max_entries_per_dir`, `max_sims`. Hitting
  a cap produces a warning in `ScanResult::warnings`, shown in the status
  bar. Hitting `max_depth` is normal and is not reported.
- Unreadable directories below a root are silently ignored (usually
  permissions). Only problems with the roots themselves are reported.

`confined_path` resolves an image path from a status file. It rejects
absolute paths and `..`, then canonicalizes and checks that the result is
still inside the canonical simulation directory, which also catches
symlinks that point outside.

## Workers (`monitor.rs`)

The file system worker (`fs_worker`) keeps the authoritative `Vec<Sim>`:
- **Full scan** every `scan_interval`, or on `Request::Rescan`.
  `merge_found` keeps known `Sim` records for directories that are still
  found, adds new ones, and drops the rest.
- **Re-read** every `refresh_interval`, after every scan, or on
  `Request::Reread`. `reread` `stat`s each known status file and reads it
  only when the mtime or size changed, or when the last attempt failed. A
  vanished file only sets `error`; the next scan drops the simulation. This
  keeps a non-atomic writer from making a simulation disappear for a whole
  scan interval.
- After each step it sends the whole list (`Update::Sims`). The lists are
  small (≤ 500 simulations), so cloning is cheap and keeps the UI simple.
- It waits with `recv_timeout` until the next deadline. Requests that queued
  up while it was busy are coalesced, with `Rescan` taking precedence.

The Slurm worker calls `slurm::query` immediately, then every
`squeue_interval`. `Request::Rescan` also triggers an early call, but never
sooner than 15 s after the previous one. If `squeue` does not exist, it sends
`SlurmDisabled` and exits.

`slurm::run_with_timeout` reads stdout and stderr on helper threads, so a
large output cannot block the child. It polls `try_wait` and kills the child
at the timeout. `Snapshot::find` matches job ids exactly first, then by base
id (`123_4`, `123_[1-5]` and `123+0` all have base `123`).

## UI (`ui/`)

`App` holds everything the UI shows. Worker messages are applied in
`App::apply`, and `Activity` records when each worker started and last
finished, for the spinners in the status bar.

**Selection.** The selection is stored as a directory (`App::selected`), not
as an index, so it survives re-sorting and new data. `visible()` computes
the filtered and sorted list of `(index, Health)` on every call. That is
cheap for hundreds of simulations, and keeps all state derived.

**Event loop** (`run`):
- Drain worker messages and finished images.
- Redraw if something changed, a key was pressed, or a tick passed. The
  tick is 100 ms while any worker is busy (to animate the spinners) and
  1 s otherwise (for the age column).
- Poll for keys with a short timeout.

ratatui's buffer diffing means that unchanged cells, including image cells,
are not sent again.

**Images.**
- `App::image()` returns the state of an image (Loading, Ready or Failed)
  and asks the loader thread for it the first time, or again when the
  simulation's status file mtime changed.
- Only images of the simulation in the detail view are kept; the cache is
  emptied when the detail view is left.
- A decoded image becomes a `StatefulProtocol` from a *clone* of the
  `Picker` whose background colour is set to the image's corner pixel.
  Images are padded to whole cells, and sixel has no transparency, so
  without this the padding shows up in some arbitrary palette colour.
- `Resize::Fit` scales images down to fit but never up.

**Clearing the screen.** Sixel pixels stay on the screen until something
erases them. Changing views, toggling help, switching images, a new image
arriving, and resizing all set `needs_clear`. The event loop then calls
`full_clear`, which erases the screen through the backend and calls
`Terminal::swap_buffers()`, so the next draw repaints every cell.

## Terminal pitfalls

These cost time to find; keep them in mind.

- **Never call `Terminal::clear()`.** It asks the terminal for the cursor
  position (`ESC[6n`) and blocks until the answer arrives: a round trip over
  ssh, and a fatal error if no answer comes. Use `full_clear`.
- **`Picker::from_query_stdio()` can eat input.** If the terminal does not
  answer, ratatui-image times out after 2 s but leaves its reader thread
  blocked on stdin. That thread later swallows a keystroke and turns raw
  mode off. `main::sixel_picker` therefore takes the cell size from
  `font_size` in the config, or else from the window-size ioctl (WezTerm
  reports pixel sizes and ssh forwards them). It queries the terminal only
  as a last resort.
- **ratatui-image's default features** include `chafa-dyn`, which needs the
  libchafa C library via pkg-config. Keep `default-features = false,
  features = ["crossterm"]`.
- ratatui-image deliberately avoids sixel on WezTerm when it detects it,
  preferring iTerm2's protocol. SimWatch sets `ProtocolType::Sixel`
  explicitly after creating the picker.

## Testing

`cargo test` runs about 30 unit tests, all in-process and fast:
- parsing edge cases;
- health states using a fixed clock;
- `squeue` parsing, the missing-binary case, and the timeout case;
- discovery limits on temporary directory trees, including symlink loops;
- image limits and path confinement;
- rendering of every view into ratatui's `TestBackend`;
- an end-to-end sixel test: load a PNG through the loader thread, render
  the detail view, and find `ESC P` in the buffer.

`writers/julia/runtests.jl` tests the Julia writer: the document, the rate
limit and `update_interval`, that it never throws (unwritable directories),
the history window, the Slurm environment and MPI ranks, and the job
lineage across `SLURM_JOB_ID`s. Run it with `julia writers/julia/runtests.jl`.

The demo generator `examples/fake_sims.rs` writes simulations in every
state, including a broken file, a file with only a couple of keys, nested
workers, and images that are too large or point outside the directory. It
keeps updating one binary black hole run with two plots.

To drive the real binary without a terminal, run it under a pseudo-terminal,
e.g. Python's `pty.fork()`. Set the window size *including pixel sizes*
with `TIOCSWINSZ`, so that the ioctl path is used instead of the terminal
query. Then send keys and look at the output. Sixel output can be checked by
cutting the `ESC P … ESC \` sequences out of the output and decoding them
with ImageMagick (`convert sixel:file.six file.png`).
