# ohmc — an Ohm → Minecraft redstone compiler

Compiles a small statically-typed imperative language (**Ohm**) into a Minecraft
redstone circuit, emitted as a **Sponge v3 `.schem`** file you can paste with
WorldEdit.

This is high-level synthesis, not a soft-core CPU. There is no fixed processor
with a ROM you fill in — every program synthesises **its own datapath and its own
control FSM**, which then gets placed and routed into actual blocks.

```
source .ohm
  → lex → parse → type/width check
  → lower to an FSMD (hash-consed expression DAG + one-hot control FSM)
  → bit-blast to a NOR-only gate netlist
  → technology-map to redstone cells
  → place (levelised) + route (signal-strength aware)
  → Sponge v3 schematic
```

## Verified in unmodified Minecraft

| design | gates | flip-flops | inputs | in-game result |
|---|---|---|---|---|
| `invert.ohm` | 20 | 0 | 1 | all cases, both sweep directions (earlier compiler) |
| `andgate.ohm` | 23 | 0 | 2 | all cases, both sweep directions (earlier compiler) |
| `add2.ohm` | 51 | 0 | 4 | all 16 cases, both sweep directions (earlier compiler) |
| `add.ohm` | 195 | 0 | 16 | 24 sampled of 65536, both directions, re-checked on the current compiler |
| `tick.ohm` | 99 | 11 | 1 | both inputs, halting on the cycle the golden model predicts |
| `count.ohm` | 165 | 15 | 2 | all four inputs, state vector matching the simulator register for register |

`add.ohm` is 24098 blocks. Designs wider than a few inputs are sampled rather
than enumerated - `OHMC_MAXCASES` sets how many - and every sweep is run twice,
ascending then descending, because a circuit that answers differently the second
time is holding state a combinational design must not have.

## Sequential designs

Programs with loops or branches synthesise a control FSM and real registers.
Those place: a register bank, both clock phases and reset distributed to it, and
each flip-flop's Q wired back as a source the combinational cone reads.

`tick.ohm` (99 gates, 11 flip-flops, 6 basic blocks) places and runs. Reset it,
clock it, and the one-hot state vector walks the basic blocks:

```
go=0   00000100000 -> 00000001000 -> 00000000010 -> 00000010000
       -> halt at cycle 4, done = 0
go=1   00100100000 -> 00100001000 -> 00100000100 -> 00010001000
       -> 00010000010 -> 01010010000 -> halt at cycle 6, done = 1
```

The golden model says 5 and 7 cycles; the placed circuit halts on exactly
those, and every clock phase settles - 116 ticks once halted, no torch burnout,
nothing left pending.

Getting there needed one thing that is easy to state and was not easy to find:
**a design with an FSM has two resets, and they are different mechanisms.**
`rst_lever` is the flip-flops' asynchronous clear - assert it and every register
goes to zero at once, no clock needed, which is what puts the build into a known
state after a `.mcfunction` places it one block at a time. `net_reset_lever`
drives the netlist's own `Src::Reset`, and that one is *synchronous*: all it does
is raise the entry block's D input, so it has to be clocked in. Hold both with
the clock still - the obvious thing to do - and the state vector is cleared and
then never loaded. No block is ever active, so the machine sits at
`Q=00000000000` for ever, settling perfectly at every step and computing
nothing. It looks exactly like a dead circuit and is a dead *procedure*.

The size problem that used to block this is gone. One bit of state was a
68 x 141 macro, then 68 x 113; it is now **35 x 55**, a quarter of the area, and
nothing about the circuit changed to get there - only the spacing, which was
chosen when the D latch's five cells were wired by hand and was never revisited
after the router took that job over. Every constant in it is a measured floor:
at a tighter pitch the latch will not store a 1, at a closer offset the R gate's
feed is walled in completely.

`gcd.ohm` (724 gates, 42 flip-flops) does not place yet. Every one of its 1103
connections routes when it has the grid to itself; routed together, one
attempt each and no rip-up, 806 to 827 do, depending on the order they are
tried in. About half the failures are the same in every order and sit on a few
very wide nets; the other half move with the order. See the status section.

## Quick start

