//! Walk the D latch's dead fan-out leg cell by cell.
//!
//! The leg from the D port to `r_gate` is correct in simulation and dead in
//! Minecraft. Every indirect hypothesis has failed - decay, locking, diagonal
//! links, connectivity rules - so this prints the wire itself: each cell it
//! occupies, what block is there, what is above and below it, and the level the
//! simulator gives it. A step that is legal here and illegal in game has to be
//! visible in that list.
use ohmc::redstone::Sim;
use ohmc::tech::stamp_d_latch;
use ohmc::world::{down, up, Block, Dir, Face, Grid, Material, Pos};

fn drive(g: &mut Grid, feed: Pos) -> Pos {
    let (x, y, z) = feed;
    g.force((x, y - 1, z), Block::Solid(Material::Wire));
    g.force((x, y, z), Block::Dust { power: 0 });
    g.force((x, y - 1, z - 1), Block::Solid(Material::Wire));
    g.force((x, y, z - 1), Block::Dust { power: 0 });
    let l = (x, y, z - 2);
    g.force((x, y - 1, z - 2), Block::Solid(Material::PortIn));
    g.force(l, Block::Lever { face: Face::Floor, facing: Dir::North, powered: false });
    l
}

fn main() {
    let mut g = Grid::new();
    let p = stamp_d_latch(&mut g, (0, 0, 0)).unwrap();
    let dl = drive(&mut g, p.d);
    let el = drive(&mut g, p.en);

    let mut sim = Sim::new(&g);
    sim.set_lever(dl, true);
    sim.set_lever(el, true);
    sim.run_until_stable(5000);
    let f = sim.field();

    // Greedy descent from the target back towards the source: at each step move
    // to the neighbouring dust with the highest level. That retraces the path
    // the signal actually took.
    let target = p.d_b; // r_gate's D feed - the dead one
    println!("tracing back from r_gate D feed {target:?} (level {})", f.dust_at(target));
    println!("also: not_d feed {:?} (level {})", p.d_a, f.dust_at(p.d_a));
    println!("source: D port {:?} (level {})\n", p.d, f.dust_at(p.d));

    let mut at = target;
    let mut seen = vec![at];
    for _ in 0..200 {
        let here = f.dust_at(at);
        let mut best: Option<(Pos, u8)> = None;
        for dx in -1..=1i32 {
            for dy in -1..=1i32 {
                for dz in -1..=1i32 {
                    if dx.abs() + dy.abs() + dz.abs() != 1 && (dx, dy, dz) != (0, 0, 0) {
                        // allow the diagonal steps a wire uses to change level
                        if dx.abs() + dz.abs() != 1 || dy == 0 {
                            continue;
                        }
                    }
                    let n = (at.0 + dx, at.1 + dy, at.2 + dz);
                    if seen.contains(&n) || !matches!(g.get(n), Block::Dust { .. }) {
                        continue;
                    }
                    let lv = f.dust_at(n);
                    if lv > here && best.map_or(true, |(_, b)| lv > b) {
                        best = Some((n, lv));
                    }
                }
            }
        }
        match best {
            Some((n, _)) => {
                at = n;
                seen.push(n);
            }
            None => break,
        }
    }

    seen.reverse();
    println!("{:<16} {:>4}  {:<28} {:<20} {}", "cell", "lvl", "block", "above", "below");
    let mut prev: Option<Pos> = None;
    for c in &seen {
        let step = prev.map(|q: Pos| (c.0 - q.0, c.1 - q.1, c.2 - q.2));
        let flag = match step {
            // A Y change is where the game's step rules apply, so mark them.
            Some((_, dy, _)) if dy != 0 => "  <-- Y STEP",
            _ => "",
        };
        println!(
            "{:<16} {:>4}  {:<28} {:<20} {:?}{}",
            format!("{:?}", c),
            f.dust_at(*c),
            format!("{:?}", g.get(*c)),
            format!("{:?}", g.get(up(*c))),
            g.get(down(*c)),
            flag
        );
        prev = Some(*c);
    }
    println!("\n{} cells on the path", seen.len());
}
