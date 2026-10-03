# Shell helpers for SimWatch status files. Source this file from a submit
# script or a Slurm batch script:
#
#     . /path/to/simwatch/writers/simwatch.sh
#
# Submit side: record the job right away, so that SimWatch shows it while it
# is queued. This also works for resubmissions of an existing simulation:
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
# Record that a job has been submitted for the simulation in DIR.
# - Without a status file, write a minimal one.
# - With one (a resubmission: a chain, or a restart after a failure, or a
#   copied run directory), keep everything the simulation wrote, set `status`,
#   `updated`, `message` and (if NAME is not empty) `name`, and set
#   `next_job_id` in [slurm]. A `next_job_id` that never started moves to
#   `previous_job_ids`. The job itself records the rest when it starts.
# This expects the `[slurm]` table form and one-line arrays, as Julia's
# TOML.print writes them. Call it after the previous job has ended.
simwatch_queued() {
    local dir=$1 name=$2 jobid=$3
    local f="$dir/simwatch.toml"
    mkdir -p "$dir" || return
    if [ ! -s "$f" ]; then
        {
            echo "name = $(_simwatch_quote "$name")"
            echo "status = \"queued\""
            echo "updated = $(_simwatch_now)"
            echo
            echo "[slurm]"
            echo "job_id = $(_simwatch_quote "$jobid")"
        } > "$f.tmp.$$" && mv -f "$f.tmp.$$" "$f"
        return
    fi
    local head keys="status|updated|message"
    head="status = \"queued\""$'\n'"updated = $(_simwatch_now)"
    head+=$'\n'"message = $(_simwatch_quote "resubmitted as job $jobid")"
    if [ -n "$name" ]; then
        head+=$'\n'"name = $(_simwatch_quote "$name")"
        keys+="|name"
    fi
    # The header goes through the environment: `awk -v` cannot pass newlines everywhere
    SIMWATCH_HEAD=$head awk -v keys="$keys" -v nj="$(_simwatch_quote "$jobid")" '
        function unquote(v) { gsub(/^[[:space:]"]+|[[:space:]"]+$/, "", v); return v }
        function value(line) { sub(/^[^=]*=[[:space:]]*/, "", line); sub(/[[:space:]]*(#.*)?$/, "", line); return line }
        # The new [slurm] keys, at the end of that table
        function slurm_keys(   list) {
            if (done) return
            list = prev
            if (oldnext != "" && unquote(oldnext) != unquote(nj))
                list = (list == "" ? "" : list ", ") oldnext
            if (list != "") print "previous_job_ids = [" list "]"
            print "next_job_id = " nj
            done = 1
        }
        BEGIN { print ENVIRON["SIMWATCH_HEAD"]; re = "^[[:space:]]*(" keys ")[[:space:]]*=" }
        /^[[:space:]]*\[/ {
            if (sec == "slurm") slurm_keys()
            sec = $0
            gsub(/^[[:space:]]*\[+[[:space:]]*|[[:space:]]*\]+.*$/, "", sec)
        }
        sec == "" && $0 ~ re { next }
        sec == "slurm" && /^[[:space:]]*next_job_id[[:space:]]*=/ { oldnext = value($0); next }
        sec == "slurm" && /^[[:space:]]*previous_job_ids[[:space:]]*=/ {
            prev = value($0); sub(/^\[[[:space:]]*/, "", prev); sub(/[[:space:]]*,?[[:space:]]*\]$/, "", prev)
            next
        }
        { print }
        END {
            if (sec == "slurm") slurm_keys()
            else if (!done) { print ""; print "[slurm]"; slurm_keys() }
        }
    ' "$f" > "$f.tmp.$$" && mv -f "$f.tmp.$$" "$f"
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
