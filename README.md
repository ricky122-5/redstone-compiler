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

3. **A flip-flop cell and clock spine.** Sequential designs need a physical
   D flip-flop macro and global clock distribution.

So `examples/gcd.ohm` produces a correct 724-gate netlist that simulates
correctly at the gate level, but cannot yet be placed — and `-o` will tell you
why rather than emitting something broken.

## Layout

| file | role |
| --- | --- |
| `lexer.rs` `parser.rs` `ast.rs` | frontend |
| `ir.rs` `lower.rs` | hash-consed DAG + FSMD lowering |
| `machine.rs` | golden-model interpreter |
| `netlist.rs` `bitblast.rs` | NOR netlist + gate-level simulator |
| `redstone.rs` | Minecraft power semantics and block simulator |
| `tech.rs` | redstone cell library |
| `layout.rs` `route.rs` | floorplan; 3D maze router with rip-up and retry |
| `world.rs` `nbt.rs` `schem.rs` | block world, NBT writer, schematic emitter |
| `structure.rs` | vanilla structure-block and `.mcfunction` export (no mods) |
| `tools/mc-validate.sh` | headless-server validation against the real game |
