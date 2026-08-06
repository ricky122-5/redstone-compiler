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

Nothing here is asserted on faith. There are **three independent models**, and
they are differentially tested against each other:

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

```sh
cargo test        # 96 tests
```

## Status — what works and what doesn't

**Working and verified end to end:**

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

1. **Routing does not scale yet.** Small circuits place and simulate correctly,
   but a real datapath does not. `examples/add.ohm` is 195 NOR gates and the
   router exhausts its search budget partway through. Two causes, both fixable:
   routes never share a path even between fanout branches of the same net (a
   simplifying choice that costs a lot of area), and there is no global ordering
   or congestion feedback — nets are routed in level order and early ones take
   the good space. The standard answers are net ordering by criticality, shared
   fanout trees, and congestion-driven rip-up across nets rather than within a
   single route.
2. **A flip-flop cell and clock spine.** Sequential designs need a physical
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
