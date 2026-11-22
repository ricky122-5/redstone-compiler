//! What sits around a few cells of a placed design, and which net owns each.
//! `OHMC_CELLS="x,y,z;x,y,z"` names the cells; prints the 3x3x3 block around
//! each with its block and owning net, so a wire that reads power its own net
//! does not have can be traced to the neighbour giving it.
use ohmc::{bitblast, layout, lower, parser};

fn main() {
    let path = std::env::args().nth(1).unwrap_or_else(|| "examples/gcd.ohm".into());
    let src = std::fs::read_to_string(&path).unwrap();
    let design = lower::lower_program(&parser::parse(&src).unwrap()).unwrap();
    let mut net = bitblast::blast_combinational(&design).unwrap_or_else(|_| bitblast::blast(&design));
    if let Some(t) = std::env::var("OHMC_CLONE").ok().and_then(|v| v.parse::<usize>().ok()) {
        net.clone_high_fanout(t);
    }
    if let Some(t) = std::env::var("OHMC_BUFFER").ok().and_then(|v| v.parse::<usize>().ok()) {
        net.buffer_high_fanout(t);
    }
    let lay = layout::build(&net).unwrap();
    let spec = std::env::var("OHMC_CELLS").unwrap_or_default();
    for c in spec.split(';').filter(|s| !s.is_empty()) {
        let v: Vec<i32> = c.split(',').map(|x| x.trim().parse().unwrap()).collect();
        let p = (v[0], v[1], v[2]);
        println!("== around {p:?}");
        for dz in [1, 0, -1] {
            for dy in -1..=1 {
                for dx in -1..=1 {
                    let q = (p.0 + dx, p.1 + dy, p.2 + dz);
                    let b = lay.grid.get(q);
                    if matches!(b, ohmc::world::Block::Air) {
                        continue;
                    }
                    let o = lay.wire_owner.get(&q).map(|o| o.to_string()).unwrap_or("-".into());
                    println!("  d=({dx:>2},{dy:>2},{dz:>2}) {q:?} net {o:>10} {b:?}");
                }
            }
        }
    }
}