```sh
cargo build --release

# Compile and run on the golden model
./target/release/ohmc examples/gcd.ohm --stats --run a=48,b=18
#   NOR gates    724
#   flip-flops   42
#   logic depth  28 gate levels
#   ran 25 cycle(s):
#     g = 6

# Emit a loadable schematic
./target/release/ohmc examples/invert.ohm -o invert.schem
```

## The Ohm language

```rust
input  u8 a;          // a lever bank
input  u8 b;
output u8 g;          // a lamp bank

fn gcd(u8 x, u8 y) -> u8 {
    while (y != 0) {
        if (x > y) { x = x - y; } else { y = y - x; }
    }
    return x;
}

proc main() { g = gcd(a, b); }
```

- Types are `u1`..`u32`; `bool` is `u1`. Widths are checked, and literals infer
  their width from context (`x + 1` works for any width of `x`).
- `+ - * & | ^ ~ << >> == != < <= > >= && || !` and `? :`.
- `if`/`else`, `while`, functions (inlined; recursion is rejected with the call
  chain), `input`/`output` ports.

Two deliberate departures from C, both because the target is hardware:

- **Evaluation is eager.** `&&`, `||` and both arms of `? :` always evaluate.
  A combinational circuit cannot short-circuit — every gate is physically
  present and always computing — so pretending otherwise would be a lie.
- **Every call ends a clock cycle.** That uniform rule is what makes cross-cycle
  register spilling correct.

## How the hardware is built

**Execution model.** Each basic block is one clock cycle. Within a block,
expressions read the register values from the *start* of the cycle and all
writes commit together at the end. Sequential source semantics survive because
lowering threads an environment of "value so far" per variable — so
`x = y; y = x;` behaves like the source says, not like a swap.

**One primitive gate: multi-input NOR.** Not an arbitrary choice — it is what
redstone gives you for free. Several signals feeding one dust net OR together
(dust takes the strongest source), and a torch on the block beneath that net
inverts it. Arbitrary fan-in NOR costs exactly one torch.

**Arithmetic is real structural hardware.** Ripple-carry adders, an array
multiplier, logarithmic barrel shifters. `a < b` reuses the adder — it is the
complement of the carry-out of `a + !b + 1` — so comparisons share gates with
nearby subtractions via hash-consing.

### The cell geometry, and two bugs worth reading about

A gate's output emerges one level **below** its input. That asymmetry is forced,
not chosen. The obvious layout puts the output dust on the block above the
torch — but a lit torch *strongly* powers the block above it, and a strongly
powered block energises **every** adjacent dust, including the gate's own input
pad one block away. The first version of this compiler built a beautiful,
compact cell that was an oscillator. Taking the output from beside the torch
instead costs one level of drop and keeps input and output isolated.

The second bug is a better story about testing. Fan-in feed points originally sat
in adjacent columns, so a gate's separate input nets were *shorted together* —
and the 4-input NOR truth-table test passed anyway, because `NOR` of a shorted
bus equals `NOR` of its members. The test could not distinguish a real 4-input
gate from one input fed by a merged bus. It took the router refusing to place a
two-input gate to surface it. There is now an explicit isolation test that fails
on a shorted cell, which the truth table alone never would.

## Routing

Levels are spaced apart in Y specifically to leave free layers between them,
because **a crossing needs somewhere to cross**. An earlier floorplan packed
levels one block apart so output and input planes lined up and all routing was
horizontal; it was collision-free but could not express a crossing at all.

Routes are found with A* over free space. Three things the search cannot see on
its own, and how they are handled:

- **Turns.** With all flat moves priced alike, a diagonal route degenerates into
  a staircase of alternating X and Z steps. That path has no three collinear
  nodes — and a repeater needs exactly that — so it cannot be kept alive over
  distance. The search state therefore carries an incoming direction and prices
  turns.
- **Self-collision.** Every wire node claims three cells in its column: dust,
  substrate below, clearance above. Two nodes in the same column must differ in
  Y by at least three. The subtle case is a gap of exactly two, where the upper
  node's substrate lands in the lower node's clearance and silently breaks the
  slope that was meant to connect them.
- **Repeaters.** A cheapest path is often a pure staircase with nowhere flat to
  refresh the signal.

Deep drops are handled by **relay staging** rather than by the router. A descent
of N blocks costs N of the 15-block signal budget with no chance to refresh,
since a repeater cannot sit on a slope; past roughly the budget itself a one-shot
descent is impossible on physics alone. So a drop deeper than `MAX_DROP` is split
into stages, each landing on a relay repeater that restores full strength and
gives the next stage a fresh budget.

