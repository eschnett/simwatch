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
- **Location:** `<run directory>/simwatch.toml`. One file per simulation
  (see [What is one simulation?](#what-is-one-simulation)). Nested run
  directories (e.g. one per worker) are fine.
- **Replace it atomically:** write `simwatch.toml.tmp` in the same directory,
  then rename it to `simwatch.toml`. (In Julia: `Base.Filesystem.rename`,
  not `mv(; force=true)`, which deletes the target first.) SimWatch copes
  with a half-written file by showing the previous version, but atomic
  replacement avoids the glitch.
- **Cadence:** about once per minute, and set `update_interval` to the
  number of seconds until the next write is *expected*. A code that can
  only write once per chunk, with chunks taking seven minutes, says so with
  a larger `update_interval`. SimWatch calls a running simulation *stale*
  when it has not written for 3 × `update_interval` (at least 3 minutes).
  Write once more at the end, with the final `status`.
- **Size:** at most 128 KiB; larger files are ignored. Typical files are a
  few KiB.

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
| `update_interval` | number | Expected seconds until the next write |
| `message` | string | One line describing the current state, e.g. the last error |
| `code` | string | Name of the simulation code |
| `host` | string | Host name, e.g. of the first compute node |
| `pid` | integer | Process id |
| `group` | string | Simulations with the same group belong together, e.g. the rows of a parameter study. SimWatch can sort and filter by it. |
| `summary` | array | The headline values, by key; see [Summary](#summary) |

### `[progress]`

| Key | Type | Meaning |
|---|---|---|
| `iteration` | integer | Iteration or step number |
| `time` | number | Current simulation time |
| `time_end` | number | Final simulation time; SimWatch shows the percentage done and an ETA |
| `time_start` | number | Simulation time when the current job started (for the average speed after a restart) |
| `time_unit` | string | Unit of the simulation time, e.g. `"M"` |
| `fraction` | number | Fraction done, 0 to 1, for runs whose progress is not simulation time: frames, sub-runs, stages. Takes precedence over `time / time_end`. |
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
| `job_id` | string or integer | `$SLURM_JOB_ID` of the job that wrote the file. SimWatch compares it with `squeue` to detect jobs that are queued, or that died without saying so. |
| `job_name` | string | `$SLURM_JOB_NAME` |
| `partition` | string | `$SLURM_JOB_PARTITION` |
| `previous_job_ids` | array | Earlier jobs of this simulation, oldest first |
| `next_job_id` | string or integer | A job submitted to continue this simulation, which has not written yet |

See [One simulation, several jobs](#one-simulation-several-jobs).

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
e.g. `constraints.ham_l2`, or `black_holes[1].mass` for the first table of an
array of tables. Group related values in tables:

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

## Summary

With many problem-specific keys, `summary` says which few matter most. It
lists keys in the dotted form above, at most 6; well-known keys work too:

```toml
summary = ["shells.r2.ham_l2", "horizon.M_irr", { key = "progress.iteration", label = "it" }]
```

SimWatch shows these values in a Summary column of the list, on the cards,
and at the top of the detail view. The label is the given `label`, else the
value's own label, else the last part of the key.

## History

SimWatch keeps no history of its own, so trends (a growth rate, say) must
come from the simulation. The `[history]` table holds a short window of
recent values: an optional `time` array and one array per series, all of
the same length and oldest first. Name each series like the key it tracks,
so that SimWatch can show it next to that value:

```toml
[history]
time = [190.0, 192.0, 194.0, 196.0]
"shells.r2.ham_l2" = [1.1e-6, 1.3e-6, 1.6e-6, 2.0e-6]
"horizon.M_irr" = [0.9452, 0.9452, 0.9451, 0.9451]
```

- Nested tables work as well as quoted keys:
  `[history.shells.r2]` with `ham_l2 = [...]` names the same series.
- Use `nan` for a missing value. Without `time`, points are equally spaced.
- Keep it small: **at most 8 series of at most 100 points**, with about 6
  significant digits. SimWatch reads at most 32 series and the last 200
  points of each.

SimWatch draws each series as a sparkline (on a log scale when it spans more
than two decades), and shows its exponential growth rate: the slope of
ln(value) against `time`.

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

With a `next_job_id`, a `queued`, `stopped` or `failed` simulation is judged
by that job instead: *queued* while it is pending, *running* once it runs,
*lost* if it vanished without writing.

## One simulation, several jobs

A simulation often runs as several Slurm jobs: a chain at the wall time
limit, a restart after a failure (perhaps with changed parameters), a
requeue after preemption. The status file belongs to the simulation and
stays in its run directory; `[slurm]` records the jobs:

```toml
[slurm]
job_id = "1234567"                          # the job that wrote this file
previous_job_ids = ["1234001", "1234350"]   # earlier jobs, oldest first
next_job_id = "1234602"                     # submitted, has not written yet
```

- **After submitting** a job for a simulation, call
  `simwatch_queued DIR NAME JOBID` from [`writers/simwatch.sh`](writers/simwatch.sh).
  For a new simulation it writes a minimal file. For an existing one it keeps
  everything the previous job wrote and sets `status = "queued"` and
  `next_job_id`. Call it after the previous job has ended; a job submitted
  ahead of time with `--dependency` can instead be passed to the Julia
  writer as `slurm=(next_job_id=…,)`.
- **When the job starts**, the Julia writer finds the existing file, and
  moves its `job_id` (and a `next_job_id` that never wrote, i.e. an attempt
  that died early) to `previous_job_ids`. Everything else is written afresh
  by the new job.
- **Changed parameters** are the simulation's own business: it writes its
  current parameters in its own tables, and can say in `message` what
  changed.

## What is one simulation?

- **One status file per simulation**, in its run directory. Several runs in
  one process (a convergence study at several resolutions, a benchmark
  sweep) write *one* file for the process, with `progress.fraction` for the
  progress and a `message` like `"N=32 (3 of 6)"`, rather than one file per
  short sub-run.
- **Separate jobs that belong together** (a parameter study) each write
  their own file, with a common `group`.
- **A copied run directory** (to continue a run with other parameters)
  starts out with the original's status file. Call
  `simwatch_queued COPY NEWNAME JOBID` when submitting the copy, so that it
  shows under its own name; the original's job then appears among its
  earlier jobs.

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
group = "bbh-study"
summary = ["constraints.ham_l2", "black_holes[1].irreducible_mass"]

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
previous_job_ids = ["1234001"]

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

[history]
time = [200.0, 202.0, 204.0, 206.0]
"constraints.ham_l2" = [1.0e-6, 1.05e-6, 1.12e-6, 1.2e-6]
```

## Instructions for AI agents

Follow these steps to add SimWatch output to a simulation code. In Julia,
use the drop-in writer
[`writers/julia/SimWatchStatus.jl`](writers/julia/SimWatchStatus.jl): it does
steps 3, 5 and 6 and fills in most of step 4 by itself. Copy it into the
code (e.g. `src/simwatch.jl`) and add the standard libraries `TOML` and
`Dates` to the package's `[deps]`. A code that keeps heavy dependencies out
of `src/` can still include it there, since it needs nothing else.

1. **Find the periodic hook.** Locate the place where the code already does
   something regularly during evolution: an observer or callback, the output
   or analysis step, a frame loop, or the end of the main loop body. The
   status file is written from there. It must not change the simulation's
   results. If the hook does not receive an iteration count or the step
   size, leave them out; `time` and `time_end` (or `fraction`) are enough.
2. **Choose the run directory and what one simulation is.** Use the
   directory where the run's other output goes. Each concurrent run (or
   worker) needs its own directory. Many short runs in one process write one
   file together (see [What is one simulation?](#what-is-one-simulation)).
3. **Write the file with a rate limit.** Write at most about once a minute,
   measured with a wall clock, plus once at the start (`status = "starting"`)
   and once at the end. Report in `update_interval` when the next write is
   expected. Cost must be negligible: no extra global reductions just for
   the status file. In MPI codes, compute collective diagnostics on all
   ranks as usual, and let only rank 0 write (the Julia writer's `active`
   does this).
4. **Fill in what is already known.** At least `name`, `status`, `updated`,
   `update_interval`. Then `[progress]` (`iteration`, `time`, `time_end`,
   `time_unit`, or `fraction`; `walltime`), `[slurm] job_id` from
   `$SLURM_JOB_ID`, and `[resources]`. Add the diagnostics the code already
   computes (e.g. `[[black_holes]]`, constraint norms in a `[constraints]`
   table, conserved totals). Convert them to plain numbers (`Float64`) and
   arrays. Omit keys whose values are unknown; never write placeholders such
   as `"missing"`. Choose 2 or 3 headline values for `summary`, put a
   `group` on runs that belong together, and record a few values that are
   watched for trends in `[history]`.
5. **Replace atomically.** Write to `simwatch.toml.tmp`, then rename it over
   `simwatch.toml`.
6. **Report the end, and never fail the run.** On normal completion write
   `status = "finished"`. When stopping at a wall time limit for a restart,
   write `status = "stopped"`. On an error, catch it at the top level, write
   `status = "failed"` with the first line of the error in `message`, and
   rethrow. Errors while writing the status file must be caught and
   reported at most once; they must never stop the simulation.
7. **Resubmissions.** If the code's job scripts resubmit or restart runs,
   call `simwatch_queued` after `sbatch` (see
   [One simulation, several jobs](#one-simulation-several-jobs)), and
   `simwatch_mark DIR failed "…"` when the job script sees a non-zero exit
   code.
8. **Images (optional).** Only if the code already makes plots: save small
   PNG thumbnails (≤ 512×512, dark background) into the run directory, write
   them atomically, and list them under `[[images]]` with relative paths.
9. **Check.** Run a short simulation and look at it with
   `simwatch --print <dir>`; the run should appear with its name, state and
   progress.

### Example: TreeGeneralizedHarmonic

`evolve!(...; observer)` calls `observer(p, t, u, row)` after each chunk,
where `row` is the chunk's record with the horizon's numbers:

```julia
include("simwatch.jl")      # a copy of writers/julia/SimWatchStatus.jl
using .SimWatchStatus

sw = SimWatchWriter(outdir; name=label, code="TreeGeneralizedHarmonic", group="bbh-study",
                    summary=["constraints.ham_l2", "black_holes[1].irreducible_mass"])
simwatch_update!(sw; force=true, status="starting", time_end=Float64(t_end), time_unit="M",
                 message="compiling and building the initial data")
steps = Ref(0)

function observer(p, t, u, row)
    steps[] += row.steps
    ham = constraint_norms(p).ham_l2
    bhs = row.horizon_success === nothing ? nothing :
          [(name="BH", irreducible_mass=row.M_irr, mass=row.M_ch,
            spin=row.J / row.M_ch^2 .* row.spin_axis, position=row.origin,
            found=row.horizon_success)]
    simwatch_update!(sw; iteration=steps[], time=t, time_end=t_end, time_unit="M",
                     black_holes=bhs, extra=Dict("constraints" => Dict("ham_l2" => ham)),
                     history=Dict("constraints.ham_l2" => ham))
    return nothing
end

try
    r = evolve!(T, case; t_end, observer, max_walltime_seconds)
    simwatch_finish!(sw; status=r.finished ? "finished" : "stopped", iteration=steps[],
                     time=r.t, time_end=t_end, time_unit="M")
catch err
    simwatch_finish!(sw; status="failed", message=sprint(showerror, err))
    rethrow()
end
```

### Example: a frame loop or a study in one process

```julia
sw = SimWatchWriter(outdir; name="kh-showcase", code="TreeHydro")
for k in 1:nframes
    advance_to_frame!(state, k)
    simwatch_update!(sw; fraction=k / nframes, message="frame $k of $nframes",
                     extra=Dict("totals" => Tuple(conserved_totals(state))))
end
simwatch_finish!(sw)
```
