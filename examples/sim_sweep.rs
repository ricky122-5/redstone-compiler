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
    let comb = bitblast::blast_combinational(&design).unwrap();
    let lay = layout::build(&comb).unwrap();

    let levers: Vec<_> = lay.input_levers.iter().flat_map(|(_, v)| v.clone()).collect();
    let gsim = GateSim::new(&comb);
    let mut fails = 0;
    for v in 0..(1u64 << levers.len()) {
        let mut sim = Sim::new(&lay.grid);
        for (i, &l) in levers.iter().enumerate() {
            sim.set_lever(l, (v >> i) & 1 == 1);
        }
        let (_, stable) = sim.run_until_stable(2000);

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
    println!("{}", if fails == 0 { "layout simulates correctly".into() } else { format!("{fails} mismatch(es)") });
}