All three are handled by rip-up and retry: find a path, check it, and on failure
bar the offending cell and search again, escalating a slope penalty in parallel
to push routes toward flat runs.

Two nets can never touch, because the router refuses to place a wire adjacent to
another net's wire. Shorts are structurally impossible rather than merely
unlikely.

## Verification

Nothing here is asserted on faith. There are **three independent models**
differentially tested against each other, plus a fourth check against the actual
game:

1. **`machine.rs`** — a cycle-accurate interpreter of the word-level IR. Golden.
2. **`netlist.rs`** — a gate-level simulator of the NOR netlist.
3. **`redstone.rs`** — a tick-accurate simulator of the *actual blocks*,
   implementing Minecraft's power rules (strong vs. weak power, dust decay,
   torch inversion, repeater diodes, dust connection shapes).

`src/bitblast.rs` checks the gate netlist against the golden model for adders,
subtractors, all six comparisons, the multiplier, the barrel shifter, and
sequential programs including GCD. `src/layout.rs` takes Ohm source all the way
to blocks and simulates the blocks.

`redstone.rs` documents exactly which Minecraft rules are modelled and which are
deliberately not (sub-tick update ordering, torch burnout, quasi-connectivity) —
the generated circuits are synchronous and clocked well below those thresholds.

### Against the real game

The three models above could all be wrong in the same way, because two of them
were written from the same understanding of redstone. So there is also a harness
that boots a **headless vanilla Minecraft server**, places a compiled circuit via
a datapack, sweeps every input combination by toggling the lever blocks, and
diffs the lamps against `ohmc --truth`:

```sh
cargo build --release
tools/mc-validate.sh examples/invert.ohm
#   in=0    expected q=1   observed 1   ok
#   in=1    expected q=0   observed 0   ok
#   PASS: Minecraft agrees with the compiler on all 2 cases
```

It downloads Mojang's official server jar (SHA-verified against the local
version manifest) into a scratch directory. Nothing touches an existing install.

Running it for the first time immediately found **two real bugs** that 96 passing
unit tests had not:

1. Output lamps were mounted *beside* the final wire. Dust only powers the block
   beneath it and the blocks it points into, and dust with a single connection
   renders as a straight line along that one axis — so a lamp off to the side of
   an arriving wire never lit. Lamps are now the substrate *under* the final
   dust, which is the same unambiguous interaction the whole cell library rests on.
2. The block simulator's `lamp_lit` returned true for *any* adjacent powered
   dust, which is exactly permissive enough to hide bug 1. It now asks whether
   the block is actually powered.

### Physics conformance

`cargo run --release --example conformance -- /tmp/conf.mcfunction` builds seven
tiny isolated configurations, prints the simulator's answer for each, and emits
an `.mcfunction` so the game can be asked the same questions. Each case
exercises exactly one rule, so a disagreement names the rule instead of leaving
you to bisect a whole circuit. All seven currently agree.

One case earns its place: `lamp_beside_powered_block`. A powered block lights an
adjacent lamp — block powering activates mechanisms even though weak power never
spreads onto dust. This was logged as an unmodelled behaviour on the assumption
it could never bite, then measured against the game and found to be real. It is
now modelled, and the layout reserves a shell around every output lamp so that
unrelated wiring passing nearby cannot switch a result on.

```sh
cargo test        # 101 tests
```

## Status — what works and what doesn't

**Verified in actual Minecraft:**

- **`examples/tick.ohm` runs.** 99 NOR gates, 11 flip-flops, 6 basic blocks,
  61,898 blocks placed by a datapack into a stock server. Pulse the async clear,
  hold the state reset for one clock, then work the two clock phases:

  ```
  go=0   lamp 0 for every cycle                     model: done=0
  go=1   lamp 0 through cycle 6, 1 from cycle 7 on  model: done=1 after 7 cycles
  ```

  The golden model says seven cycles and the lamp lights on the seventh, then
  holds - a real halt, not a glitch. This is a compiled program with a control
  FSM and real registers executing in vanilla Minecraft.
- `examples/invert.ohm` inverts correctly, all inputs, repeatably.
- `examples/andgate.ohm` (3 NOR gates, crossing wires) matches its truth table
  on all 4 input combinations.
