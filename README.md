# Vibe

[![CI](https://github.com/accraze/vibe/actions/workflows/ci.yml/badge.svg)](https://github.com/accraze/vibe/actions/workflows/ci.yml)

An ambient micro-looper with reverb (CLAP / VST3), built with [nih-plug](https://github.com/robbert-vdh/nih-plug).

The input runs through a lush feedback-delay-network reverb, and a looper records the result.
Change the loop's speed (or play it backwards) while it plays, overdub new layers, and drop the
Clock to run the looper at a lower sample rate for longer, grittier loops.

| Control    | Range                                                                  |
|------------|------------------------------------------------------------------------|
| Wet        | Reverb level, 0–100%                                                   |
| Time       | Reverb decay, 0.3–20 s                                                 |
| Clock      | Looper sample rate, 1/8 to 1x the host rate (lower = longer and dirtier) |
| Loop       | Loop playback level, 0–100%                                            |
| Speed      | −2x to 2x (negative plays in reverse), glides like tape                |
| Clear      | Erases the loop                                                        |
| Record     | Tap to start the first take, tap again to close the loop. Later taps overdub. The LED is red while recording and green while a loop plays |
| Bypass     | The LED is lit when the effect is on. The loop and reverb keep running underneath |

The loop holds 20 s at full Clock, up to 160 s at the lowest. It's saved with the project, so it's
still there when you reopen it (a full 20 s loop adds about 10 MB to the project).

Knobs: drag or scroll to turn, hold Shift for fine control, double-click to reset.
Drag the bottom-right corner to resize the window.

## Build

Needs a recent Rust from [rustup](https://rustup.rs) (distro packages are usually too old).
The GUI (egui) also needs X11/GL headers:

```sh
sudo apt install libx11-xcb-dev libxcb1-dev libxcb-dri2-0-dev libxcb-icccm4-dev libxcursor-dev libxkbcommon-dev libxcb-shape0-dev libxcb-xfixes0-dev libgl-dev
```

```sh
cargo xtask bundle vibe --release
cargo test --lib
```

Bundles land in `target/bundled/`.

## Install (Linux)

Grab `vibe-<version>-linux.zip` from [Releases](https://github.com/accraze/vibe/releases) and unzip it, or build it yourself. Then:

```sh
cp target/bundled/Vibe.clap ~/.clap/
cp -r target/bundled/Vibe.vst3 ~/.vst3/   # optional
```

Then rescan plug-ins in your DAW.

## Releasing

Changes are listed in [CHANGELOG.md](CHANGELOG.md). See [docs/RELEASING.md](docs/RELEASING.md) for the process. In short: bump the version, push an annotated `v*` tag, and CI publishes the release.

## License

Vibe is licensed under the [GNU General Public License v3.0 or later](LICENSE).
nih-plug itself is ISC licensed, but its VST3 bindings are GPLv3, so VST3 builds must be GPL anyway.
