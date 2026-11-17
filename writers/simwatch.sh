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
# because it crashed, or Slurm ended the job (scancel, time limit):
#
#     trap 'simwatch_terminated "$rundir"; exit 143' TERM
#     srun julia --project run.jl
#     rc=$?
#     [ $rc -eq 0 ] || simwatch_mark "$rundir" failed "exit code $rc on $(hostname)"
#     exit $rc
#
# Cancelling a simulation that is no longer needed (instead of a plain
# scancel, which leaves SimWatch to report it as lost):
#
#     simwatch_cancel "$rundir" "superseded by the run with N=48"

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

# The value of KEY in TABLE ("" for the top level) of FILE, without quotes
_simwatch_get() {
    [ -f "$1" ] || return 0
    awk -v want="$2" -v key="$3" '
        /^[[:space:]]*\[/ {
            sec = $0
            gsub(/^[[:space:]]*\[+[[:space:]]*|[[:space:]]*\]+.*$/, "", sec)
            next
        }
        sec == want && $0 ~ "^[[:space:]]*" key "[[:space:]]*=" {
            v = $0; sub(/^[^=]*=[[:space:]]*/, "", v); sub(/[[:space:]]*(#.*)?$/, "", v)
            gsub(/^"|"$/, "", v); print v; exit
        }
    ' "$1"
}

# The reported status of the simulation in DIR, in lower case
_simwatch_status() {
    _simwatch_get "$1/simwatch.toml" "" status | tr '[:upper:]' '[:lower:]'
}

# simwatch_cancel DIR [MESSAGE]
# Cancel the simulation in DIR because it is no longer needed: mark it
# `cancelled`, then scancel its job (the `next_job_id` it waits for, else
# `job_id`). Marking first tells the job's `simwatch_terminated` trap that
# the end was intended. Also marks a run whose job is already gone, such as
# one that SimWatch shows as lost.
simwatch_cancel() {
    local dir=$1 message=${2-} f="$1/simwatch.toml" job=
    if [ ! -f "$f" ]; then
        echo "simwatch_cancel: no $f" >&2
        return 1
    fi
    # As SimWatch decides which job a simulation waits for
    case $(_simwatch_status "$dir") in
        queued | pending | submitted | stopped | checkpointed | requeued | paused | suspended | \
            failed | error | crashed | aborted | killed)
            job=$(_simwatch_get "$f" slurm next_job_id) ;;
    esac
    [ -n "$job" ] || job=$(_simwatch_get "$f" slurm job_id)
    simwatch_mark "$dir" cancelled "${message:-${job:+job $job cancelled}}"
    [ -z "$job" ] || scancel "$job"
}

# simwatch_terminated DIR
# For a TERM trap in a batch script (see the top of this file). Slurm sends
# TERM when the job is cancelled, reaches its time limit or is preempted.
# Unless the simulation already reported its end (or `simwatch_cancel`
# did), mark it `failed` at the time limit and `cancelled` otherwise. A
# preempted and requeued job reports `running` again once it restarts.
simwatch_terminated() {
    local dir=$1 end=${SLURM_JOB_END_TIME-} host
    case $(_simwatch_status "$dir") in
        finished | done | completed | complete | success | succeeded | \
            failed | error | crashed | aborted | killed | cancelled | canceled | \
            stopped | checkpointed | requeued | paused | suspended)
            return 0 ;;
    esac
    host=$(hostname)
    # SLURM_JOB_END_TIME (Slurm 22.05 and later) is when the time limit is reached
    if [ -n "$end" ] && [ "$(date +%s)" -ge $((end - 60)) ]; then
        simwatch_mark "$dir" failed "killed at the time limit (job ${SLURM_JOB_ID-?} on $host)"
    else
        simwatch_mark "$dir" cancelled "ended by Slurm: scancel or preemption (job ${SLURM_JOB_ID-?} on $host)"
    fi
}