- All 7 cases in the physics conformance suite agree with the game: flat runs,
  dust ramps up and down, a roofed ramp correctly *failing* to climb, weak power
  correctly failing to cross a solid block, repeater-drives-dust, and the NOR
  cell itself.

**Working and verified against the internal models:**

- Full frontend: lexer, parser, width checking, function inlining, FSMD lowering.
- Bit-blasting to a NOR/DFF netlist, differentially tested against the golden
  model on arithmetic, comparison, multiplication, shifts, loops, and GCD.
- The redstone cell library: 1-, 2- and 4-input NOR truth tables, gate chaining,
  and automatic repeater insertion, all verified in the block simulator.
- Sponge v3 schematic emission (gzip NBT, varint palette, baked dust shapes).
- **Straight-line programs compile to placed, simulated, loadable redstone**,
  up to a few dozen gates. Inverters, AND/OR/XOR, 3-input majority and XOR all
  place and are verified by simulating the emitted blocks.
- **Programs with a control FSM place, clock and compute in unmodified
  Minecraft.** `count.ohm` - 165 NOR gates, 15 flip-flops, 6 basic blocks, a
  two-bit input and a two-bit output - resets, runs its state machine, halts on
  the cycle the golden model predicts, and returns the right answer for every
  input:

      n=0: levers read back 0, lamps c=0, want 0
      n=1: levers read back 1, lamps c=1, want 1
      n=2: levers read back 2, lamps c=2, want 2
      n=3: levers read back 3, lamps c=3, want 3

  The state vector matches the block simulator register for register and
  transition for transition, so this is the whole machine agreeing, not just the
  lamps landing on the right value. `tick.ohm` (99 gates, 11 flip-flops) does the
  same for its single input. `examples/seq_settle.rs` walks a ladder of synthetic sequential designs
  from one flip-flop up to eleven flops and 121 gates; every one places *and
  settles*.

**Verified in unmodified Minecraft, under the current compiler:**

| design | shape | result |
|---|---|---|
| `add.ohm` | 95 gates, combinational, 8-bit | 24 of 65536 inputs sampled, both sweep directions, all correct |
| `tick.ohm` | 99 gates, 11 flip-flops, 6 basic blocks | both inputs, halts on the predicted cycle |
| `count.ohm` | 165 gates, 15 flip-flops, 6 basic blocks | all four inputs, state vector matching the simulator register for register |

`add` is re-checked against the current router rather than carried forward
as a remembered result. Every routing rule underneath it changed: relay
chains converge in Z, hop grade is constrained at both ends, the output
spine cap went from 24 to 80 and the rip-up radius from 24 to 40. Its Z
depth fell from 181 to 52 and it lost 2500 blocks, so "it passed once" was
a claim about a build that no longer exists.

**`gcd.ohm` routes 715 of its 1103 connections, and the remaining gap is
congestion, not a missing rule.** Every local lever has been swept and is
either at its optimum or saturated:

| lever | routed / 1103 |
|---|---|
| **current settings** | **715** |
| eviction radius 24 (was) | 330 |
| eviction radius 64 | 715 — saturated |
| rip-up attempts 6 → 20 | 330 — no effect |
| connection order: easiest first | 580 |
| connection order: grouped by net | 65 |
| grade slack `+2+dv/8` | 353 |
| grade slack `+3+dv/6` | 236 |
| gate gap 6 → 10 | 181 |
| level pitch 6 → 9 | 1 |
| congestion charge, weight 15 | 453 |
| congestion charge, weight 40 | 314 |

The pair at the top is the informative one: more attempts at the same
radius change nothing, while the same attempts at a wider radius double
the result. The router was never giving up too early - it was evicting
the wrong neighbours, because the net holding the contested space sat
outside the window it was allowed to consider.

Thirteen variants; the current configuration is the best of all of them and a
local optimum in every direction tested. Three of those results are worth more
than their numbers:

* **Level pitch is not a spacing knob.** `LEVEL_H` divides `MAX_DROP`, so a
  relay hop spans exactly two levels; at 9 that relationship breaks and the
  design routes *one* connection. The two constants sit five lines apart with
  nothing recording the coupling.
