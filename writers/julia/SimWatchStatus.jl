"""
    SimWatchStatus

Write `simwatch.toml` status files that SimWatch displays. This is a single
drop-in file that depends only on the standard libraries `Dates` and `TOML`.
See `FORMAT.md` in the `simwatch` repository for the file format.

```julia
include("SimWatchStatus.jl")
using .SimWatchStatus

sw = SimWatchWriter(outdir; name="bbh-q1-d10", code="TreeGeneralizedHarmonic")
for it in 1:niters
    # ... evolve ...
    update!(sw; iteration=it, time=t, time_end=t_end, time_unit="M")  # writes at most once per minute
end
finish!(sw; message="reached t_end")
```
"""
module SimWatchStatus

using Dates: Dates, DateTime, UTC, now
using TOML: TOML

export SimWatchWriter, update!, finish!

const FILENAME = "simwatch.toml"

mutable struct SimWatchWriter
    dir::String
    name::String
    code::Union{Nothing,String}
    "seconds between regular writes"
    interval::Float64
    started::DateTime
    "`time()` when the writer was created"
    t0::Float64
    "`time()` of the last write"
    last_write::Float64
    "simulation time at the first update (for the average speed)"
    time_start::Union{Nothing,Float64}
end

"""
    SimWatchWriter(dir; name=basename(dir), code=nothing, interval=60)

Prepare to write `dir/simwatch.toml`. `interval` is the minimum number of
seconds between writes by [`update!`](@ref); SimWatch considers a simulation
stale when it has not written for about three intervals.
"""
function SimWatchWriter(dir::AbstractString; name::AbstractString=basename(abspath(dir)),
                        code::Union{Nothing,AbstractString}=nothing, interval::Real=60)
    mkpath(dir)
    return SimWatchWriter(String(dir), String(name), code === nothing ? nothing : String(code),
                          Float64(interval), now(UTC), time(), -Inf, nothing)
end

"""
    update!(sw::SimWatchWriter; force=false, status="running", kw...)::Bool

Write the status file, but only if `sw.interval` seconds have passed since the
last write or `force=true`. Returns whether the file was written. Cheap to call
often, e.g. after every step.

All keyword arguments are optional; `nothing` values are omitted:
- `status`: "starting", "running", "stopped", "finished", "failed"
- `message`: one line of text
- `iteration`, `time`, `time_end`, `time_unit`, `speed` (simulation time per
  wall-clock hour; by default SimWatch shows the average), `walltime_limit`
  (seconds), `checkpoint` (file name)
- `black_holes`: vector of named tuples or dicts with any of `name`, `mass`,
  `irreducible_mass`, `spin` (number or 3-vector), `position` (3-vector),
  `found` (Bool)
- `images`: vector of named tuples or dicts with `file` (relative to the
  simulation directory; PNG, at most 512×512 pixels), `title`, `description`
- `extra`: dict of problem-specific values or tables, e.g.
  `Dict("constraints" => Dict("ham_l2" => 1.2e-6))`. A value can carry a unit
  and a label: `Dict("value" => 50.2, "unit" => "M^2", "label" => "area")`.
"""
function update!(sw::SimWatchWriter; force::Bool=false, status::AbstractString="running",
                 message=nothing, iteration=nothing, time=nothing, time_end=nothing,
                 time_unit=nothing, speed=nothing, walltime_limit=nothing, checkpoint=nothing,
                 black_holes=nothing, images=nothing, extra=nothing)
    t = Base.time()
    if sw.time_start === nothing && time !== nothing
        sw.time_start = Float64(time)
    end
    force || t - sw.last_write >= sw.interval || return false
    sw.last_write = t

    doc = Dict{String,Any}()
    extra === nothing || merge!(doc, todict(extra))
    setkey!(doc, "name", sw.name)
    setkey!(doc, "status", status)
    setkey!(doc, "message", message === nothing ? nothing : first(split(string(message), '\n')))
    setkey!(doc, "updated", utc_string(now(UTC)))
    setkey!(doc, "started", utc_string(sw.started))
    setkey!(doc, "update_interval", sw.interval)
    setkey!(doc, "code", sw.code)
    setkey!(doc, "host", gethostname())
    setkey!(doc, "pid", getpid())

    progress = Dict{String,Any}()
    setkey!(progress, "iteration", iteration)
    setkey!(progress, "time", time)
    setkey!(progress, "time_start", time === nothing ? nothing : sw.time_start)
    setkey!(progress, "time_end", time_end)
    setkey!(progress, "time_unit", time_unit)
    setkey!(progress, "walltime", t - sw.t0)
    setkey!(progress, "walltime_limit", walltime_limit)
    setkey!(progress, "speed", speed)
    setkey!(progress, "checkpoint", checkpoint)
    doc["progress"] = progress

    resources = Dict{String,Any}()
    setkey!(resources, "nodes", envint("SLURM_JOB_NUM_NODES"))
    setkey!(resources, "tasks", envint("SLURM_NTASKS"))
    setkey!(resources, "threads", Threads.nthreads())
    setkey!(resources, "memory_bytes", current_rss())
    setkey!(resources, "memory_peak_bytes", Sys.maxrss())
    doc["resources"] = resources

    slurm = Dict{String,Any}()
    setkey!(slurm, "job_id", get(ENV, "SLURM_JOB_ID", nothing))
    setkey!(slurm, "job_name", get(ENV, "SLURM_JOB_NAME", nothing))
    setkey!(slurm, "partition", get(ENV, "SLURM_JOB_PARTITION", nothing))
    isempty(slurm) || (doc["slurm"] = slurm)

    black_holes === nothing || (doc["black_holes"] = [todict(b) for b in black_holes])
    images === nothing || (doc["images"] = [todict(i) for i in images])

    write_atomic(joinpath(sw.dir, FILENAME), doc)
    return true
