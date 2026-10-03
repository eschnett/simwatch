# Tests of the drop-in writer. Run with `julia writers/julia/runtests.jl`;
# needs only the standard library.

using Test, TOML
include(joinpath(@__DIR__, "SimWatchStatus.jl"))
using .SimWatchStatus
root = mktempdir()
read_doc(dir) = TOML.parsefile(joinpath(dir, "simwatch.toml"))
for v in ("SLURM_JOB_ID", "SLURM_PROCID", "SLURM_JOB_START_TIME", "SLURM_JOB_END_TIME",
          "SLURM_GPUS_ON_NODE", "CUDA_VISIBLE_DEVICES", "PMIX_RANK", "PMI_RANK", "OMPI_COMM_WORLD_RANK")
    delete!(ENV, v)
end

@testset "document" begin
    doc = simwatch_document(; name="x", message="a\nb", progress=(time=1.5f0, iteration=missing),
                            extra=Dict("progress" => Dict("chunk" => 3), "totals" => (1//2, 2.0f0, 3)),
                            summary=["progress.chunk"], group="g")
    @test doc["message"] == "a"
    @test doc["progress"] == Dict("time" => 1.5, "chunk" => 3)      # merged, missing dropped
    @test doc["totals"] == [0.5, 2.0, 3]
    @test doc["progress"]["time"] isa Float64
    @test doc["summary"] == ["progress.chunk"] && doc["group"] == "g"
    # A non-table `extra["progress"]` is replaced, not an error
    @test simwatch_document(; progress=(time=1,), extra=Dict("progress" => 5))["progress"] == Dict("time" => 1)
end

@testset "rate limit and interval" begin
    dir = joinpath(root, "rate")
    sw = SimWatchWriter(dir; interval=0.5, startup_interval=900)
    @test simwatch_update!(sw; status="starting", time=0.0)
    @test read_doc(dir)["update_interval"] == 900
    @test !simwatch_update!(sw; time=0.1)                    # within the minimum spacing
    sleep(1.2)
    @test simwatch_update!(sw; time=1.0)
    @test read_doc(dir)["update_interval"] ≥ 1              # 1.5 × the last spacing
    @test read_doc(dir)["progress"]["time_start"] == 0.0
    @test simwatch_finish!(sw; status="stopped", fraction=0.5)
    d = read_doc(dir)
    @test d["status"] == "stopped" && d["progress"]["fraction"] == 0.5
    @test !isfile(joinpath(dir, "simwatch.toml.tmp"))
end

@testset "never throws" begin
    f = joinpath(root, "afile"); mkpath(root); write(f, "x")
    @test write_simwatch(joinpath(f, "run"), Dict("a" => 1)) == false
    sw = SimWatchWriter(joinpath(f, "run"))
    @test (@test_logs (:warn,) simwatch_update!(sw; force=true)) == false
    @test (@test_logs simwatch_update!(sw; force=true)) == false      # warned once only
    dir = joinpath(root, "ro"); mkpath(dir); chmod(dir, 0o555)
    sw = SimWatchWriter(dir)
    @test (@test_logs (:warn,) simwatch_update!(sw; force=true)) == false
    chmod(dir, 0o755)
end

@testset "history" begin
    dir = joinpath(root, "hist")
    sw = SimWatchWriter(dir; history_length=3)
    for (i, t) in enumerate(1.0:5.0)
        h = Dict{String,Any}("a" => 0.1234567891 * t)
        i == 2 && (h["b"] = 7)
        simwatch_update!(sw; force=true, time=t, history=h)
    end
    hist = read_doc(dir)["history"]
    @test hist["time"] == [3.0, 4.0, 5.0]
    @test hist["a"] ≈ [0.370370, 0.493827, 0.617284] atol=1e-6
    @test !haskey(hist, "b")                                   # all NaN in the window: dropped
    simwatch_update!(sw; force=true, history=Dict("a" => 1.0))  # no time: no point
    @test read_doc(dir)["history"]["time"] == [3.0, 4.0, 5.0]
end

@testset "slurm environment and ranks" begin
    dir = joinpath(root, "slurm")
    withenv("SLURM_JOB_ID" => "777", "SLURM_JOB_START_TIME" => "1790000000",
            "SLURM_JOB_END_TIME" => "1790086400", "CUDA_VISIBLE_DEVICES" => "0,1") do
        sw = SimWatchWriter(dir)
        simwatch_update!(sw; force=true)
        d = read_doc(dir)
        @test d["slurm"]["job_id"] == "777"
        @test d["started"] == "2026-09-21T14:13:20Z"
        @test d["progress"]["walltime_limit"] == 86400
        @test d["progress"]["walltime"] > 1e5
        @test d["resources"]["gpus"] == 2
    end
    withenv("CUDA_VISIBLE_DEVICES" => "NoDevFiles", "SLURM_GPUS_ON_NODE" => nothing) do
        sw = SimWatchWriter(joinpath(root, "nogpu"))
        simwatch_update!(sw; force=true)
        @test !haskey(read_doc(joinpath(root, "nogpu"))["resources"], "gpus")
    end
    withenv("SLURM_PROCID" => "1") do
        sw = SimWatchWriter(joinpath(root, "rank1"))
        @test !simwatch_update!(sw; force=true)
        @test !isdir(joinpath(root, "rank1"))
        sw = SimWatchWriter(joinpath(root, "rank1"); active=true)
        @test simwatch_update!(sw; force=true)
    end
    withenv("OMPI_COMM_WORLD_RANK" => "0") do
        @test SimWatchWriter(joinpath(root, "r0")).active
    end
end

@testset "job lineage" begin
    dir = joinpath(root, "chain")
    withenv("SLURM_JOB_ID" => "100") do
        simwatch_update!(SimWatchWriter(dir); force=true)
        # The same job again (e.g. a second writer) has no earlier jobs
        simwatch_update!(SimWatchWriter(dir); force=true)
        @test !haskey(read_doc(dir)["slurm"], "previous_job_ids")
    end
    # Resubmitted as 200, which died before writing; then 300 runs
    d = read_doc(dir); d["slurm"]["next_job_id"] = "200"
    open(io -> TOML.print(io, d), joinpath(dir, "simwatch.toml"), "w")
    withenv("SLURM_JOB_ID" => "300") do
        simwatch_update!(SimWatchWriter(dir); force=true)
    end
    s = read_doc(dir)["slurm"]
    @test s["job_id"] == "300"
    @test s["previous_job_ids"] == ["100", "200"]
    @test !haskey(s, "next_job_id")
    withenv("SLURM_JOB_ID" => "400") do
        simwatch_update!(SimWatchWriter(dir); force=true)
    end
    @test read_doc(dir)["slurm"]["previous_job_ids"] == ["100", "200", "300"]
end