* **More room makes it worse.** Widening the gate gap gives every wire more
  space and more distance to cross, and the distance costs more than the space
  buys - 181 against 715.
* **A congestion charge applied online does not work.** Penalising contested
  ground during a forward pass charges it only *after* the early nets have taken
  the good corridors, so it pushes the later, already-struggling nets further
  out without freeing anything. It has to be paired with ripping up everything
  between passes, so that the nets holding the corridors have to re-justify
  them.

**The array is five times wider than packing needs, and that is where the
long connections come from.** `examples/fanout.rs` with `OHMC_NET=` names a
signal's driver and readers; `OHMC_TRACE=1` prints the plan for every
connection. Together they identify the connection that fails in nearly every
configuration tried:

    conn g216 in1 <- net215: src0=(423,-6,2) feed=(982,-11,-7) drop=5 spanx=559

Gate 216 is `Nor([167, 215])`. Its two drivers sit far apart, barycenter
placement puts it at their mean, and it lands at x=982 in empty space - 559
blocks from the spine that has to reach it, for a five-block drop. Because the
placement sweep can only push a gate *right* (`at = max(wish, prev_end)`), that
one outlier stretches its whole row, and across 28 levels `gcd`'s array reaches
1164 blocks wide where packing 724 gates needs about 230. A third of all
connections span over 200 blocks in X.

Capping how far a wish may open a gap does not fix it: 60 routes 156
connections and 20 routes 271, against 715 uncapped. Width is a symptom, not
the cause. Gate 216 *wants* to be at 982 because its drivers are 1100 blocks
apart, and forcing it nearer one of them only lengthens the wire to the other.
The spread is in the netlist's connectivity, and clamping positions does not
change connectivity - it needs real placement (analytical, or partitioning so
strongly-connected gates share a region), not a clamp on this sweep.

Three placement fixes were tried against that width and all three fail, which
corrects the diagnosis above:

| placement change | effect |
|---|---|
| cap how far a wish may open a gap | 715 → 271 (cap 20), 156 (cap 60) |
| relaxation rounds that let a gate move *left* | no effect at all — width identical |
| place at the median of drivers, not the mean | width 1003 → 1101, mean span 406 → 719 |

The relaxation result is the informative one. If the array were wide because the
legalising sweep ratchets rightward, letting gates move left would contract it;
it changes nothing. So the width is not an artefact of legalisation - the
*barycenters themselves* are spread, because the gates they are computed from
are spread, which is circular. That is the problem analytical placement exists
to solve by solving for all positions at once, and it is not reachable by a
local sweep however it is legalised.

**Wire length is not the binding constraint - routing space is.** Levelisation
is ASAP, which crams gates into the earliest level their inputs allow, and that
is very lopsided: `gcd` puts 120 of its 608 gates on level 2 against a mean of
21.7. A level is one row, so that single row is 1080 blocks wide and the whole
array measures 1164. Gates within a level are independent by construction, so
spilling the excess to later levels is always legal, and `OHMC_LEVEL_CAP` does
exactly that:

| level cap | array width | mean span | routed |
|---|---|---|---|
| none (ASAP) | 1003 | 406 | **715** |
| 45 | 345 | 120 | 379 |
| 30 | 195 | 106 | 431 |

It works precisely as designed and makes routing *worse*. A five-fold narrower
array with four-fold shorter connections routes half as many of them. Narrowing
concentrates the same gates into less space and takes away the room the router
needs around each connection; widening (gate gap 6 → 10) lengthens the wires
instead and routes 181. Both directions lose, so the default is a genuine
optimum on that trade and not an untuned guess.

**Sharing wire between a net's branches does not help either.** The router gives
every branch of a net its own disjoint path back to the driver. A branch is
connected the moment it reaches any cell already carrying the signal, so letting
it start from cells just downstream of the net's existing repeaters - full
strength, clean signal budget - should save exactly the routing space that is
binding. It saves the wire and loses the routing:

| taps offered per branch | add | tick | count | gcd routed |
|---|---|---|---|---|
| none (star routing) | 12.7s | 18.1s | 31.2s | **715** |
| all | 13.0s | 84.4s | 72.6s | 412 |
| nearest 3 | 12.3s | 15.8s | 82.9s | 251 |