end

"""
    finish!(sw::SimWatchWriter; status="finished", kw...)

Write the final status immediately. Use `status="failed"` with a `message`
after an error, or `status="stopped"` when stopping at a wall time limit.
"""
finish!(sw::SimWatchWriter; status::AbstractString="finished", kw...) =
    update!(sw; force=true, status, kw...)

setkey!(d::AbstractDict, k, ::Nothing) = d
setkey!(d::AbstractDict, k, v) = (d[k] = tomlvalue(v); d)

tomlvalue(x::Bool) = x
tomlvalue(x::Integer) = Int64(x)
tomlvalue(x::Real) = Float64(x)
tomlvalue(x::AbstractString) = String(x)
tomlvalue(x::Symbol) = String(x)
tomlvalue(x::DateTime) = utc_string(x)
tomlvalue(x::AbstractVector) = [tomlvalue(y) for y in x if y !== nothing]
tomlvalue(x::Tuple) = tomlvalue(collect(x))
tomlvalue(x::Union{NamedTuple,AbstractDict}) = todict(x)
tomlvalue(x) = string(x)

function todict(x::Union{NamedTuple,AbstractDict})
    d = Dict{String,Any}()
    for (k, v) in pairs(x)
        setkey!(d, string(k), v)
    end
    return d
end

"An unambiguous UTC time stamp; Julia's TOML writer cannot write offsets"
utc_string(t::DateTime) = Dates.format(t, Dates.dateformat"yyyy-mm-ddTHH:MM:SS") * "Z"

envint(name) = tryparse(Int, get(ENV, name, ""))

"Current resident memory in bytes (Linux only)"
function current_rss()
    Sys.islinux() || return nothing
    try
        pages = parse(Int, split(read("/proc/self/statm", String))[2])
        return pages * ccall(:getpagesize, Cint, ())
    catch
        return nothing
    end
end

"Replace the file atomically, so that readers never see a partial file"
function write_atomic(path::AbstractString, doc::AbstractDict)
    tmp = "$path.tmp"
    open(tmp, "w") do io
        TOML.print(io, doc; sorted=true)
    end
    # `rename` replaces the target atomically; `mv(; force=true)` would delete it first
    Base.Filesystem.rename(tmp, path)
    return nothing
end

end # module SimWatchStatus
