//! Physics conformance: tiny isolated redstone configurations, evaluated by our
//! simulator and emitted as a `.mcfunction` so the real game can be asked the
//! same questions.
//!
//! The full compiler output is too tangled to debug a rule disagreement in. Each
//! case here exercises exactly one rule, so a mismatch names the rule directly.
//!
//! Two traps, learned the hard way:
//!
//! * **Isolate the probe.** Dust with a single connection renders as a straight
//!   line and powers the block at *both* ends, and a powered block lights an
//!   adjacent lamp. A probe placed near the circuit lights for reasons unrelated
//!   to the rule under test.
//! * **Wipe the world between runs.** Circuits land at fixed coordinates, so
//!   leftovers from a previous run sit underneath the new one. That cost a full
//!   debugging cycle chasing a compiler bug that did not exist.
//!
//! Usage: cargo run --release --example conformance -- /tmp/conf.mcfunction

use ohmc::redstone::Sim;
use ohmc::structure::to_mcfunction;
use ohmc::world::{Block, Dir, Face, Grid, Material, Pos};

/// Each case gets its own slab of X so nothing can interact.
const PITCH: i32 = 12;

struct Case {
    name: &'static str,
    /// The lamp whose state is the answer.
    lamp: Pos,
    expect: bool,
}

/// A lever at `x, 0, -2` feeding dust that starts at `x, 0, 0`.
fn source(g: &mut Grid, x: i32) {
    g.force((x, -1, -2), Block::Solid(Material::PortIn));
    g.force((x, 0, -2), Block::Lever { face: Face::Floor, facing: Dir::North, powered: true });
    g.force((x, -1, -1), Block::Solid(Material::Wire));
    g.force((x, 0, -1), Block::Dust { power: 0 });
}

/// Lamp under a dust cell: the validated way to read a wire's state.
fn probe(g: &mut Grid, p: Pos) -> Pos {
    g.force((p.0, p.1 - 1, p.2), Block::Lamp { lit: false });
    g.force(p, Block::Dust { power: 0 });
    (p.0, p.1 - 1, p.2)
}

