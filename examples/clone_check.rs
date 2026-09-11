//! Does cloning high-fan-out gates preserve the function?
//!
//! Blasts the design twice, clones one copy, and drives both through the gate
//! simulator the way the hardware runs - one reset step, then clock until done -
//! comparing every output and `done` on every cycle. A clone that loses a reader
//! shows up here as a divergence at a specific cycle, long before it could show
//! up as a wrong lamp.
use ohmc::netlist::GateSim;
use ohmc::{bitblast, lower, parser};

fn main() {
    let path = std::env::args().nth(1).unwrap_or("examples/gcd.ohm".into());
    let t: usize = std::env::var("OHMC_CLONE").ok().and_then(|v| v.parse().ok()).unwrap_or(12);
    let src = std::fs::read_to_string(&path).unwrap();
    let design = lower::lower_program(&parser::parse(&src).unwrap()).unwrap();
    let a = bitblast::blast(&design);
    let mut b = bitblast::blast(&design);
    let before = b.max_nor_fanout();
    let made = b.clone_high_fanout(t);
    println!("{path}: threshold {t}, {made} copies, max NOR fan-out {before} -> {}, gates {} -> {}",
        b.max_nor_fanout(), a.gate_count(), b.gate_count());

    let widths: Vec<u32> = design.inputs.iter().map(|p| p.width).collect();
    let total: u32 = widths.iter().sum();
    let seeds: Vec<u64> = vec![48 | (18 << 8), 18 | (48 << 8), 255 | (1 << 8), 7 | (7 << 8),
        0 | (5 << 8), 100 | (75 << 8), 1, 2, 3, 0x5a3c, 0xffff, 0x1234, 0x8001];
    let mut bad = 0;
    for seed in seeds {
        let v = if total >= 64 { seed } else { seed & ((1u64 << total) - 1) };
        let mut vals = Vec::new();
        let mut rest = v;
        for &w in &widths { vals.push(rest & ((1u64 << w) - 1)); rest >>= w; }
        let (mut sa, mut sb) = (GateSim::new(&a), GateSim::new(&b));
        sa.step(&vals, true);
        sb.step(&vals, true);
        let mut cycle = 0;
        let mut diverged = None;
        while cycle < 5000 {
            let (va, vb) = (sa.eval(&vals, false), sb.eval(&vals, false));
            let oa: Vec<u64> = a.outputs.iter().map(|(n, _)| sa.read_output(n, &vals, false)).collect();
            let ob: Vec<u64> = b.outputs.iter().map(|(n, _)| sb.read_output(n, &vals, false)).collect();
            let (da, db) = (va[a.done as usize], vb[b.done as usize]);
            if oa != ob || da != db { diverged = Some((cycle, oa, ob, da, db)); break; }
            if da { break; }
            sa.step(&vals, false);
            sb.step(&vals, false);
            cycle += 1;
        }
        match diverged {
            None => println!("  inputs {vals:?}: agree for {cycle} cycles, outputs {:?}",
                a.outputs.iter().map(|(n, _)| sa.read_output(n, &vals, false)).collect::<Vec<_>>()),
            Some((c, oa, ob, da, db)) => { bad += 1;
                println!("  inputs {vals:?}: DIVERGE at cycle {c}: {oa:?}/{da} vs {ob:?}/{db}"); }
        }
    }
    println!("{}", if bad == 0 { "EQUIVALENT on every vector" } else { "NOT EQUIVALENT" });
}
