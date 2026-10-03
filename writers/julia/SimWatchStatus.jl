"""
    SimWatchStatus

Write `simwatch.toml` status files that SimWatch displays. A single drop-in
file that depends only on the standard libraries `Dates` and `TOML` (a
package that includes it must list both in its `[deps]`). See `FORMAT.md` in
the `simwatch` repository for the file format.

Three layers, each usable alone:

- `simwatch_document(; …)` builds the document as a `Dict`; no side effects.
- `write_simwatch(dir, doc)` writes it atomically and never throws.
- `SimWatchWriter` carries what a run needs between calls: its start, the
  rate limit, the spacing of its calls (from which `update_interval` is
  reported), a short history of chosen values, and its earlier Slurm jobs.
  `simwatch_update!` and `simwatch_finish!` never throw either.

A whole run:

```julia
include("SimWatchStatus.jl")
using .SimWatchStatus

sw = SimWatchWriter(outdir; name="bbh-q1-d10", code="TreeGeneralizedHarmonic",
                    group="bbh-study", summary=["constraints.ham_l2"])
simwatch_update!(sw; force=true, status="starting", message="building initial data")

# Called after every step or chunk; writes at most once per minute. In MPI
# runs every rank may call it (after computing collective diagnostics); only
# rank 0 writes.
observer = function (p, t, u)
    ham = constraint_norm(p)
    simwatch_update!(sw; time=t, time_end=t_end, time_unit="M",
                     extra=Dict("constraints" => Dict("ham_l2" => ham)),
                     history=Dict("constraints.ham_l2" => ham))
end

try
    r = evolve!(…; observer)
    # A run stopped at its wall time limit will be continued: "stopped"
    simwatch_finish!(sw; status=r.finished ? "finished" : "stopped", time=r.t,
                     time_end=t_end, time_unit="M")
catch err
    simwatch_finish!(sw; status="failed", message=sprint(showerror, err))
    rethrow()
end
```

Runs whose progress is not simulation time (frames, the stages of a
convergence study) pass `fraction=k/n` instead of `time`/`time_end`.
"""
module SimWatchStatus

using Dates: Dates, DateTime, UTC, now
using TOML: TOML

export SimWatchWriter, simwatch_update!, simwatch_finish!, simwatch_document, write_simwatch

const FILENAME = "simwatch.toml"
"Status files larger than this are not read back (as in SimWatch)"
const MAX_BYTES = 128 * 1024
"At most this many earlier jobs are remembered"
const MAX_PREVIOUS_JOBS = 50

"""
    simwatch_document(; name, status, message, updated, started,
                      update_interval, code, host, pid, group, summary,
                      progress, resources, slurm, black_holes, images,
                      history, extra) -> Dict{String,Any}

The contents of a `simwatch.toml`, ready for `TOML.print`. Every keyword is
optional, and `nothing` and `missing` are omitted:

- top level: `name`, `status` (`"starting"`, `"running"`, `"stopped"`,
  `"finished"`, `"failed"`), `message` (its first line), `updated` and
  `started` (`DateTime`s in UTC), `update_interval` (expected seconds until
  the next write), `code`, `host`, `pid`, `group` (simulations that belong
  together), `summary` (keys of the headline values, e.g.
  `["constraints.ham_l2"]`);
- `progress`, `resources`, `slurm`: `NamedTuple`s or `Dict`s of the keys of
  those tables (`iteration`, `time`, `time_end`, `time_unit`, `fraction`,
  `walltime`, …; `nodes`, `threads`, `gpus`, …; `job_id`,
  `previous_job_ids`, `next_job_id`, …);
- `black_holes`, `images`: vectors of `NamedTuple`s or `Dict`s;
- `history`: a `Dict` of equally long vectors, `"time"` and one per series;
- `extra`: any further keys and tables. A table named like a known one
  (`progress`, `resources`, `slurm`) extends it.

Any `Real` is written as a `Float64` (or `Int64`), tuples as arrays.
"""
function simwatch_document(; name=nothing, status=nothing, message=nothing,
                           updated=nothing, started=nothing, update_interval=nothing,
                           code=nothing, host=nothing, pid=nothing, group=nothing,
                           summary=nothing, progress=nothing, resources=nothing,
                           slurm=nothing, black_holes=nothing, images=nothing,
                           history=nothing, extra=nothing)
    doc = Dict{String,Any}()
    extra === nothing || merge!(doc, todict(extra))
    setkey!(doc, "name", name)
    setkey!(doc, "status", status)
    setkey!(doc, "message", message === nothing ? nothing :
                            first(split(string(message), '\n')))
    setkey!(doc, "updated", updated)
    setkey!(doc, "started", started)
    setkey!(doc, "update_interval", update_interval)
    setkey!(doc, "code", code)
    setkey!(doc, "host", host)
    setkey!(doc, "pid", pid)
    setkey!(doc, "group", group)
    setkey!(doc, "summary", summary)
    for (key, table) in (("progress", progress), ("resources", resources), ("slurm", slurm))
        table === nothing && continue
        old = get(doc, key, nothing)
        t = old isa AbstractDict ? merge!(old, todict(table)) : todict(table)
        isempty(t) || (doc[key] = t)
    end
    black_holes === nothing || (doc["black_holes"] = Any[todict(b) for b in black_holes])
    images === nothing || (doc["images"] = Any[todict(i) for i in images])
    history === nothing || isempty(history) || (doc["history"] = todict(history))
    return doc