fn main() {
    let out = std::env::args().nth(1).unwrap_or_else(|| "/tmp/conf.mcfunction".into());
    let mut g = Grid::new();
    let mut cases = Vec::new();
    let mut x = 0;

    // 1. Flat run - the baseline. If this fails nothing else means anything.
    {
        source(&mut g, x);
        for z in 0..4 {
            g.force((x, -1, z), Block::Solid(Material::Wire));
            g.force((x, 0, z), Block::Dust { power: 0 });
        }
        let lamp = probe(&mut g, (x, 0, 4));
        cases.push(Case { name: "flat_run", lamp, expect: true });
        x += PITCH;
    }

    // 2. Ramp up: dust climbing the side of blocks, one Y per Z.
    {
        source(&mut g, x);
        g.force((x, -1, 0), Block::Solid(Material::Wire));
        g.force((x, 0, 0), Block::Dust { power: 0 });
        for i in 1..=3 {
            g.force((x, i - 1, i), Block::Solid(Material::Wire));
            g.force((x, i, i), Block::Dust { power: 0 });
        }
        let lamp = probe(&mut g, (x, 4, 4));
        cases.push(Case { name: "ramp_up_3", lamp, expect: true });
        x += PITCH;
    }

    // 3. Ramp down.
    {
        source(&mut g, x);
        g.force((x, -1, 0), Block::Solid(Material::Wire));
        g.force((x, 0, 0), Block::Dust { power: 0 });
        for i in 1..=3 {
            g.force((x, -1 - i, i), Block::Solid(Material::Wire));
            g.force((x, -i, i), Block::Dust { power: 0 });
        }
        let lamp = probe(&mut g, (x, -4, 4));
        cases.push(Case { name: "ramp_down_3", lamp, expect: true });
        x += PITCH;
    }

    // 4. Ramp up with the lower dust roofed: the climb must NOT form.
    {
        source(&mut g, x);
        g.force((x, -1, 0), Block::Solid(Material::Wire));
        g.force((x, 0, 0), Block::Dust { power: 0 });
        g.force((x, 1, 0), Block::Solid(Material::Shield)); // roof over lower dust
        g.force((x, 0, 1), Block::Solid(Material::Wire));
        g.force((x, 1, 1), Block::Dust { power: 0 });
        g.force((x, 0, 2), Block::Solid(Material::Wire));
        g.force((x, 1, 2), Block::Dust { power: 0 });
        // Probe well clear of the climb. Two earlier versions of this case put
        // the lamp within reach of a block the *lower* dust points into: dust
        // with a single connection renders as a line and powers the block at
        // either end, and a powered block lights an adjacent lamp. Both times
        // the lamp lit for reasons that had nothing to do with the climb.
        g.force((x, 0, 3), Block::Solid(Material::Wire));
        let lamp = probe(&mut g, (x, 1, 3));
        cases.push(Case { name: "ramp_up_roofed_blocked", lamp, expect: false });
        x += PITCH;
    }

    // 5. dust -> solid block -> dust: weak power must not cross.
    {
        source(&mut g, x);
        g.force((x, -1, 0), Block::Solid(Material::Wire));
        g.force((x, 0, 0), Block::Dust { power: 0 });
        g.force((x, 0, 1), Block::Solid(Material::Gate)); // in the way
        g.force((x, -1, 2), Block::Solid(Material::Wire));
        let lamp = probe(&mut g, (x, 0, 2));
        cases.push(Case { name: "block_gap_blocked", lamp, expect: false });
        x += PITCH;
    }

    // 6. Repeater driving dust, the way a gate feeds its pad.
    {
        source(&mut g, x);
        g.force((x, -1, 0), Block::Solid(Material::Wire));
        g.force((x, 0, 0), Block::Repeater { facing: Dir::North, delay: 1, powered: false });
        g.force((x, -1, 1), Block::Solid(Material::Wire));
        let lamp = probe(&mut g, (x, 0, 1));
        cases.push(Case { name: "repeater_drives_dust", lamp, expect: true });
        x += PITCH;
    }

    // 7. The gate itself: dust on a block, torch on its side, output beside it.
    {
        source(&mut g, x);
        g.force((x, -1, 0), Block::Solid(Material::Gate)); // torch support
        g.force((x, 0, 0), Block::Dust { power: 0 }); // pad (driven high)
        g.force((x, 0, 1), Block::Solid(Material::Shield)); // roof-ish spacer
        g.force((x, -1, 1), Block::WallTorch { facing: Dir::South, lit: true });
        g.force((x, -2, 2), Block::Solid(Material::Wire));
        let lamp = probe(&mut g, (x, -1, 2));
        // Input is high, so the torch is out and the output must be dark.
        cases.push(Case { name: "nor_cell_input_high", lamp, expect: false });
    }

    // --- our simulator's answer ---------------------------------------------
    let mut sim = Sim::new(&g);
    let (_, stable) = sim.run_until_stable(400);
    eprintln!("simulator stable = {stable}");
    let lo = g.bounds().map(|(lo, _)| lo).unwrap();
    let mut manifest = String::new();
    for c in &cases {
        let got = sim.lamp_lit(c.lamp);
        let mark = if got == c.expect { "ok " } else { "BAD" };
        eprintln!("  {mark} {:<28} sim={got:<5} expected={}", c.name, c.expect);
        // Coordinates in the mcfunction frame, for the in-game probe.
        manifest.push_str(&format!(
            "CASE {} {} {} {} {}\n",
            c.name,
            c.lamp.0 - lo.0,
            c.lamp.1 - lo.1 + 1,
            c.lamp.2 - lo.2,
            c.expect
        ));
    }
    std::fs::write(format!("{out}.manifest"), manifest).unwrap();
    std::fs::write(&out, to_mcfunction(&g, (0, 1, 0))).unwrap();
    eprintln!("wrote {out} and {out}.manifest");
}
