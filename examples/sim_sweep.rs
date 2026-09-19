//! Sweep every input combination through the *placed* layout in the block
//! simulator and diff against the gate-level model. Tells you whether a
//! disagreement with the real game is a layout bug we can see locally, or a
//! divergence between our redstone model and Minecraft's.
use ohmc::netlist::GateSim;
use ohmc::redstone::Sim;
use ohmc::{bitblast, layout, lower, parser};

fn main() {
    let path = std::env::args().nth(1).unwrap_or_else(|| "examples/add2.ohm".into());
    let src = std::fs::read_to_string(&path).unwrap();
    let design = lower::lower_program(&parser::parse(&src).unwrap()).unwrap();
    let mut comb = bitblast::blast_combinational(&design).unwrap();
    // The same netlist splitting `main` applies, so this sweeps the design the
    // compiler actually places rather than one that shares a source file.
    if let Some(t) = std::env::var("OHMC_CLONE").ok().and_then(|v| v.parse::<usize>().ok()) {
        eprintln!("cloned {} gate copy(s)", comb.clone_high_fanout(t));
    }
    if let Some(t) = std::env::var("OHMC_BUFFER").ok().and_then(|v| v.parse::<usize>().ok()) {
        eprintln!("buffered {} reader group(s)", comb.buffer_high_fanout(t));
    }
    let lay = layout::build(&comb).unwrap();

    let levers: Vec<_> = lay.input_levers.iter().flat_map(|(_, v)| v.clone()).collect();
    let gsim = GateSim::new(&comb);
    let mut fails = 0;
    let mut worst = 0u64;
    // ONE simulator across every case, with the levers toggled between them -
    // exactly how the real game is driven. Building a fresh Sim per case starts
    // from a clean slate every time and so cannot observe latch-up: a circuit
    // that answers correctly from reset but sticks once its inputs have moved
    // looks perfect. That is precisely the failure the game showed.
    let mut sim = Sim::new(&lay.grid);
    // A full sweep is 2^levers, which is fine for a two-bit adder and
    // 262144 cases for `alu`. `OHMC_CASES` names how many to run - still
    // driven through one simulator in sequence, so latch-up is still
    // observable - and `OHMC_CASE_STEP` spreads them over the input space
    // instead of taking a prefix that never moves the high bits.
    let cases: u64 = std::env::var("OHMC_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1u64 << levers.len())
        .min(1u64 << levers.len());
    let step: u64 =
        std::env::var("OHMC_CASE_STEP").ok().and_then(|v| v.parse().ok()).unwrap_or(1).max(1);
    let span = 1u64 << levers.len();
    for k in 0..cases {
        let v = k.wrapping_mul(step) % span;
        for (i, &l) in levers.iter().enumerate() {
            sim.set_lever(l, (v >> i) & 1 == 1);
        }
        let (ticks, stable) = sim.run_until_stable(4000);
        worst = worst.max(ticks);
        if ticks > 40 {
            println!("  in={v:<3} settled after {ticks} redstone ticks ({:.1}s in game)", ticks as f64 * 0.1);
        }

        // Split the flat counter across ports the same way `--truth` does.
        let mut vals = Vec::new();
        let mut rest = v;
        for p in &design.inputs {
            vals.push(rest & ((1u64 << p.width) - 1));
            rest >>= p.width;
        }
        for (name, lamps) in &lay.output_lamps {
            let got: u64 = lamps
                .iter()
                .enumerate()
                .map(|(i, &p)| (sim.lamp_lit(p) as u64) << i)
                .sum();
            let want = gsim.read_output(name, &vals, false);
            if got != want || !stable {
                fails += 1;
                println!("  in={v:<3} {name}: sim-of-layout={got} gate-model={want} stable={stable}");
            }
        }
    }
    println!("worst-case settling: {worst} redstone ticks = {:.1}s in game", worst as f64 * 0.1);
    println!("{}", if fails == 0 { "layout simulates correctly".into() } else { format!("{fails} mismatch(es)") });
}