end

"""
    write_simwatch(dir, doc) -> Bool

Write `doc` to `dir/simwatch.toml` atomically: to `simwatch.toml.tmp`, then
renamed over it, so that a reader never sees half a file. Returns whether it
worked. **Never throws**: a status file must not stop a run.
"""
function write_simwatch(dir::AbstractString, doc::AbstractDict)
    path = joinpath(dir, FILENAME)
    tmp = path * ".tmp"
    try
        mkpath(dir)
        open(tmp, "w") do io
            TOML.print(io, doc; sorted=true)
        end
        # `rename` replaces the target atomically; `mv(; force=true)` would
        # delete it first
        Base.Filesystem.rename(tmp, path)
        return true
    catch err
        err isa InterruptException && rethrow()
        try
            rm(tmp; force=true)
        catch
        end
        return false
    end
end

"""
    SimWatchWriter(dir; name=basename(dir), code=nothing, group=nothing,
                   summary=nothing, interval=60, startup_interval=900,
                   history_length=100, active=<rank 0>)

The status file of one run in `dir`, and the state its writes need.

- `interval`: the least number of seconds between two writes by
  [`simwatch_update!`](@ref).
- `startup_interval`: the `update_interval` reported before the second call,
  which has to cover loading and compilation.
- `history_length`: how many points of each `history` series are kept.
- `active`: whether this process writes at all. By default only rank 0 does,
  judged from `SLURM_PROCID`, `PMIX_RANK`, `PMI_RANK` or
  `OMPI_COMM_WORLD_RANK`; pass `active = MPI.Comm_rank(comm) == 0` to be
  explicit. An inactive writer does nothing and returns `false`.

The run's start is the Slurm job's start if Slurm says, else now. If `dir`
already has a status file from another Slurm job, that job (and the jobs
before it) are recorded as this simulation's `previous_job_ids`.
"""
mutable struct SimWatchWriter
    dir::String
    name::String
    code::Union{Nothing,String}
    group::Union{Nothing,String}
    summary::Any
    interval::Float64
    startup_interval::Float64
    active::Bool
    started::DateTime
    t0::Float64                  # `time()` at the start
    last_write::Float64          # `time()` of the last write
    last_call::Float64           # `time()` of the last call
    gap::Float64                 # the spacing of the last two calls
    time_start::Union{Nothing,Float64}
    history_length::Int
    history_time::Vector{Float64}
    history::Dict{String,Vector{Float64}}
    previous_job_ids::Vector{String}
    warned::Bool
end

function SimWatchWriter(dir::AbstractString; name::AbstractString=basename(abspath(dir)),
                        code=nothing, group=nothing, summary=nothing, interval::Real=60,
                        startup_interval::Real=900, history_length::Integer=100,
                        active::Bool=is_rank0())
    t = time()
    job = envfloat("SLURM_JOB_START_TIME")
    t0 = job === nothing ? t : job
    previous = active ? previous_jobs(dir, get(ENV, "SLURM_JOB_ID", nothing)) : String[]
    return SimWatchWriter(String(dir), String(name), maybe_string(code), maybe_string(group),
                          summary, Float64(interval), Float64(startup_interval), active,
                          Dates.unix2datetime(t0), t0, -Inf, -Inf, NaN, nothing,
                          Int(history_length), Float64[], Dict{String,Vector{Float64}}(),
                          previous, false)
end

