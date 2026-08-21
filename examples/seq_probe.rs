//! Where exactly does a register bank's Q sit relative to the logic it feeds?
//!
//! The sequential placer fails on one connection with an 82-block span. Which
//! axis that span is in decides the fix - staging keys off the vertical drop,
//! so a span that is mostly horizontal is simply never broken up.
use ohmc::netlist::Netlist;
use ohmc::tech::stamp_dff;
use ohmc::world::Grid;

fn main() {
    // The smallest real sequential circuit: a flop fed its own inverted output.
    let mut net = Netlist::new();
    let (idx, q) = net.add_dff("r");
    let nq = net.nor(&[q]);
    net.set_dff_d(idx, nq);
    net.outputs.push(("q".into(), vec![q]));

    // Where the macro puts its ports, measured rather than assumed.
    let mut g = Grid::new();
    let p = stamp_dff(&mut g, (0, 0, 0)).unwrap();
    let (lo, hi) = g.bounds().unwrap();
    println!("one flip-flop spans {lo:?} .. {hi:?}");
    println!("  q      {:?}", p.q);
    println!("  q_port {:?}   (dx from macro origin: {})", p.q_port, p.q_port.0);
    println!("  d      {:?}", p.d_feeds[0]);

    for s in 0..net.sigs.len() as u32 {
        println!("sig {s}: {:?}", net.src(s));
    }
}
