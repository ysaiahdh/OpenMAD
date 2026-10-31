# OpenMAD

[![Logo](assets/logo.png)](https://openmad.accw.dev)

[![Static Badge](https://img.shields.io/badge/License-GNU%20GPLv3-green)](https://creativecommons.org/licenses/by-sa/4.0/?ref=chooser-v1)
[![Rust](https://img.shields.io/badge/Rust-1.75+-DEA584?logo=rust&logoColor=white)](https://www.rust-lang.org/)
[![Platforms](https://img.shields.io/badge/Platform-Windows-lightgrey)]()

A native, lightweight Windows driver and utility for **MAD68 HE** magnetic-switch keyboards, replacing the
web-only configurator at `hub.fgg.com.cn`.

It does everything the stock tool does, plus the things a web page can't: named
profiles stored as files, zero-latency telemetry overlay HUD, and automatic profile switching based on which
application is in the foreground.

## Disclaimer

This project was done using generative AI to make reverse-engineering the manufacturer's read/write process easier. However, that doesn't mean OpenMAD is a vibe coding project.
I have spent quite a few hours gathering a good understanding of how the stock software handled everything to ensure my project is safe to use and won't brick anyone's board.
That being said, if you have a different keyboard which uses Madlion's web driver and would like to collaborate, don't hesitate to contact me to make OpenMAD better for everyone.

---

## Install

Download `OpenMAD-Setup-x.y.z.exe` from the releases page and run it. It installs
per-user and never asks for admin rights (the keyboard is reached over raw HID,
which Windows exposes to ordinary user processes).

The app lives in the system tray. Right-click it for the quick menu, **Open** launches
the configurator in your browser.

## What it does

**Per-key settings:** actuation point, rapid trigger, dead zone, and the
advanced key modes: Dynamic Key Stroke, Hold/Click, Rappy Snappy, SOCD.

**Lighting:** all 21 effects the firmware supports, per-key colours, brightness
and speed, plus a colour-calibration panel (the LEDs are not colour-balanced
from the factory).

**Keymap and macros:** four layers, 16 macro slots with a recorder.

**Profiles:** stored as JSON in your data folder. One can be written to the
keyboard's onboard memory. The rest are applied by the driver on demand.

**App switching:** bind one or more executables to a profile and the driver
switches when that app takes focus. One profile can be marked the default, used
whenever nothing else matches.

---

## How this was reverse-engineered

The stock configurator is a Vue app protected with `javascript-obfuscator`. No
firmware was disassembled and no traffic was intercepted. Everything came from
reading the shipped JavaScript and confirming each finding against the hardware.

**1. Recover the sources.** The bundle uses the string-array transform: a
rotating array, an offset decoder, and per-scope aliases. The decoder does no
encryption (it is an indexed lookup) so once the array is rotated into its
correct order every call can be resolved statically and the literal inlined.
That is what `research/deobfuscate.js` does, after Prettier re-expands the
minified source.

**2. Find the right code.** The app code-splits per device family. The device
table maps USB VID/PID to a controller class. The MAD68 HE's (`0x373B:0x1058`, "MAD 68
RGB") binds to the `DuckBread` controller. The lighting UI turned out to live in a lazily-loaded chunk
that `index.html` never references, only a dynamic import does.

**3. Read the packet classes.** Each request is a small class with typed
accessors over a `DataView`, which gives exact byte offsets. Cross-referencing
the enums (`Mu` for VIA commands, `Bu` for vendor sub-commands, `Eu` for the
flash operation) reconstructs the wire format without guessing.

**4. Confirm against the hardware.** Every claim was checked by writing to the
keyboard and reading it back. This is what caught the things the source alone
would not tell you:

- brightness above 210 is silently dropped while the rest of the same packet
  still applies, so the write looks successful and changes nothing,
- `read_light_info` returns the colour *after* a gamma curve, so a read never
  matches the write,
- writing advanced keys re-applies actuation to the two keys they bind, so the
  write order in a profile matters.

---

## Not damaging the keyboard

This was the design constraint from the start, and it shapes the architecture.

**Automatic switching never touches flash.** The vendor's protocol has a
`flashOp` field. `NORMAL` applies values immediately without committing them while
`ERASE_AND_WRITE` persists. Profile application uses `NORMAL`, so switching
profiles on every alt-tab costs no write endurance. Only two actions ever
commit: "Save to keyboard" and "Save profile onboard".

**Key bindings are never applied automatically.** Actuation, rapid trigger and
lighting all ride the vendor channel and can be applied with `flashOp` set to
`NORMAL`, which does not touch persistent memory. Key bindings do not: they go
through VIA's dynamic-keymap command, which has no volatile mode, so every
write is an EEPROM write. Each profile stores and can edit its own key
bindings, but switching to it never writes them, since that would spend real
write endurance every time the foreground window changed. Applying them is a
deliberate action, either from Change Key Setting or the tray.

**Reads are free.** The live telemetry the gauge uses is read-only.

**Everything is reversible.** `openmad-rs backup` snapshots the full onboard state
to JSON and `openmad-rs restore` writes it back. Take a backup before your first
write.

**The dangerous commands are gated.** Calibration, factory reset, EEPROM reset
and bootloader jump are refused by the transport unless a caller explicitly opts
in, and the UI additionally requires a typed confirmation. Firmware flashing is
not implemented at all.

**Factory reset is a keyboard operation, not an OpenMAD one.** It wipes the
board's own onboard memory (calibration, whatever profile was saved to it) and
nothing else. It will not fix a crashed tray app, a stuck configurator, or a
corrupted profile file. Those live on your PC, not the keyboard, and a factory
reset cannot touch them.

---

## Building from source (Rust native)

Requires Rust 1.75+ with the MSVC toolchain (`x86_64-pc-windows-msvc`) on Windows.

```bash
git clone <repo> && cd OpenMAD
cargo build --release
```

The resulting standalone executable `target\release\openmad-rs.exe` has zero Python runtime dependencies and embeds all required branding and icon resources.

### Running & CLI subcommands

When launched without arguments (or double-clicked from File Explorer), OpenMAD starts silently in the Windows system tray.

You can also run specific tools directly from PowerShell or CMD:

```bash
# Launch the tray application with auto-switching (default mode)
target\release\openmad-rs.exe tray
target\release\openmad-rs.exe tray --dry-run        # log decisions without hardware writes

# Launch the live telemetry HUD / web dashboard (default: http://127.0.0.1:8787/)
target\release\openmad-rs.exe hud
target\release\openmad-rs.exe hud --overlay         # includes transparent on-top Win32 HUD

# Hardware backup / restore
target\release\openmad-rs.exe backup --out backup.json
target\release\openmad-rs.exe backup --verify backup.json
target\release\openmad-rs.exe restore backup.json              # simulation (dry-run)
target\release\openmad-rs.exe restore backup.json --apply      # write changes to keyboard
```

### Building the installer

Install [Inno Setup](https://jrsoftware.org/isinfo.php) and run:

```bash
cargo build --release
iscc packaging/mad68.iss     # -> packaging/Output/OpenMAD-Setup-<version>.exe
```

The installer adds Start-menu and optional desktop shortcuts, and offers to
start the app at sign-in.

### Releasing

The version and repository live in one file, `version.ini` at the repository
root. The app reads it at runtime and build time, and the installer script reads it with the Inno preprocessor, so there
is nothing to keep in sync.

```ini
[app]
name = OpenMAD
version = 1.1.4
repo = Acciaw/OpenMAD
```

To cut a release: bump `version`, rebuild both artifacts, then tag the commit
with the same number prefixed by `v` and publish a GitHub release with the
binaries attached.

```bash
git tag v1.1.4 && git push origin v1.1.4
```

The GitHub Actions workflow automatically builds the release binary, builds the installer, attests provenance, and publishes the release.

---

## Repository layout

| | |
|---|---|
| `src/` | Native Rust core: protocol, device HID, profiles, switcher, HUD, tray, updater, CLI |
| `packaging/` | Inno Setup script (`mad68.iss`) |
| `assets/` | logo, icon and embedded HUD dashboard |
| `research/` | protocol notes and the deobfuscation tool |
| `profiles/` | profile JSON |

---

## Compatibility

Confirmed on hardware, reading and writing:

| Board | USB | Matrix |
| --- | --- | --- |
| MAD68 HE RGB | `0x373B:0x1058` | 5 × 15, 68 keys |
| MAD60 HE | `0x373B:0x1053` | 5 × 14, 61 keys |

The `DuckBread` controller backs 25 boards in the same family — see
[`src/devices.rs`](src/devices.rs) for the full list, taken from
the vendor's own registry. They share the packet layouts, vendor sub-commands
and flash semantics; only the matrix geometry differs, and the driver reads
that from the board it is talking to rather than assuming one.

A board that is not on the confirmed list opens **read-only**. It can be
inspected, diagnosed and reported on safely, but nothing can write to it until
its firmware reports a protocol version this driver was built against *and*
someone has actually run it on that model.

Take a backup first.

Not affiliated with, endorsed by, or supported by the manufacturer.
