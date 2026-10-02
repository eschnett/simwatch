# The `simwatch.toml` status file

A simulation tells SimWatch about itself by writing one small file,
`simwatch.toml`, into its run directory, and rewriting it periodically
(about once a minute). SimWatch finds these files by searching the
directories it is told to watch, and shows what it finds.

SimWatch is an *opportunistic* viewer, not a strict protocol. Every key is
optional. Keys it knows are displayed specially; everything else is shown as
generic key-value pairs. A file with only `iteration = 5` in it is fine.

## Writing the file

- **Format:** [TOML](https://toml.io). Julia's standard library `TOML` can
  write it; see [`writers/julia/SimWatchStatus.jl`](writers/julia/SimWatchStatus.jl).
- **Location:** `<run directory>/simwatch.toml`. One file per simulation.
  Nested run directories (e.g. one per worker) are fine.
- **Replace it atomically:** write `simwatch.toml.tmp` in the same directory,
  then rename it to `simwatch.toml`. (In Julia: `Base.Filesystem.rename`,
  not `mv(; force=true)`, which deletes the target first.) SimWatch copes
  with a half-written file by showing the previous version, but atomic
  replacement avoids the glitch.
- **Cadence:** about once per minute, and set `update_interval` to the
  number of seconds between writes. SimWatch calls a running simulation
  *stale* when it has not written for 3 × `update_interval` (at least 3
  minutes). Write once more at the end, with the final `status`.
- **Size:** at most 64 KiB; larger files are ignored. Typical files are
  under 2 KiB.

## Well-known keys

All optional. Types are given for guidance; a value of an unexpected type is
displayed generically instead of being rejected.

### Top level

| Key | Type | Meaning |
|---|---|---|
| `name` | string | Simulation name. Without it SimWatch shows the directory name. |
| `status` | string | `queued`, `starting`, `running`, `stopped`, `finished`, `failed` (see below) |
| `updated` | time | When this file was written. Without it, the file's modification time is used. |
| `started` | time | When the simulation (or the current job) started |
| `update_interval` | number | Seconds between writes |
| `message` | string | One line describing the current state, e.g. the last error |
| `code` | string | Name of the simulation code |
| `host` | string | Host name, e.g. of the first compute node |
| `pid` | integer | Process id |

### `[progress]`

| Key | Type | Meaning |
|---|---|---|
| `iteration` | integer | Iteration or step number |
| `time` | number | Current simulation time |
| `time_end` | number | Final simulation time; SimWatch shows the percentage done and an ETA |
| `time_start` | number | Simulation time when the current job started (for the average speed after a restart) |
| `time_unit` | string | Unit of the simulation time, e.g. `"M"` |
| `walltime` | number | Seconds since the current job started |
| `walltime_limit` | number | Seconds the current job may run |
| `speed` | number | Simulation time per wall-clock **hour**. If missing, SimWatch shows the average `(time - time_start) / walltime`. |
| `speed_unit` | string | Display unit for `speed`; default `<time_unit>/h` |
| `checkpoint` | string | The most recent checkpoint file |

### `[resources]`

| Key | Type | Meaning |
|---|---|---|
| `nodes` | integer | Number of nodes |
| `tasks` | integer | Number of processes (MPI ranks) |
| `threads` | integer | Threads per process |
| `gpus` | integer | Number of GPUs |
| `memory_bytes` | number | Current memory use |
| `memory_peak_bytes` | number | Peak memory use |
| `memory_limit_bytes` | number | Available memory |

### `[slurm]`

| Key | Type | Meaning |
|---|---|---|
| `job_id` | string or integer | `$SLURM_JOB_ID`. SimWatch compares it with `squeue` to detect jobs that are queued, or that died without saying so. |
| `job_name` | string | `$SLURM_JOB_NAME` |
| `partition` | string | `$SLURM_JOB_PARTITION` |
| `next_job_id` | string or integer | The follow-up job when a job chain has been resubmitted |

### `[[black_holes]]`

An array of tables, one per black hole:

| Key | Type | Meaning |
|---|---|---|
| `name` | string | e.g. `"BH1"` |
| `mass` | number | (Christodoulou) mass |
| `irreducible_mass` | number | Irreducible mass |
| `spin` | number or `[x, y, z]` | Dimensionless spin χ |
| `position` | `[x, y, z]` | Coordinate position of the centre |
| `found` | bool | Whether the horizon was found this time |

### `[[images]]`

An array of tables, at most 10:

| Key | Type | Meaning |
|---|---|---|
| `file` | string | Path **relative to the run directory**, e.g. `"plots/track.png"`. Absolute paths and `..` are refused. |
| `title` | string | Short title |
| `description` | string | One or two sentences |

Images are thumbnails, shown as sixel graphics in the detail view. Use PNG
(JPEG works too). Keep them small: **at most 512×512 pixels, about 400×300 is
best**, with a dark background. SimWatch refuses files over 1 MiB or
1024×1024 pixels. Replace image files atomically too (write, then rename),
before rewriting `simwatch.toml`.

## Problem-specific keys

Any other key or table is shown in the detail view as flattened dotted keys,
e.g. `constraints.ham_l2`. Group related values in tables:

```toml
[constraints]
ham_l2 = 1.2e-6
mom_l2 = 8.4e-7
```

A value can carry a unit and a label by writing it as a small table with a
`value` key:

```toml
[horizon]
area = { value = 50.2, unit = "M^2", label = "Apparent horizon area" }
```

The well-known numeric keys accept this form too.

## Status values

The `status` a simulation reports is combined with the age of the file and
with Slurm's view of the job:

| `status` | Shown as |
|---|---|
| `queued` (also `pending`, `submitted`) | *queued*; *running* once Slurm starts the job; *lost* if the job vanished |
| `starting`, `running`, anything else, or missing | *running*; *stale* if not updated for 3 × `update_interval`; *queued* if Slurm says pending; *lost* if the Slurm job is gone |
| `stopped` (also `checkpointed`, `requeued`, `paused`) | *stopped*: ended on purpose and will continue, e.g. at a wall time limit |
| `finished` (also `done`, `completed`, `success`) | *finished* |
| `failed` (also `error`, `crashed`, `aborted`, `killed`) | *failed*; put the reason into `message` |

## Times

Write times as TOML offset date-times (`2026-10-02T15:35:00Z`), as RFC 3339
strings (`"2026-10-02T15:35:00Z"`), or as Unix seconds (`1790969700`). Prefer
UTC with a `Z`. A date-time without an offset is read in the viewer's local
time zone.

## Example

```toml
name = "bbh-q1-d10"
status = "running"
updated = 2026-10-02T15:35:00Z
started = 2026-10-02T12:00:00Z
update_interval = 60
message = "chunk 412: both horizons found"
code = "TreeGeneralizedHarmonic"
host = "cn042"

[progress]
iteration = 26368
time = 206.0
time_end = 1000.0
time_unit = "M"
walltime = 12900.0
walltime_limit = 86400.0

[resources]
nodes = 1
threads = 64
memory_bytes = 23.4e9

[slurm]
job_id = "1234567"
partition = "amdq"

[[black_holes]]
name = "BH1"
irreducible_mass = 0.4952
spin = [0.0, 0.0, 0.6]
position = [4.1, 2.3, 0.0]
found = true

[[black_holes]]
name = "BH2"
irreducible_mass = 0.4987
spin = [0.0, 0.0, -0.3]
position = [-4.1, -2.3, 0.0]
found = true

[[images]]
file = "plots/track.png"
title = "Black hole tracks"
description = "x-y plane; BH1 orange, BH2 cyan"

[constraints]
ham_l2 = { value = 1.2e-6, label = "Hamiltonian constraint, L2 norm" }
mom_l2 = 8.4e-7
```

## Instructions for AI agents

Follow these steps to add SimWatch output to a simulation code.

1. **Find the periodic hook.** Locate the place where the code already does
   something regularly during evolution: an observer or callback, the output
   or analysis step, or the end of the main loop body. The status file is
   written from there. It must not change the simulation's results.
2. **Choose the run directory.** Use the directory where the run's other
   output goes. Each concurrent run (or worker) needs its own directory.
3. **Write the file with a rate limit.** Write at most once per
   `update_interval` seconds (default 60), measured with a wall clock, plus
   once at the start (`status = "starting"` or `"running"`) and once at the
   end. Cost must be negligible: no extra global reductions just for the
   status file. In MPI codes, only rank 0 writes.
4. **Fill in what is already known.** At least `name`, `status`, `updated`,
   `update_interval`. Then `[progress]` (`iteration`, `time`, `time_end`,
   `time_unit`, `walltime`), `[slurm] job_id` from `$SLURM_JOB_ID`, and
   `[resources]`. Add physics diagnostics the code already computes (e.g.
   `[[black_holes]]`, constraint norms in a `[constraints]` table). Omit keys
   whose values are unknown; never write placeholders.
5. **Replace atomically.** Write to `simwatch.toml.tmp`, then rename it over
   `simwatch.toml`.
6. **Report the end.** On normal completion write `status = "finished"`. When
   stopping at a wall time limit for a restart, write `status = "stopped"`. On
   an error, catch it at the top level, write `status = "failed"` with the
   first line of the error in `message`, and rethrow. Errors while writing
   the status file must be caught and ignored (or logged once); they must
   never stop the simulation.
7. **Images (optional).** Only if the code already makes plots: save small
   PNG thumbnails (≤ 512×512, dark background) into the run directory, write
   them atomically, and list them under `[[images]]` with relative paths.
8. **Check.** Run a short simulation and look at it with
   `simwatch --print <dir>`; the run should appear with its name, state and
   progress.

In Julia, use the drop-in writer
[`writers/julia/SimWatchStatus.jl`](writers/julia/SimWatchStatus.jl), which
does steps 3–6. In shell scripts, [`writers/simwatch.sh`](writers/simwatch.sh)
provides `simwatch_queued` (call after `sbatch`) and `simwatch_mark` (e.g. to
mark a run as failed when the job script sees a non-zero exit code).

### Example: TreeGeneralizedHarmonic

`evolve!(...; observer)` calls `observer(p, t, u)` after each chunk:

```julia
include("SimWatchStatus.jl")
using .SimWatchStatus

sw = SimWatchWriter(outdir; name=label, code="TreeGeneralizedHarmonic")
nchunks = Ref(0)
observer = function (p, t, u)
    nchunks[] += 1
    try
        hz = latest_horizon_row()   # whatever the run script already computes
        bhs = hz === nothing ? nothing :
              [(name="BH", irreducible_mass=hz.M_irr, mass=hz.M_ch,
                spin=hz.J / hz.M_ch^2 .* hz.spin_axis, position=hz.center,
                found=hz.horizon_success)]
        update!(sw; time=Float64(t), time_end=Float64(t_end), time_unit="M",
                message="chunk $(nchunks[])", black_holes=bhs,
                extra=Dict("constraints" => Dict("ham_l2" => last_ham_l2())))
    catch err
        @warn "SimWatch status not written" err maxlog=1
    end
end

try
    r = evolve!(T, case; t_end, observer, max_walltime_seconds)
    finish!(sw; status=r.finished ? "finished" : "stopped", time=Float64(r.t),
            time_end=Float64(t_end), time_unit="M")
catch err
    finish!(sw; status="failed", message=sprint(showerror, err))
    rethrow()
end
```
