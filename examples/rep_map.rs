use ohmc::tech::stamp_d_latch;
use ohmc::world::{Block, Grid};
fn main() {
    let mut g = Grid::new();
    let _ = stamp_d_latch(&mut g, (0, 0, 0)).unwrap();
    let mut reps: Vec<_> = g
        .iter()
        .filter_map(|(&p, &b)| match b {
            Block::Repeater { facing, .. } => Some((p, facing)),
            _ => None,
        })
        .collect();
    reps.sort_by_key(|&(p, _)| p);
    for (i, (p, f)) in reps.iter().enumerate() {
        println!("R{i:<3} {p:?} facing {f:?}");
    }
}
