//! Footprint of one flip-flop, so the register bank's pitch is measured rather
//! than remembered. The macro grew when boundary ports were added and the
//! bank's constants did not follow, which collided registers 4 and 5.
use ohmc::tech::stamp_dff;
use ohmc::world::Grid;
fn main() {
    let mut g = Grid::new();
    stamp_dff(&mut g, (0, 0, 0)).unwrap();
    let (lo, hi) = g.bounds().unwrap();
    println!("lo {lo:?}  hi {hi:?}");
    println!("extent x={} y={} z={}", hi.0 - lo.0 + 1, hi.1 - lo.1 + 1, hi.2 - lo.2 + 1);
    println!("blocks {}", g.len());
}
