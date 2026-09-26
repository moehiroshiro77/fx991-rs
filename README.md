# fx991-rs

An emulator for the **Casio fx-991CN X** (VerF) scientific calculator, written in
Rust. It runs the calculator's real firmware.

English | [简体中文](README.zh.md)

[![CI](https://github.com/moehiroshiro77/fx991-rs/actions/workflows/ci.yml/badge.svg)](https://github.com/moehiroshiro77/fx991-rs/actions/workflows/ci.yml)

## What you need to supply

**The firmware is not included.** It is Casio's copyrighted ROM and is not
distributed with this repository. You need a dump of the fx-991CN X **VerF**:

| file | what | |
|---|---|---|
| `data/rom_verF.bin` | the ROM image | 262 144 bytes, MD5 `47bbf88fb3a9432b311b423f9b766e8f` |
| `data/skin.rgba` | the face texture | 307×615, 8-bit RGBA, no header — 755 220 bytes |
| `data/_disas_verF.txt` | optional disassembly listing | only needed by `emu disas --listing` |

All three are overridable: `--rom PATH`, `--skin PATH`, `--listing PATH`.

The debugger needs only the ROM: it decodes instructions from the bytes at an
address rather than reading the listing, so a patch, a byte written into RAM, or an
opcode the listing's generator never saw all show up.  The listing keeps a different
job -- the test suite renders all 116 943 of its instructions and requires the two to
agree, which is how the decoder is checked against an independently written one.

The skin is a bare pixel buffer, row-major RGBA at 307×615 with no header. Decode
the calculator's own manual image with any PNG library and write the pixels out;
the emulator checks the length and refuses a file of the wrong size rather than
rendering garbage.

Without these files the crate-local unit tests still pass — the integration tests
skip — so `cargo test` is green on a fresh clone.

## Build and run

Needs **Rust 1.90**.  That floor comes from the graphical front end — `wgpu` needs
1.87 and its `ordered-float` dependency needs 1.90 — and is declared once for the
whole workspace, so one `cargo build` covers every crate.

CI builds and tests on **nightly**, which is the toolchain the project is judged
by; a separate job checks the declared 1.90 floor still holds.

```bash
cargo build --release

target/release/emu calc "1+2*3"           # 7
target/release/emu key "1+2" --enter      # input area, then the answer
target/release/emu render face.png --keys "1+2" --enter
target/release/emu --help

target/release/fx991cnx                   # the clickable window
```

The window needs a GPU; the CLI does not.

## Debugger

`emu dbg` is a debugger in the shape of one for a native program: conditional
breakpoints, memory watchpoints, stepping, rollback snapshots, an instruction trace,
and reversible code patches.

```bash
target/release/emu dbg                            # interactive
target/release/emu dbg --script scripts/rop-trigger.dbg
```

```text
dbg> bp 0F968h if [filter] == 0xFF     # stop when the ROM arms its key filter
dbg> wp D180 w if r0 == 0x41           # watch a write, but only of that value
dbg> trace on 64
dbg> g 2000000
dbg> regs / stack / context
dbg> snap before / restore before / diff before now
dbg> patch 10000h POP PC               # ROP research: plant a gadget
dbg> tap SHIFT / tap 8                 # the unit-conversion menu, as a person opens it
dbg> keys SHIFT(-)4EXE                 # log10(4): SHIFT+(-) is the one-argument log
```

Three scripts come with the repository and each reproduces a finding from the
research notes on the real firmware:

| script | what it proves |
|---|---|
| `scripts/rop-trigger.dbg` | `SP = 0xD522 + n + 2`, and the first gadget address lands at `0xD530 + n` |
| `scripts/selftest-entry.dbg` | SHIFT + 7 held through an ON reset enters the self-test |
| `scripts/lbf-converter.dbg` | the `lbf/in²>kPa` character converter: `23h` terminates a history record |

Each script asserts what it demonstrates and exits non-zero when the assertion
fails, so they double as regression tests for the behaviours they cover.  Full
documentation, including the four design decisions worth knowing, is in
[`docs/debugger.md`](docs/debugger.md).

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
compositing in enough detail to check an implementation against. This project's
structure follows it: the crate split, the handler names (`OP_PUSH` → `op_push`)
and the screen constants (`N_ROW`, `ROW_SIZE`, `OFFSET`) all come from there,
which is why this repository is GPL-3.0 as well.

Where this implementation differs, and why:

| | CasioEmuNeo | here | why |
|---|---|---|---|
| clock | wall-clock driven | one instruction = one tick, paced separately | reproducible experiments; the front end adds real-time pacing on top |
| timer divider | driven off real time | a plain instruction counter | same behaviour, but deterministic |
| CW-II / `TIMER_SKIPPED` branches | present | omitted | only compiled for `HW_CLASSWIZ_II`, which the VerF firmware never reaches |
| RAM mirror at `0x49800` | present on the non-hardware model | not implemented | the VerF model reports `real_hardware = 1` |
| coprocessor `CRn` | implemented | raises `Unimplemented` | the firmware never executes one; failing loudly beats silently mis-executing |
| display font | sprites from the manual image | bit-per-pixel dot matrix | enough for debugging; the UI composes the real skin |

### Other references

All GPL-3.0, and none of them contributed code to this repository — they were
read for background on the machine and its instruction set.

| | |
|---|---|
| [991CN-X-CW-Decompilation](https://github.com/Physics365/991CN-X-CW-Decompilation) | ROP tutorial, assembler and an nX-U8 language module. Targets VerC, so its addresses do not transfer to VerF, but the method does |
| [ropide-vscode-plugin](https://github.com/Yaing-Yan/ropide-vscode-plugin) | ROP tooling with a built-in VerF preset table. Useful as a starting point; a few of its addresses are wrong |
| [fxesplus](https://github.com/qiufuyu123/fxesplus) | disassembler sources and an nX-U8 instruction table |
