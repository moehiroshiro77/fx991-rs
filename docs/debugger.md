# The debugger

`emu dbg` is a debugger for the fx-991CN X, in the shape of one for a native
program: breakpoints with conditions, memory watchpoints, stepping, rollback
snapshots, an instruction trace, and reversible code patches.  It is driven from a
command line, so a session can be typed or recorded in a script.

```sh
emu dbg                                      # interactive
emu dbg --script scripts/rop-trigger.dbg     # scripted, stops at the first failure
emu dbg --boot 500000                        # run half a second of boot first
```

## Why it decodes memory rather than reading a listing

`data/_disas_verF.txt` is *derived* data: `tools/u8dis/` generated it from a
declarative instruction chart.  It is an excellent reference for a static pass over
the ROM, but a debugger cannot use it, because a file on disk cannot show:

* a byte that a patch changed,
* code written into RAM,
* a `load_code` overlay,
* an opcode the generator never saw.

So the disassembler decodes the bytes actually present at an address.  The listing
still has a job -- it is an independently produced oracle, and
`crates/nxu8-asm/tests/listing_oracle.rs` renders all **116 943** of its
instructions from the ROM and requires the two texts to be identical.  Two
implementations that share no code agreeing on every instruction in the image is a
much stronger statement than either one testing itself.

## Commands

```
stepping
  s [n]              step n instructions (default 1)
  n / over           step over a call
  out                run until the current call returns
  g [BUDGET]         run until a breakpoint, watch, or the budget
  r ADDR             run to an address
  t N                run N ticks, ignoring breakpoints
  runmode run        leave STOP/HALT so instructions run again
breakpoints and watches
  bp ADDR [if EXPR] [once] [log]     set a breakpoint; `log` records without stopping
  bl / bc / be N / bd N              list, clear, enable, disable
  wp ADDR [r|w|rw|x] [if EXPR]       watch memory (default rw)
  wl / wc                            list, clear
registers and memory
  regs / get REG / set REG VALUE / sp VALUE
  m ADDR [LEN] / poke ADDR BYTES / stack [LEN] / fill ADDR LEN [BYTE]
views
  context            summary, disassembly around the PC, and registers
  disas [ADDR] [N]   disassemble from memory
  screen / periph / ints / port
patches
  patch ADDR INSTRUCTION   e.g. `patch 10000h POP PC`
  patches / undo ADDR / undoall
snapshots and trace
  snap NAME / restore NAME / snaps / diff A B / diffnow NAME
  trace on [N] / trace off / trace show [N] / trace save PATH
keys and misc
  tap NAME [SETTLE] / keys 1+2EXE   press a key and release it once the ROM takes
                                    it; SETTLE=0 runs nothing after, so a breakpoint
                                    can catch what the key causes
  latch SHIFT [SHIFT ...]           toggle keys down without a finger, and lift them
                                    again on a second `latch`
  fill ADDR LEN [BYTE] / assert EXPR / echo TEXT
  # comment / help / q
```

### Keys

`tap` waits for the ROM's **accept edge** (`rom:0F968`), not a tick count.  That is
the same signal `Calculator::tap_code` waits on, and it matters: the ROM only takes
a key while its input filter (`0xF042`) is armed, and a tick-count approximation
silently drops keys -- which is how `SHIFT 8` used to type an `8` instead of opening
the unit menu.

A modifier is a **state**, not a chord, so `tap SHIFT` followed by `tap 8` is enough;
nothing has to be held.  The one case that does need a key held is the self-test,
where SHIFT and 7 must stay down through an ON reset: `latch SHIFT 7` sets the
keyboard's own latch (the same flag the window's right-click sets), and it survives
both a release and a reset.  `latch` again lifts it.

**`SETTLE = 0` is what a script needs when a key causes something worth watching.**
The tap still waits for the accept edge, then runs nothing, so a breakpoint armed on
what the key *does* -- evaluating, copying the input area, running a ROP chain --
still fires.  With the default settle, all of that is stepped over first.

Key names come from the measured table in `crates/fx991-model/src/keys.rs`, which
carries `//   SHIFT ->` and `//   ALPHA ->` notes under each entry saying what the
key becomes with a modifier held.

A name is a **prefix match**, so a modifier and its key are written joined up:
`tap SHIFT(-)` presses SHIFT and then the `(-)` key, which types the single-argument
`log`.  There is no `SHIFT+key` spelling, and `+` is itself a key name.

**Two keys are called `log` and they are not the same key.**  `log(a,b)` is the
two-argument template (`7D 1A 19 1C 19 1B`), and both boxes must be filled or the
answer is an undefined value rather than a default -- `emu calc "log(4)"` returns
`0` for exactly this reason.  The single-argument one, which is what a person means
by `lg`, is `SHIFT` + `(-)`, and it writes a bare `7D`:

```sh
emu dbg ... ; keys SHIFT(-)4EXE       # log10(4) = 0.602059991327962
```

Inside a two-box template `RIGHT` moves to the second box, and inside a fraction
(`0x05`, `C8 1D .. 1A 1B`) it is `DOWN` that moves to the denominator.

The short mnemonics (`s`, `t`, `g`, `m`, `sp`, `q`) come from the Python prototype
this project's notes already document; the long ones (`patch`, `snap`, `restore`)
match what `emu`'s other subcommands call those things.

## Conditions

A condition is evaluated **on arrival**, before the instruction runs, so it sees the
state the instruction is about to act on.  Anything non-zero is true.

