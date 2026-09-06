//! Is the compiler reproducible? Same source in, same bytes out?
//!
//! It was not, and nothing said so: three compiles of `add2.ohm` produced three
//! different schematics. Rust seeds its default hasher per process, so anything
//! that iterates a `HashMap` and acts on the order is a fresh coin flip each
//! run. Two causes, both now fixed - `Router::first_conflict` reported whichever
//! self-collision the iterator reached first, so retries took different paths;
//! and `schem.rs` numbered its palette in iteration order, so identical blocks
//! serialised to different bytes.
//!
//! Reproducibility is not tidiness here. Without it a routing failure cannot be
//! reproduced, a bisect is meaningless, and there is no way to tell whether a
//! change altered the build or merely reshuffled it.
//!
//! Each stage is checked by something that cannot lie about ordering: counts and
//! structure for the front end, sorted cells for the layout, and the actual
//! serialised bytes at the end.
use ohmc::{bitblast, layout, lower, parser, schem};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

fn digest<T: Hash>(t: &T) -> u64 {
    let mut h = DefaultHasher::new();
    t.hash(&mut h);
    h.finish()
}

fn main() {
    let path = std::env::args().nth(1).unwrap_or_else(|| "examples/add2.ohm".into());
    let src = std::fs::read_to_string(&path).expect("read source");
    let reps: usize = std::env::var("OHMC_REPS").ok().and_then(|v| v.parse().ok()).unwrap_or(3);

    let names = ["design (IR shape)", "netlist (gates)", "layout (blocks)", "schematic (bytes)"];
    let mut stages: Vec<Vec<u64>> = vec![Vec::new(); names.len()];

    for _ in 0..reps {
        let program = parser::parse(&src).expect("parse");
        let design = lower::lower_program(&program).expect("lower");
        stages[0].push(digest(&(
            design.inputs.len(),
            design.outputs.len(),
            design.blocks.len(),
            design.regs.len(),
            design.node_count(),
        )));

        let net = bitblast::blast(&design);
        // The netlist's own shape, read through its gates rather than its
        // `Debug` text: every gate's operands, in signal order.
        let mut gates: Vec<(u32, Vec<u32>)> = (0..net.sigs.len() as u32)
            .map(|s| (s, net.operands(s).to_vec()))
            .collect();
        gates.sort();
        stages[1].push(digest(&(gates, net.dffs.len(), net.gate_count(), net.logic_depth())));

        match layout::build(&net) {
            Ok(l) => {
                let mut cells: Vec<(ohmc::world::Pos, String)> =
                    l.grid.iter().map(|(&p, b)| (p, format!("{b:?}"))).collect();
                cells.sort();
                stages[2].push(digest(&cells));
                let bytes = schem::Schematic::from_grid(&l.grid).to_bytes().expect("serialise");
                stages[3].push(digest(&bytes));
            }
            Err(e) => {
                println!("layout failed: {e}");
                stages[2].push(0);
                stages[3].push(0);
            }
        }
    }

    let mut ok = true;
    for (name, hs) in names.iter().zip(&stages) {
        let same = hs.windows(2).all(|w| w[0] == w[1]);
        ok &= same;
        println!("{name:<20} {}", if same { "stable".to_string() } else { format!("VARIES {hs:?}") });
    }
    println!("\n{}", if ok { "reproducible" } else { "NOT REPRODUCIBLE" });
}
