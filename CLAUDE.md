# Vibe

Ambient micro-looper with reverb plugin (CLAP/VST3) in Rust using nih-plug. Single crate, all DSP in `src/lib.rs`.
The user runs it in Bitwig Studio on Linux (CLAP). Scaffolding, CI and editor widgets are copied from `../rodent`.

## Commands

```sh
cargo xtask bundle vibe --release && cp -r target/bundled/Vibe.clap ~/.clap/   # build + install
cargo test --release --lib -- --nocapture                                       # DSP sanity test
cargo clippy --release --lib
```

- The system has an old distro Rust (1.75) in `/usr/bin` that comes before `~/.cargo/bin` on PATH and
  can't build nih-plug. Prefix commands with `PATH="$HOME/.cargo/bin:$PATH"`.
- The GUI build needs X11/GL dev packages (listed in README); a missing `x11-xcb` pkg-config error means they're not installed.
- Bitwig needs the plugin re-added or a restart to pick up a new build.
- Add user-facing changes to `CHANGELOG.md` under `[Unreleased]` as you make them.
- Releases: follow `docs/RELEASING.md` (CI publishes on annotated `v*` tags; never move an existing tag).
- The "trailing semicolon in macro" warnings come from nih-plug's export macros, not our code.

## DSP structure

- Tuning values are named constants at the top of `lib.rs`.
- Per frame: `Reverb::process` (stereo FDN, 8 lines through a fast Walsh-Hadamard mix, one-pole damping in the
  loop) is added to the dry input, the looper records that sum, and the output is that sum plus the loop. Mono
  runs through the same stereo engine with both sides equal.
- `Looper` has one head that plays and records, like tape. The first take advances by `rate` (from Clock, via
  `clock_rate()`) and sets `len` on release; later Record presses overdub every cell the head enters, in either
  direction, decaying the old layer by `OVERDUB_DECAY`. `close()` fades `SEAM_FADE` cells at both ends.
  Cells are written sample-and-hold with no anti-alias filter on purpose (low Clock grit).
- Reverb and loop buffers are allocated in `initialize()`, because `reset()` can run on the audio thread.
- `Shared` (an `Arc` shared with the editor) carries the Clear request and whether a loop exists (for the LED).
  The loop audio isn't part of the plugin state, so it's lost when the project closes.
- GUI lives in `src/editor.rs`: a hand-painted pedal. `knob()`, `screw()`, `footswitch()` and window scaling
  come from Rodent (see `../rodent/CLAUDE.md`). Drag or mouse wheel to turn (2%/notch), Shift for fine,
  double-click resets.
- The audio thread must not allocate (`assert_process_allocs` is enabled).
- `CLAP_ID` and `VST3_CLASS_ID` must never change, or saved Bitwig projects lose the plugin.
