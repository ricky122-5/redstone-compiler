//! Where does a placed design's footprint actually go?
//!
//! The in-game harness can only force-load 256 chunks, which is a 256x256
//! square. A design wider than that has parts sitting in unticked chunks, where
//! commands still place blocks and probes still read them but redstone never
//! runs - and it reads as a fixed wrong answer rather than as a failure.
use ohmc::world::Pos;
use ohmc::{bitblast, layout, lower, parser};

fn main() {
    let path = std::env::args().nth(1).unwrap_or("examples/tick.ohm".into());
    let src = std::fs::read_to_string(&path).unwrap();
    let prog = parser::parse(&src).unwrap();
    let design = lower::lower_program(&prog).unwrap();
    let net = bitblast::blast_combinational(&design).unwrap_or_else(|_| bitblast::blast(&design));
    let lay = layout::build(&net).expect("must place");
    let (lo, hi) = lay.grid.bounds().unwrap();
    println!("whole build {lo:?} .. {hi:?}  = {} x {} x {}", hi.0 - lo.0 + 1, hi.1 - lo.1 + 1, hi.2 - lo.2 + 1);
    let chunks = ((hi.0 - lo.0) / 16 + 2) * ((hi.2 - lo.2) / 16 + 2);
    println!("~{chunks} chunks (forceload cap is 256)");

    let span = |name: &str, ps: Vec<Pos>| {
        if ps.is_empty() {
            return;
        }
        let (x0, x1) = (ps.iter().map(|p| p.0).min().unwrap(), ps.iter().map(|p| p.0).max().unwrap());
        let (z0, z1) = (ps.iter().map(|p| p.2).min().unwrap(), ps.iter().map(|p| p.2).max().unwrap());
        println!("  {name:<14} x {x0:>5}..{x1:<5} ({:>4})   z {z0:>5}..{z1:<5} ({:>4})", x1 - x0 + 1, z1 - z0 + 1);
    };
    span("gate outputs", lay.gate_cells.values().map(|(o, _)| *o).collect());
    span("gate feeds", lay.gate_cells.values().flat_map(|(_, f)| f.clone()).collect());
    span("flop Q", lay.flop_ports.iter().map(|f| f.q_port).collect());
    span("flop D pads", lay.flop_ports.iter().map(|f| f.d_feeds[0]).collect());
    span("routed wire", lay.wire_owner.keys().copied().collect());
    span("levers", lay.input_levers.iter().flat_map(|(_, v)| v.clone()).collect());

    // Gates per level, and how wide each row actually is. A level with many
    // gates sets the whole array's width on its own, and no amount of
    // re-centring can help that - the fix would be folding the row.
    let mut by_y: std::collections::BTreeMap<i32, Vec<Pos>> = Default::default();
    for (o, _) in lay.gate_cells.values() {
        by_y.entry(o.1).or_default().push(*o);
    }
    println!("\nper level:");
    for (y, ps) in by_y.iter().rev() {
        let (x0, x1) = (ps.iter().map(|p| p.0).min().unwrap(), ps.iter().map(|p| p.0).max().unwrap());
        println!("  y={y:>4}  {:>3} gates   x {x0:>4}..{x1:<4} span {:>4}", ps.len(), x1 - x0 + 1);
    }
}