```text
registers   r0..r15, er0..er14, xr0/xr4/xr8/xr12, qr0/qr8
            sp pc csr psw ea lr lcsr dsr
flags       c z s ov mie hc elevel
memory      [0xD180], [filter], [er4]        (a quiet read: see below)
symbols     input ans filter ki ko key_accepted idle reset_entry ...  (`help`)
hits        this breakpoint's own hit count
operators   == != < <= > >= && || ! ( )
numbers     10, 0x0A, 0Ah
```

```sh
bp 0F968h if r0 == 0x40 && [filter] == 0xFF
bp 201FA14 if hits > 2
wp D180 w if [D180] == 0x99
```

Evaluating a condition reads memory through the side-effect-free path, so testing a
breakpoint does not itself add to the fault log or trip a watchpoint.  It also
short-circuits, so `[x] != 0 && [y] != 0` does not read `y` when `x` is zero.

## Four decisions worth knowing

**A parked machine is waited out by `continue`, and reported by `step`.**  The ROM
parks in STOP as normal operation -- after every reset and once per key scan -- so
`g` keeps ticking and lets the timer or keyboard wake it, and reports `Stopped` only
if the whole budget passes with nothing executing.  `s` reports a parked machine
immediately, because one step that executed nothing is always worth saying.  A
machine that will not wake is stuck by its own `SBYCON` write; `runmode run` leaves
standby.

**A data watch stops after the access, not before.**  The instruction that wrote is
the interesting one, so the report carries its PC.  Stopping before would name the
previous instruction, which is never what was meant.

**A patch lives in the bus's overlay list and nowhere else.**  An overlay shadows
both the code fetch and the data read at an address, and the ROM image on disk is
never modified, so a patch is undoable by construction.  Because the overlay list is
part of a snapshot, `restore` brings patches back with the rest of the machine --
which is why the patch list is re-read from the bus afterwards rather than cached.

**A snapshot is the whole machine, and restoring one replays identically.**  It
covers the CPU, RAM, the peripherals, the timer divider, the screen, the keyboard's
held keys, and the planted code.  Snapshot, run, restore, run again reproduces the
same state to the byte; an integration test pins that on the real ROM.  Breakpoints
are *not* in a snapshot -- they are the debugger's bookkeeping, not machine state.

## Scripts

A script is a list of commands.  Blank lines and `#` comments are ignored, output
goes to stdout, and **the script stops at the first failure** with a non-zero exit
code and the line number -- a script that continued past a failed assertion would
report success for a run that proved nothing.

`#` is ambiguous: it opens a comment and prefixes an immediate.  It is a comment
when what follows is not a number, so `set r0 #0x41` sets a register and
`s  # one step` is a comment.

Two scripts come with the repository, and both reproduce a finding from the research
notes on the real ROM:

| script | what it proves |
|---|---|
| `scripts/rop-trigger.dbg` | `SP = 0xD522 + n + 2`; the first gadget address lands at `0xD530 + n` |
| `scripts/selftest-entry.dbg` | SHIFT + 7 held through an ON reset enters the self-test |
| `scripts/lbf-converter.dbg` | why the `lbf/in²>kPa` character converter works: byte `23h` terminates a history record |

The first two were measured independently a second time, by a Python prototype kept
alongside the research notes, and the two sets of results agree.

The third answers a question the community ROP tutorial explicitly leaves open -- it
says the `23` byte of `lbf/in²>kPa` "gets eaten" and defers the reason to a later
chapter that never gives it.  The reason is that `23h` is a history record's terminator and the
reader stops at the first one, which for `FE23` is the character's own second byte;
the recalled text is then `31 FE 00`, and the `FE00` is what lets the cursor be
placed inside a double-byte character.  The script shows all three cases, including
a control where the second byte is not `23` and the recall is intact.

It is also a worked example of the debugger earning its keep: the write side was
found with a watchpoint on `0xD392`, the record layout by reading the bytes back, and
the reader's behaviour by pressing `↑` and looking at what arrived -- none of which
the listing alone would have shown, since this is about data written at run time.

## Address notation

`0x22110`, `22110h`, `$22110`, and a bare hex run containing a letter (`D180`,
`121A8`) all work.  A bare run of **digits** is decimal, because `22110` is a valid
decimal *and* a valid hex address and they are different places (`0x565E` versus
`0x22110`).  Guessing would be the worst outcome -- a breakpoint would land
somewhere plausible and the result would look right -- so an all-digit address
written as hex needs its `h`.

A bare symbol is its **address**; the byte stored there needs brackets.  Write
`[filter] == 0xFF` to read `0xF042`, or `filter == 0xF042` to name it.  `m filter`
shows what is stored there, because a memory command's argument is always an
address.

## Limits

* **No line editing or history.**  Input is read a line at a time.  That is the
  price of this workspace's zero-dependency policy; the engine and the command
  language are independent of it, so a terminal library can be added later.
* **A dialogue a key opens cannot be driven by name.**  `SHIFT`+`8` opens the
  unit-conversion menu and the keys inside it will be pressed by the ROM, but the
  debugger has no concept of "the menu is up" -- it presses codes and reads memory,
  and the menu lives in the ROM's own state.  Addresses and codes still work.
* **`step out` is meaningless on a ROP chain.**  A gadget ends in `POP PC`, which
  does not use the link register, so there is no return address to stop at.
* **`BC` cannot be assembled by `patch`.**  Its displacement is relative to its own
  address, which the assembler is not told; the error says so.  Use
  `nxu8_asm::asm::bc_to(pc, target, condition)` from a Rust test instead.
* **No reverse execution.**  Rollback is the explicit `snap`/`restore` pair.
* **The self-test window is narrow.**  `0xF042` reads `0xFF` only briefly, and
  running past it leaves the scan loop; `scripts/selftest-entry.dbg` asserts on the
  armed value so an overshoot fails rather than silently proceeding.