Wire falls 5-6% in every case, and `tick` places 12% faster from three good
seeds. `gcd` gets worse either way. Seeding A* with every tap turns a focused
search into a broad one - it fails with "gave up after 250001 expansions" - and
restricting to the nearest three fixes the small designs without helping the
large one at all.

**Changing what a failing connection retries does not help.** Two changes that
touch only connections which have already failed, so neither can disturb what
already routes:

| retry change | gcd routed |
|---|---|
| none | **715** |
| vary the relay stage count on each retry | 232 |
| remove a failed attempt's relays before retrying | 332 |

The second looked like a leak: relays are claimed, `rip` spares claimed cells,
so every failed attempt leaves its chain in the grid. It is not a leak. Those
relays belong to the net, and the next attempt can attach to them - a retry
inherits a partial chain instead of rebuilding one. Removing them makes `count`
place twice as slowly with *more* blocks, which a genuine leak fix cannot do.
Varying the stage count fails for the same reason from the other side: each
retry stamps a chain at fresh sites and inherits nothing.

**Every connection routes on its own; they fail because of each other.**
`OHMC_ISOLATE=1` routes each connection alone against the structure-only grid -
gates, spines and stubs, but no other net's wire - and counts:

| design | routes together | routes alone |
|---|---|---|
| `tick` | 141 of 141 | 140 of 141 |
| `count` | 233 of 233 | 231 of 233 |
| `gcd` | 715 of 1103 | **1100 of 1103** |

The first two rows calibrate the instrument: both designs place fully, so the
one or two failures in isolation are its error, and it errs conservative (a
real run retries a failed connection after rip-up; isolation gives each one
pass). `gcd`'s three failures are inside that error.

So the placement is routable, and roughly 385 connections fail purely through
contention with other nets. That retires every placement hypothesis above -
the array is not too wide to route and the wires are not too long - and it
explains why none of the placement and tuning variants helped: they perturbed a
layout that was never the problem. It also says why the congestion charge
failed. It was applied around a connection that had *failed*, which is where a
route gave up and not where the contention is; that was a guess at a signal
that can now be measured directly.

**The routed-connection count is noise-dominated, and most of the comparisons
above are inside the noise.** `OHMC_TIE_SEED` reorders connections of equal
difficulty and changes nothing else - same netlist, same placement, same
hardest-first policy, every order as valid as the default:

| tie order | `gcd` routed |
|---|---|
| default | 715 |
| seed 3 | 683 |
| seed 2 | 419 |
| seed 4 | 343 |
| seed 1 | 238 |

An equally valid order moves the result by about 480 connections. Nearly every
variant recorded in the tables above lands inside that range, so none of them
has been shown to be better or worse than the default by a single run - and
that includes the one recorded win, the eviction radius going from 24 to 40
(330 to 715). The default's 715 is the top of its own spread.

The reason is the metric. "N of 1103 routed" is where, in the work order, the
*first* connection that cannot be recovered happens to fall. That is not how
many connections can be routed, and where the first wall is hit is exactly
the thing a different order changes. What the numbers support is narrower:
every connection routes alone, they fail by crowding each other, and which one
fails first is close to arbitrary.

`OHMC_SURVEY=1` replaces the metric: it gives up on a connection once its rip-up
rounds are spent, keeps going, and reports how many route in total. A survey on
`gcd` is expensive, so its budgets were calibrated first on `count`, which routes
fully:

| `count` survey | routed | time |
|---|---|---|
| `OHMC_ATTEMPTS=24 OHMC_RIPS=6` (defaults) | 233 of 233 | 30s |
| `OHMC_ATTEMPTS=24 OHMC_RIPS=1` | 233 of 233 | 29s |
| `OHMC_ATTEMPTS=4 OHMC_RIPS=1` | 195 of 233 | 135s |
| `OHMC_ATTEMPTS=4 OHMC_RIPS=6` | 148 of 233 | 888s |

A single rip-up round with the full retry ladder is as faithful as the defaults
and as fast, so that is what a `gcd` survey should use. Cutting the ladder is a
false economy twice over: it loses connections, and the failures it causes make
the run slower. The last row is the instructive one. With a weak search, *more*
rip-up is worse - 195 routed becomes 148 and the run takes six times as long -
because every connection it evicts cannot find a new path either, fails, and
evicts its own neighbours in turn. Rip-up helps only when the search is strong
enough to re-route whatever it displaces.

