//! What surrounds each output lamp in a placed design, and who owns it?
//!
//! In game, a lamp lights if anything powers it: dust on top, dust pointing
//! into it, an adjacent strongly powered block, a repeater facing it. If any
//! of those cells belongs to a different net than the lamp's driver, the lamp
//! reads the wrong signal even though every gate is correct - which is exactly
//! what the add2 in-game run shows.
use ohmc::world::{down, offset, up, Block, Dir};
use ohmc::{bitblast, layout, lower, parser};

fn main() {
    let path = std::env::args().nth(1).unwrap_or_else(|| "examples/add2.ohm".into());
    let src = std::fs::read_to_string(&path).unwrap();
    let prog = parser::parse(&src).unwrap();
    let design = lower::lower_program(&prog).unwrap();
    // The netlist `main` places: sequential designs included, and split the
    // same way, so a sequential design's lamps can be inspected too.
    let mut net = bitblast::blast(&design);
    if let Some(t) = std::env::var("OHMC_CLONE").ok().and_then(|v| v.parse::<usize>().ok()) {
        net.clone_high_fanout(t);
    }
    if let Some(t) = std::env::var("OHMC_BUFFER").ok().and_then(|v| v.parse::<usize>().ok()) {
        net.buffer_high_fanout(t);
    }
    let lay = layout::build(&net).unwrap();
    let g = &lay.grid;

    for (name, lamps) in &lay.output_lamps {
        let bits = &net.outputs.iter().find(|(n, _)| n == name).unwrap().1;
        for (i, &lp) in lamps.iter().enumerate() {
            println!("\n{name}[{i}] lamp at {lp:?}, driven by net {}:", bits[i]);
            for c in Dir::ALL
                .iter()
                .map(|&d| offset(lp, d))
                .chain([up(lp), down(lp)])
            {
                let b = g.get(c);
                if matches!(b, Block::Air) {
                    continue;
                }
                println!("  {c:?} {b:?} owner={:?}", lay.wire_owner.get(&c));
                // A solid neighbour powers the lamp if anything powers *it*,
                // so its own surroundings are suspects too.
                if b.is_opaque() {
                    for q in Dir::ALL.iter().map(|&d| offset(c, d)).chain([up(c), down(c)]) {
                        let qb = g.get(q);
                        if q == lp || matches!(qb, Block::Air) {
                            continue;
                        }
                        println!("      {q:?} {qb:?} owner={:?}", lay.wire_owner.get(&q));
                    }
                }
            }
        }
    }

    // `OHMC_NET_DUMP=n,zmin` lists every cell net `n` owns past `zmin`, with
    // what is under it, so a wire that reaches its lamp but carries nothing
    // can be read hop by hop.
    if let Ok(spec) = std::env::var("OHMC_NET_DUMP") {
        let mut it = spec.split(',').map(|v| v.trim().parse::<i32>().unwrap());
        let (n, zmin) = (it.next().unwrap() as u32, it.next().unwrap_or(i32::MIN));
        let mut cells: Vec<_> =
            lay.wire_owner.iter().filter(|&(p, &o)| o == n && p.2 >= zmin).map(|(&p, _)| p).collect();
        cells.sort_by_key(|p| (p.2, p.0, p.1));
        println!("\nnet {n} past z={zmin}: {} cells", cells.len());
        for c in cells {
            println!("  {c:?} {:?}  under {:?}", g.get(c), g.get(down(c)));
        }
    }

    // `OHMC_PROBE_LEVER=x,y,z` swaps the block there - typically a relay's
    // repeater - for a lever that is on, settles, and reports power along the
    // dumped net and at every lamp. That drives one wire in isolation, with
    // nothing else in the build powered, so a lamp that stays dark is that
    // wire's own fault.
    // Several sites, `;`-separated, each probed on its own copy of the grid.
    for spec in std::env::var("OHMC_PROBE_LEVER").unwrap_or_default().split(';').filter(|s| !s.is_empty()) {
        use ohmc::redstone::Sim;
        use ohmc::world::Face;
        let v: Vec<i32> = spec.split(',').map(|t| t.trim().parse().unwrap()).collect();
        let at = (v[0], v[1], v[2]);
        let mut pg = g.clone();
        pg.force(at, Block::Lever { face: Face::Floor, facing: Dir::North, powered: true });
        let mut sim = Sim::new(&pg);
        sim.set_lever(at, true);
        let (ticks, ok) = sim.run_until_stable(40000);
        println!("\nprobe lever at {at:?}: {ticks} ticks, settled={ok}");
        let f = sim.field();
        if let (Ok(spec), true) = (std::env::var("OHMC_NET_DUMP"), std::env::var("OHMC_PROBE_CELLS").is_ok()) {
            let mut it = spec.split(',').map(|t| t.trim().parse::<i32>().unwrap());
            let (n, zmin) = (it.next().unwrap() as u32, it.next().unwrap_or(i32::MIN));
            let mut cells: Vec<_> = lay
                .wire_owner
                .iter()
                .filter(|&(p, &o)| o == n && p.2 >= zmin && (p.0 - at.0).abs() <= 16)
                .map(|(&p, _)| p)
                .collect();
            cells.sort_by_key(|p| (p.2, p.0, p.1));
            for c in cells {
                println!("  {c:?} {:?} power={}", pg.get(c), f.dust_at(c));
            }
        }
        for (name, lamps) in &lay.output_lamps {
            for (i, &lp) in lamps.iter().enumerate() {
                println!("  {name}[{i}] at {lp:?} lit={}", sim.lamp_lit(lp));
            }
        }
    }
}
