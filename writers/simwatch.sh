# Shell helpers for SimWatch status files. Source this file from a submit
# script or a Slurm batch script:
#
#     . /path/to/simwatch/writers/simwatch.sh
#
# Submit side: create the run directory's status file right away, so that
# SimWatch shows the job while it is still queued:
#
#     jobid=$(sbatch --parsable job.sbatch)
#     simwatch_queued "$rundir" "$name" "$jobid"
#
# Batch script: record the outcome if the simulation itself could not, e.g.
# because it crashed or was killed:
#
#     julia --project run.jl
#     rc=$?
#     [ $rc -eq 0 ] || simwatch_mark "$rundir" failed "exit code $rc on $(hostname)"
#     exit $rc

# Escape a string for a TOML basic string
_simwatch_quote() {
    local s=${1//\\/\\\\}
    s=${s//\"/\\\"}
    printf '"%s"' "${s//$'\n'/ }"
}

_simwatch_now() {
    date -u +%Y-%m-%dT%H:%M:%SZ
}

# simwatch_queued DIR NAME JOBID
# Write a minimal status file for a job that has been submitted.
simwatch_queued() {
    local dir=$1 name=$2 jobid=$3
    local f="$dir/simwatch.toml"
    mkdir -p "$dir" || return
    {
        echo "name = $(_simwatch_quote "$name")"
        echo "status = \"queued\""
        echo "updated = $(_simwatch_now)"
        echo
        echo "[slurm]"
        echo "job_id = $(_simwatch_quote "$jobid")"
    } > "$f.tmp.$$" && mv -f "$f.tmp.$$" "$f"
}

# simwatch_mark DIR STATUS [MESSAGE]
# Set `status`, `message` and `updated`, keeping everything else the
# simulation wrote. Creates the file if there is none.
simwatch_mark() {
    local dir=$1 status=$2 message=${3-}
    local f="$dir/simwatch.toml"
    mkdir -p "$dir" || return
    local head
    head="status = $(_simwatch_quote "$status")"$'\n'"updated = $(_simwatch_now)"
    if [ -n "$message" ]; then
        head+=$'\n'"message = $(_simwatch_quote "$message")"
    fi
    # Top-level keys must come before the first [table]; drop the old values
    # of the keys we set, but only at the top level
    {
        printf '%s\n' "$head"
        if [ -f "$f" ]; then awk '
            /^[[:space:]]*\[/ { intable = 1 }
            !intable && /^[[:space:]]*(status|updated|message)[[:space:]]*=/ { next }
            { print }
        ' "$f"; fi
    } > "$f.tmp.$$" && mv -f "$f.tmp.$$" "$f"
}