**Routing is deterministic, so comparisons across tie orders are valid.** Two
runs of the same survey on the same input must print the same progress at every
checkpoint. A second run of an identical configuration, hours after the first
and under different machine load, matched all eight checkpoints exactly:

| taken | routed | unroutable | still queued |
|---|---|---|---|
| 50 | 48 | 0 | 1054 |
| 100 | 79 | 2 | 1021 |
| 150 | 105 | 4 | 993 |
| 200 | 130 | 5 | 967 |
| 250 | 165 | 7 | 930 |
| 300 | 197 | 8 | 897 |
| 350 | 203 | 8 | 891 |
| 400 | 173 | 9 | 920 |

The last two rows are the demanding part. Between them rip-up evicts thirty
finished connections, which is where any hash-order or timing dependence would
show first, and the replay reproduces the drop to the connection. This was worth
checking rather than assuming: the original run had used 70 CPU-minutes to reach
a point this one reached in 21, a gap large enough to suggest divergence. It was
contention - that run shared the machine with six others.

**Surveyed without rip-up, 806 to 827 of `gcd`'s 1103 connections route, and
about half the failures are the same in every order.** `OHMC_RIPS=0` gives each
connection one full-ladder attempt and never evicts anything, which avoids the
cascade and loses a single connection on `tick` and on `count`:

| tie order | routed | unroutable |
|---|---|---|
| default | 807 | 296 |
| seed 1 | 827 | 276 |
| seed 2 | 806 | 297 |

A spread of 21 across orders, where the first-failure metric spread by 480.
`OHMC_SURVEY_OUT` writes out which connections failed, and the three lists
were compared under a rule fixed before any of them existed - the share of an
average list that every order loses: at least 0.60 is a stable hard core, at
most 0.30 is order-dependent contention, anything between is mixed.

142 connections fail in all three orders, 437 in at least one: a share of
**0.49, mixed**. The core is concentrated where the hot-slab measurement
pointed. Six nets hold 44 of the 142:

| net | driver | readers | core failures |
|---|---|---|---|
| 167 | `Nor([43])` | 41 | 14 |
| 168 | `Nor([34])` | 18 | 8 |
| 43 | `DffQ(41)` | 42 | 7 |
| 201 | `Nor([39])` | 9 | 6 |
| 44 | `Nor([36])` | 9 | 5 |
| 36 | `DffQ(34)` | 11 | 4 |

That is what splitting a wide net addresses - cloning for the NOR drivers,
buffer trees for the flip-flop outputs - and both are being surveyed the same
way. The other half of the failures changes with the order connections are
tried in, which is contention between nets, and only a router that stops
depending on that order removes it.

**Splitting a wide net does not thin its hot spot; it concentrates it.** Before
spending three hours surveying a transformed netlist, the isolation run measures
the mechanism a split is supposed to act on - how many connections' footprints
overlap each cell when every connection is routed alone:

| netlist | contested | cells wanted by 6+ | cells wanted by 10+ | max |
|---|---|---|---|---|
| baseline | 37.5% | 43,489 | 4,266 | 16 |
| cloning, threshold 12 | 37.8% | 44,121 | 4,163 | 18 |
| buffering, threshold 12 | 35.2% | 51,876 | 5,976 | 21 |
| both | 36.3% | 52,571 | 8,055 | 20 |

Cloning leaves demand where it was. Buffering lowers contention overall and makes
the worst cells hotter, and doing both nearly doubles the cells that ten or more
connections want. A copy of a gate reads exactly the same inputs as the
original, so placement, which pulls a gate toward its drivers, pulls every copy
toward the same place; and a buffer's new wires all begin at the original
source. The funnel moves instead of dividing.

That also says where the ceiling comes from. Each connection can be helped
by taking room from its neighbours, right up until the neighbours have
none left to give, and no constant fixes that. What is needed is
negotiated congestion in the PathFinder sense - route everything with
overlap allowed, price shared cells, and re-route until nobody shares -
which replaces the first-come-first-served ordering rather than working
around it. Rip-up as it stands only ever perturbs one connection's
neighbourhood at a time.

**Not done — the honest gap:**