"""
    simwatch_update!(sw::SimWatchWriter; force=false, status="running",
                     message, iteration, time, time_end, time_unit, fraction,
                     speed, walltime_limit, checkpoint, progress, resources,
                     slurm, black_holes, images, history, extra) -> Bool

Write the status file if `sw.interval` seconds have passed since the last
write, or if `force`; return whether it was written. Cheap to call after
every step or chunk.

- Every call is timed. The reported `update_interval` is
  `max(interval, 1.5 × the last spacing of calls)`, or `startup_interval`
  before there is one, so that a run that calls every seven minutes is not
  shown as stale after three.
- `fraction` (0 to 1) is the progress of runs that are not measured in
  simulation time.
- `history = Dict("constraints.ham_l2" => x, …)` adds one point per written
  update, at the current `time`; SimWatch shows the recent values as
  sparklines. Name a series like the key it tracks.
- `progress`, `resources`, `slurm` add keys to those tables.
- `walltime_limit` and `gpus` come from Slurm unless given.
- **Never throws** (except `InterruptException`): a failed write warns once
  and returns `false`.
"""
function simwatch_update!(sw::SimWatchWriter; force::Bool=false,
                          status::AbstractString="running", message=nothing,
                          iteration=nothing, time=nothing, time_end=nothing,
                          time_unit=nothing, fraction=nothing, speed=nothing,
                          walltime_limit=nothing, checkpoint=nothing, progress=nothing,
                          resources=nothing, slurm=nothing, black_holes=nothing,
                          images=nothing, history=nothing, extra=nothing)
    sw.active || return false
    try
        t = Base.time()
        isfinite(sw.last_call) && (sw.gap = t - sw.last_call)
        sw.last_call = t
        if sw.time_start === nothing && time !== nothing
            sw.time_start = Float64(time)
        end
        force || t - sw.last_write ≥ sw.interval || return false
        sw.last_write = t
        time === nothing || history === nothing || record_history!(sw, Float64(time), history)

        expected = isnan(sw.gap) ? max(sw.interval, sw.startup_interval) :
                   max(sw.interval, 3 * sw.gap / 2)
        limit = walltime_limit !== nothing ? walltime_limit : slurm_walltime_limit()
        prog = Dict{String,Any}()
        for (k, v) in (("iteration", iteration), ("time", time),
                       ("time_start", time === nothing ? nothing : sw.time_start),
                       ("time_end", time_end), ("time_unit", time_unit),
                       ("fraction", fraction), ("walltime", t - sw.t0),
                       ("walltime_limit", limit), ("speed", speed),
                       ("checkpoint", checkpoint))
            setkey!(prog, k, v)
        end
        progress === nothing || merge!(prog, todict(progress))
        res = Dict{String,Any}()
        for (k, v) in (("nodes", envint("SLURM_JOB_NUM_NODES")),
                       ("tasks", envint("SLURM_NTASKS")),
                       ("threads", Threads.nthreads()), ("gpus", gpus()),
                       ("memory_bytes", current_rss()),
                       ("memory_peak_bytes", Sys.maxrss()))
            setkey!(res, k, v)
        end
        resources === nothing || merge!(res, todict(resources))
        sl = Dict{String,Any}()
        for (k, v) in (("job_id", get(ENV, "SLURM_JOB_ID", nothing)),
                       ("job_name", get(ENV, "SLURM_JOB_NAME", nothing)),
                       ("partition", get(ENV, "SLURM_JOB_PARTITION", nothing)),
                       ("previous_job_ids",
                        isempty(sw.previous_job_ids) ? nothing : sw.previous_job_ids))
            setkey!(sl, k, v)
        end
        slurm === nothing || merge!(sl, todict(slurm))
        doc = simwatch_document(; name=sw.name, status=status, message=message,
                                updated=now(UTC), started=sw.started,
                                update_interval=round(expected), code=sw.code,
                                host=gethostname(), pid=getpid(), group=sw.group,
                                summary=sw.summary, progress=prog, resources=res,
                                slurm=isempty(sl) ? nothing : sl,
                                black_holes=black_holes, images=images,
                                history=history_document(sw), extra=extra)
        ok = write_simwatch(sw.dir, doc)
        ok || warn_once(sw, "could not write $(joinpath(sw.dir, FILENAME))")
        return ok
    catch err
        err isa InterruptException && rethrow()
        warn_once(sw, sprint(showerror, err))
        return false
    end
end

"""
    simwatch_finish!(sw::SimWatchWriter; status="finished", kw...) -> Bool

Write the final status now: `"finished"`, `"stopped"` (at a wall time limit,
to be continued) or `"failed"` with the error as `message`. Takes the
keywords of [`simwatch_update!`](@ref).
"""
simwatch_finish!(sw::SimWatchWriter; status::AbstractString="finished", kw...) =
    simwatch_update!(sw; force=true, status=status, kw...)

function warn_once(sw::SimWatchWriter, what)
    sw.warned && return nothing
    sw.warned = true
    @warn "SimWatch: $what; the run continues, further problems are not reported"
    return nothing
end

# --- history -----------------------------------------------------------------

