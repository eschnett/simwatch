# SimWatch

SimWatch is a TUI (text user interface) tool to watch HPC simulations
progress. Simulations periodically (every minute?) output a status
file defining keyword-value pair. SimWatch displays these.

There are well-known keywords with specific meanings, specifying e.g.
the name of the simulation, wall time, simulation time, iteration,
speed, memory usage, number of nodes/GPUS/cores, etc. Simulation can
freely provide additional problem-specific keyword-value pairs to
describe their state, e.g. black hole location/mass/spin. SimWatch may
or may not understand these and display them in a special way.

SimWatch should find running simulations by itself, and pick up newly
started ones, starting from a (configurable?) list of directories.

There should be an overview page listing all simulations (maybe newest
first?), maybe another mode where each simulation is shown in a few
(5? 10?) lines, and a mode where all information about a simulation is
shown.

SimWatch would refresh its view periodically, maybe once every minute.

It would detect simulations which have finished (there is a keyword
for this) or which are stale, or which have crashed. Simulations can
post their Slurm job id, or maybe list shell commands that show
whether they are queued, have started, are running, or have finished.
However, talking to Slurm is slow -- we probably can't do this every
minute for all simulations. Maybe just `squeue --user USER` would be
better, examining the jobs' lines ourselves.

SimWatch would need to be robust, never scanning too many files, too
many directories, or calling Slurm too frequently. Errors in input
files are handled gracefully.

SimWatch is written in Julia. There are several packages that might be
useful. Tachikoma might be one (high level), or maybe just
ncurses/notcurses, or going with ANSI escapes directly. SimWatch runs
in a terminal and must work over ssh.

Simulations can also post small (check this!) images with a
description, e.g. a black hole track in a binary black hole merger, or
the time evolution of the constraint violation.

SimWatch does not keep any state about simulations, it only displays
what it finds in the simulations' directories. Ideally it looks at a
single file (`simwatch.SUFFIX`?) for a simulation, plus possible a few
(at most 10?) referenced other files, e.g. images.

SimWatch uses a simple file format (JSON? TOML? shell syntax?
Prometheus?).

In its first iteration, SimWatch monitors HPC simulations run by
TreeGeneralizedHarmonic on Symmetry. TreeGeneralizedHarmonic would be
updated to provide such output. This package SimWatch would provide a
small set of concrete, AI-readable instructions to create such output.