1. **The 2-bit adder is 14/16 in the real game, and the harness is the weak
   link.** `examples/add2.ohm` was reported last round as latching. It does not.
   Driven correctly it computes correctly: verified cell by cell at two separate
   inputs, where all 336 powered dust cells and both output lamps match the
   simulator exactly.

   What was actually wrong was the validator. Four separate harness defects, each
   of which produced output indistinguishable from a compiler bug:

   - **Rebuilding in place.** Leftovers from the previous case survive underneath
     the new one.
   - **`fill` caps at 32768 blocks** and fails *silently* above it, so the
     clearing step did nothing at all.
   - **`forceload` caps at 256 chunks**, so an attempt to give each case its own
     patch of ground left distant cases in unticked chunks reading zero.
   - **`sim_sweep` built a fresh simulator per input**, so it began from a clean
     slate every time and could not observe state-dependence by construction.

   The validator now runs one case per freshly created world, which is slow -
   a server boot per case - but is the only scheme measured to agree with a
   hand-checked result. It also reads the input levers back and reports
   `[HARNESS: ...]` when it failed to drive the circuit, so a harness failure can
   no longer masquerade as a logic error.

   Two cases still disagree, both reading zero, which is what an unsettled
   circuit looks like. Raising the settling time destabilised the server-restart
   sequencing instead of helping, and the lever readback caught that immediately.
   Making the restart robust is the next step, not a compiler change.

2. **Routing scales further but not far enough.** `examples/add.ohm` (99 gates,
   164 connections) now places in full: 179x123x181, 24654 blocks. `alu.ohm`
   still fails on its first connection (5/12 approaches, span 176).

   What got it there, in order of how much it mattered:

   - **Reserved landing zones.** A feed stub has ~3 legal approaches; a passing
     net could take all of them, leaving the target sealed and the router
     burning its whole budget looking for a way in.
   - **Relay chains that interpolate** from driver toward sink. Parking them at
     either end made the hop into or out of the chain span the whole build.
   - **Lamps below their driver** rather than in a shared bottom row. A tidy row
     put each lamp an arbitrary distance beneath its driver, which is exactly
     the one-shot descent dust cannot make.
   - **Barycenter placement** and **hardest-first net ordering**.

   Still missing is negotiated congestion (PathFinder): rip-up happens only
   within a single route, so an early net can take space a later one needs and
   nothing reconsiders.

3. **A flip-flop cell and clock spine — memory element working.** This is the
   only thing between the compiler and its goal of running `gcd.ohm` in
   Minecraft.

   `stamp_rs_latch` in `src/tech.rs` now **holds a bit**: raise an input, drop
   it, and the state persists. It is two cross-coupled NOR cells — the first
   structure in the project with feedback, which the router cannot express at
   all, since levelisation assumes a DAG and a cycle has no levels. So it is
   hand-placed and handed to the placer as an opaque macro, the way a
   standard-cell library ships a flip-flop.

   Four constraints, each found by measurement:

   - Each NOR needs **fan-in two** — feedback on one input, external set/reset
     on the other, or the two short together and the latch becomes a follower.
   - The cells need **separate Z bands**; side by side, each one's feedback
     wire runs through the other's output.
   - The pair must **start in a defined state**; both torches lit is not a state
     a cross-coupled pair can occupy.
   - **The whole loop must start consistent, not just the torches.** This was the
     real one. Repeaters hold state too, and a repeater relaying a high output
     while itself starting low is an inconsistency that launches a one-tick
     pulse. Round a loop with two inversions — non-inverting overall — that pulse
     circulates forever. Real redstone damps it through torch burnout; a
     deterministic simulator rings indefinitely.

   `stamp_d_latch` gates it into a D latch — `S = NOR(!D,!E)`, `R = NOR(D,!E)`
   feeding the RS pair. It places without collision and settles, but Q does not
   move yet: S and R are not asserting. The geometry is sound, so the fault is
   in the signal path. Test left in place and `#[ignore]`d.

   Each gate there gets its own X column *and* Z stage — wasteful, deliberately.
   Links then always run forward in Z on a lane unique to their source, which
   makes the macro collision-free by construction rather than by tuning. `D` and
   `enable` are exposed twice rather than fanned out internally, since two
   internal gates need each and a caller routing a net to two feeds costs
   nothing.

   Remaining for the goal: make the D latch latch, pair two into a master–slave
   flip-flop, distribute a clock, and teach the placer to treat flip-flops as
   macros in a register bank.