function record_history!(sw::SimWatchWriter, t::Float64, vals)
    n = length(sw.history_time)
    push!(sw.history_time, t)
    for (k, v) in pairs(vals)
        key = string(k)
        x = v isa Real ? round(Float64(v); sigdigits=6) : NaN
        series = get!(() -> fill(NaN, n), sw.history, key)
        push!(series, x)
    end
    # Series not given this time are padded
    for series in values(sw.history)
        length(series) == n && push!(series, NaN)
    end
    drop = length(sw.history_time) - sw.history_length
    if drop > 0
        deleteat!(sw.history_time, 1:drop)
        foreach(s -> deleteat!(s, 1:drop), values(sw.history))
    end
    filter!(((_, s),) -> any(isfinite, s), sw.history)
    return nothing
end

function history_document(sw::SimWatchWriter)
    isempty(sw.history) && return nothing
    doc = Dict{String,Any}("time" => copy(sw.history_time))
    for (k, s) in sw.history
        doc[k] = copy(s)
    end
    return doc
end

# --- TOML values -------------------------------------------------------------

setkey!(d::AbstractDict, k, ::Nothing) = d
setkey!(d::AbstractDict, k, ::Missing) = d
setkey!(d::AbstractDict, k, v) = (d[k] = tomlvalue(v); d)

tomlvalue(x::Bool) = x
tomlvalue(x::Integer) = Int64(x)
tomlvalue(x::Real) = Float64(x)       # `nan` and `±inf` are valid TOML
tomlvalue(x::AbstractString) = String(x)
tomlvalue(x::Symbol) = String(x)
# Julia's TOML writer cannot write offsets, so times are RFC 3339 strings in UTC
tomlvalue(x::DateTime) = Dates.format(x, Dates.dateformat"yyyy-mm-ddTHH:MM:SS") * "Z"
tomlvalue(x::Union{AbstractVector,Tuple}) =
    Any[tomlvalue(y) for y in x if y !== nothing && y !== missing]
tomlvalue(x::Union{NamedTuple,AbstractDict}) = todict(x)
tomlvalue(x) = string(x)

function todict(x::Union{NamedTuple,AbstractDict})
    d = Dict{String,Any}()
    for (k, v) in pairs(x)
        setkey!(d, string(k), v)
    end
    return d
end

maybe_string(x) = x === nothing ? nothing : String(x)

# --- what the environment says ----------------------------------------------

envint(name) = tryparse(Int, get(ENV, name, ""))
envfloat(name) = tryparse(Float64, get(ENV, name, ""))

"Rank 0, or not an MPI or multi-task Slurm run"
function is_rank0()
    for name in ("SLURM_PROCID", "PMIX_RANK", "PMI_RANK", "OMPI_COMM_WORLD_RANK")
        r = envint(name)
        r === nothing || return r == 0
    end
    return true
end

"The Slurm job's wall time limit in seconds, where Slurm says (22.05 and later)"
function slurm_walltime_limit()
    s, e = envfloat("SLURM_JOB_START_TIME"), envfloat("SLURM_JOB_END_TIME")
    return s === nothing || e === nothing ? nothing : e - s
end

"The GPUs the job was given: Slurm's count, else the visible CUDA devices"
function gpus()
    n = envint("SLURM_GPUS_ON_NODE")
    n === nothing || return n
    v = strip(get(ENV, "CUDA_VISIBLE_DEVICES", ""))
    (isempty(v) || v == "NoDevFiles" || v == "-1") && return nothing
    return count(==(','), v) + 1
end

"The resident memory in bytes (Linux only)"
function current_rss()
    Sys.islinux() || return nothing
    try
        pages = parse(Int, split(read("/proc/self/statm", String))[2])
        return pages * ccall(:getpagesize, Cint, ())
    catch
        return nothing
    end
end

"""
The earlier Slurm jobs of the simulation in `dir`: those its existing status
file lists, then the job that wrote it, then a next job that never wrote
(an attempt that died early). Empty if the file is from the current job.
"""
function previous_jobs(dir, current)
    try
        path = joinpath(dir, FILENAME)
        isfile(path) && filesize(path) ≤ MAX_BYTES || return String[]
        slurm = get(TOML.parsefile(path), "slurm", nothing)
        slurm isa AbstractDict || return String[]
        id(x) = x isa Union{AbstractString,Integer} ? strip(string(x)) : nothing
        old = id(get(slurm, "job_id", nothing))
        old == current && return String[]
        jobs = String[]
        prev = get(slurm, "previous_job_ids", [])
        prev isa AbstractVector && append!(jobs, filter(!isnothing, id.(prev)))
        old === nothing || push!(jobs, old)
        next = id(get(slurm, "next_job_id", nothing))
        next === nothing || next == current || push!(jobs, next)
        jobs = unique(filter(!=(current), jobs))
        return jobs[max(1, end - MAX_PREVIOUS_JOBS + 1):end]
    catch
        return String[]
    end
end

end # module SimWatchStatus
