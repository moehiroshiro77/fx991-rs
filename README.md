# fx991-rs

An emulator for the **Casio fx-991CN X** (VerF) scientific calculator, written in
Rust. It runs the calculator's real firmware.

[![CI](https://github.com/moehiroshiro77/fx991-rs/actions/workflows/ci.yml/badge.svg)](https://github.com/moehiroshiro77/fx991-rs/actions/workflows/ci.yml)

## What you need to supply

**The firmware is not included.** It is Casio's copyrighted ROM and is not
distributed with this repository. You need a dump of the fx-991CN X **VerF**:

| file | what | |
|---|---|---|
| `data/rom_verF.bin` | the ROM image | 262 144 bytes, MD5 `47bbf88fb3a9432b311b423f9b766e8f` |
| `data/skin.rgba` | the face texture | 307×615, 8-bit RGBA, no header — 755 220 bytes |
| `data/_disas_verF.txt` | optional disassembly listing | only needed by `emu disas` |

All three are overridable: `--rom PATH`, `--skin PATH`, `--listing PATH`.

The skin is a bare pixel buffer, row-major RGBA at 307×615 with no header. Decode
the calculator's `interface.png` with any PNG library and write the pixels out;
the emulator checks the length and refuses a file of the wrong size rather than
rendering garbage.

Without these files the crate-local unit tests still pass — the integration tests
skip — so `cargo test` is green on a fresh clone.

## Build and run

```bash
cargo build --release

target/release/emu calc "1+2*3"           # 7
target/release/emu key "1+2" --enter      # input area, then the answer
target/release/emu render face.png --keys "1+2" --enter
target/release/emu --help

target/release/fx991cnx                   # the clickable window
```

The window needs a GPU; the CLI does not.

## License

**GNU General Public License v3.0** — see [`LICENSE`](LICENSE). Every crate in the
workspace is `GPL-3.0-only`; there is no per-crate exception.

This program comes with **absolutely no warranty**, to the extent permitted by
law. It is free software: you may redistribute it and/or modify it under the terms
of the GPL, and if you distribute a modified version you must make its source
available under the same licence.

By contributing, you agree that your contribution is licensed under the same
terms (GPL-3.0 section 5 — no separate agreement is needed).

**The firmware is not covered by this licence** and is not distributed here.
`data/` is in `.gitignore` so it cannot be committed by accident.

This project is not affiliated with or endorsed by Casio.

## Credits

Written from the **OKI nX-U8/100 Core Instruction Manual** (FEZ0317A0-U8-INST-02),
which specifies the instruction set, the flag behaviour and the addressing modes,
and from **observation of the firmware's own behaviour**. Where the manual is
ambiguous the behaviour was settled experimentally, and the source comments say
which.

### [CasioEmuNeo](https://github.com/qiufuyu123/CasioEmuNeo) — GPL-3.0

The most useful reference by far, and the reason this project was tractable. It
documents the peripheral map, the timer and interrupt model, and the display
compositing in enough detail to check an implementation against. Like this
project, it is GPL-3.0.

Where this emulator differs, and why:

| | CasioEmuNeo | here | why |
|---|---|---|---|
| clock | wall-clock driven | one instruction = one tick, paced separately | reproducible experiments; the front end adds real-time pacing on top |
| timer divider | driven off real time | a plain instruction counter | same behaviour, but deterministic |
| CW-II / `TIMER_SKIPPED` branches | present | omitted | only compiled for `HW_CLASSWIZ_II`, which the VerF firmware never reaches |
| RAM mirror at `0x49800` | present on the non-hardware model | not implemented | the VerF model reports `real_hardware = 1` |
| coprocessor `CRn` | implemented | raises `Unimplemented` | the firmware never executes one; failing loudly beats silently mis-executing |
| `PdValue` (`0xF050`) | registered only on the non-hardware model | not registered, reads as 0 | matches — the hole is in both |
| display font | sprites from `interface.png` | bit-per-pixel dot matrix | enough for debugging; the UI composes the real skin |

Four behaviours were established by experiment here, and they are the ones worth
knowing if you compare the two:

* **The timer divides twice.** An instruction counter runs every instruction, but
  the timer's own divider only ticks once per 10 000 instructions — one tick per
  209.7 instructions at 2 Mi/s. Treating `data_interval` as "every instruction"
  wedges the firmware in `STOP`, which looks like it ignoring input.
* **The LCD has three states, not two.** With the display off the screen is the
  panel colour and nothing is drawn at all. Lit dots are `(25,46,75)`; *unlit*
  dots are `(152,167,168)` — a light wash of ink, not the background. The blend
  rounds rather than truncates; truncating puts a channel at 151 instead of 152.
* **A reset is not a finger.** `ON` resets the machine. The reset clears the key
  matrix's drive state but does not lift a held key, which is exactly what the
  self-test needs: hold `SHIFT` and `7`, press `ON`, keep holding.
* **Several key names in the calculator's own layout file are wrong.** They are
  screen positions, not functions — `0x07` is `SHIFT` (not `F1`), `0x03` is `STO`
  (not `SHIFT`), `0x42` is `AC` (not `Space`). The table here was measured by
  pressing each key and reading what the firmware stored.

### Other references

All GPL-3.0, and none of them contributed code to this repository — they were
read for background on the machine and its instruction set.

| | |
|---|---|
| [991CN-X-CW-Decompilation](https://github.com/Physics365/991CN-X-CW-Decompilation) | ROP tutorial, assembler and an nX-U8 language module. Targets VerC, so its addresses do not transfer to VerF, but the method does |
| [ropide-vscode-plugin](https://github.com/Yaing-Yan/ropide-vscode-plugin) | ROP tooling with a built-in VerF preset table. Useful as a starting point; a few of its addresses are wrong |
| [fxesplus](https://github.com/qiufuyu123/fxesplus) | disassembler sources and an nX-U8 instruction table |
