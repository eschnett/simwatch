# Ideas for later

Collected while designing SimWatch. None of these are implemented.

## Images without sixel

SimWatch currently shows images only as sixel graphics. Text-based
fallbacks would work in every terminal, inside tmux, and over any ssh link,
at lower resolution. Each uses 24-bit foreground and background colours, so
each cell can show two colours:

| Characters | Pixels per cell | Notes |
|---|---|---|
| Half blocks `▀` `▄` | 1 × 2 | Works everywhere; ratatui-image has it built in (`ProtocolType::Halfblocks`) |
| Quadrants `▘▝▖▗…` | 2 × 2 | Unicode 1.0 block elements |
| Sextants `🬀…🬻` | 2 × 3 | Unicode 13, Symbols for Legacy Computing |
| Octants | 2 × 4 | Unicode 16, Symbols for Legacy Computing Supplement; needs recent fonts, but WezTerm draws block glyphs itself |
| Braille `⠁…⣿` | 2 × 4 | One colour per cell; good for line plots |

## Plotting data instead of images

The `[history]` table already carries short time series, shown as one-line
sparklines. SimWatch could also draw them as proper plots, with axes and
several series, using Braille characters (like UnicodePlots.jl) at whatever
size fits the terminal. A black hole track could come the same way, as
`x` and `y` series. This is sharper than a scaled-down image and needs no
graphics protocol.

## Other graphics protocols

- **Kitty graphics protocol:** supported by kitty, Ghostty and WezTerm. The
  image is transmitted once and can then be placed repeatedly, which saves
  bandwidth on redraws.
- **iTerm2 inline images:** supported by iTerm2 and WezTerm. ratatui-image
  prefers this over sixel on WezTerm.
- **Auto-detection:** ratatui-image's `Picker::from_query_stdio()` can choose
  a protocol, with the user able to override it.

## Terminal multiplexers and compatibility

- tmux 3.4 and later can pass sixel through when built with sixel support.
  For the kitty protocol tmux needs `set -g allow-passthrough on`.
- GNU screen generally breaks inline images.
- macOS Terminal.app supports no image protocol at all.
- kitty and Ghostty do not support sixel; they need the kitty protocol.
- Konsole's sixel implementation is reportedly buggy.

## Slurm alternatives

GOALS.md suggested that simulations could list shell commands that report
whether they are queued, running or finished. SimWatch uses a single
`squeue --user=$USER` call instead, which scales to many simulations. Probe
commands could still be useful for other schedulers or for jobs on other
machines, run rarely and with a timeout.

`sacct` could tell how a job ended (completed, failed, timeout, out of
memory, node failure) once it is gone from `squeue`. It is slower, so it
would be called only for jobs that just disappeared.

## Other implementation languages

- **Julia:** the natural choice next to Julia simulation codes. Start-up and
  first-render latency need PrecompileTools workloads and a Pkg app,
  sysimage, or `juliac` binary. The Julia TUI libraries are less mature, and
  none handles image placement, so sixel output would need manual cell
  reservation.
- **Go with Bubble Tea:** a single static binary, and goroutines suit slow
  file systems. Bubble Tea's string-based renderer does not know about
  images, so sixel output must be written outside it, with fragile cell
  bookkeeping.
- **Rust with ratatui** (chosen): ratatui-image reserves cells for images and
  redraws them correctly.

## Small things

- Mouse support: click to select, scroll wheel.
- Open the run directory or the log file in `$PAGER`.
- Show group headers in the list, or collapse a group to one line.
- Group simulations by root directory when they have no `group`.
