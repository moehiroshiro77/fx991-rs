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

`data/_disas_verF.txt` is *derived* data: a separate tool generated it from a
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

The third explains the `lbf/in²>kPa` character converter: `23h` is a history
record's terminator and the reader stops at the first one, which for `FE23` is the
character's own second byte.  The recalled text is then `31 FE 00`, and the `FE00` is
what lets the cursor be placed inside a double-byte character.  The script shows all
three cases, including a control where the second byte is not `23`.

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

## The window debugger

The same engine also drives a window, so a session can be driven with the mouse
and watched while the calculator runs.  It lives in the graphical front end:

```sh
cargo run --release -p fx991-app      # then press F12
```

`F12` opens a **second window** with the panels, leaving the calculator where it
is.  That is deliberate: the ROM is debugged through its display, so a breakpoint
on a menu handler is not much use if the menu cannot be seen, and the menu cannot
be driven if the keys cannot be clicked.  `F12` again, `Esc`, or the window's own
close button puts the debugger away -- none of them quits the program, because the
calculator is the program.  Breakpoints and scroll positions survive closing and
reopening.

The panels are arranged the way a native debugger arranges them -- disassembly
across the top, memory under it, the register file and the stack down the right,
and the breakpoints along the bottom:

```
+--------------------------------------------------------------+
| PC 0010000h   tick 0   SP F000h          paused -- breakpoint |
+---------------------------------------------+----------------+
| Disassembly                                  | Registers      |
| > 0010000h  00 11  MOV  R0, #0               | PC 0010000h    |
| * 0010002h  10 22  AND  R2, #16              | R0 11  R1 22   |
+---------------------------------------------+----------------+
| Memory                                       | Stack          |
| 000D180h  39 A8 39 00 ...  |9.9..            | SP F000h       |
+---------------------------------------------+----------------+
| Breakpoints / Watches                                         |
| * B1  on  0010002h  0 hits                                   |
+--------------------------------------------------------------+
```

| key | action |
|---|---|
| `F7` | step into |
| `F8` | step over |
| `F6` | step out |
| `F5` / `F9` | run |
| `Space` | pause |
| `F2` | toggle a breakpoint at the PC |
| `F3` | follow the machine: re-anchor the listing and the dump to the PC |
| `F4` | move the keyboard to the next panel |
| arrows, `PgUp`/`PgDn`, wheel | scroll the focused panel |
| `Esc` / `F12` | close the debugger window |

Clicking a disassembly row toggles a breakpoint at the address that row shows.
The address is recomputed from the same window the panel drew, so a click cannot
land on an address that has since scrolled away.

The listing follows the PC until you scroll it.  Scrolling moves the window's
anchor one **instruction** at a time rather than by a fixed number of bytes,
because instructions are 2 or 4 bytes wide and an address has to land on a
boundary; the walk tests both lengths and prefers the longer one, since a forward
scan from a known boundary consumes a 4-byte instruction whole.  Scrolling also
stops the listing following the machine, so the view stays where you put it while
the program runs; `F3` anchors it to the PC again.

A run is a blocking loop with no way to interrupt it, so the window runs it in
slices and regains control between them.  A slice that spends its budget, or that
finds the ROM parked, is not a stop: a parked machine is the ROM's normal state
and a timer will wake it, so the run continues -- in larger slices while parked,
where nothing executes and a smaller slice would buy only more wakeups.

While the debugger is open it owns the machine: it holds it stopped, or advances
it a slice at a time, and the calculator window's own pacing stands down.  Two
things driving one machine would otherwise race.  The calculator window still
repaints and still takes clicks -- a key press is input to the guest, not to the
emulator, so it is as meaningful stopped as running.

**The font is required and is not bundled.**  Put a monospace face at
`data/font.ttf`, or pass `--font PATH`.  Monospace is not a preference: every
panel is a grid of character cells, and a hex dump's columns line up only because
every glyph has the same advance.  A face that cannot draw ASCII is refused with a
message.  Without a font the calculator runs normally and `F12` reports what is
missing.

## Limits

* **No line editing or history.**  Input is read a line at a time.  That is the
  price of this workspace's zero-dependency policy; the engine and the command
  language are independent of it, so a terminal library can be added later.  The
  window debugger is the other answer to this: it needs no typing at all, and its
  panels cover the commands a session is usually driven with.
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
