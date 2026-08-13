use ohmc::tech::stamp_d_latch;
use ohmc::world::{offset, up, down, Block, Dir, Grid};
fn main() {
    let mut g = Grid::new();
    let p = stamp_d_latch(&mut g, (0, 0, 0)).unwrap();
    let mut seen = std::collections::HashSet::new();
    let mut stack = vec![p.en];
    let mut cells = Vec::new();
    while let Some(q) = stack.pop() {
        if !seen.insert(q) { continue; }
        match g.get(q) {
            Block::Dust { .. } => {
                cells.push(q);
                for d in Dir::ALL {
                    let n = offset(q, d);
                    stack.push(n); stack.push(up(n)); stack.push(down(n));
                }
            }
            Block::Repeater { facing, .. } => { cells.push(q); stack.push(offset(q, facing.opposite())); }
            _ => {}
        }
    }
    cells.sort_by_key(|&(x, y, z)| (z, x, y));
    for (i, c) in cells.iter().enumerate() {
        println!("{i:>2} {c:?} {:?} above={:?} below={:?}", g.get(*c), g.get(up(*c)), g.get(down(*c)));
    }
    println!("en_a feed {:?}  en_b feed {:?}  en port {:?}", p.en_a, p.en_b, p.en);
}
