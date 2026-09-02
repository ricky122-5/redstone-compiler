//! What do the register bank's control pads actually read once placed?
//!
//! `tick` rings with every lever held still, and the gate array's drive graph
//! matches the netlist exactly - so the oscillator is not in the logic. The
//! remaining candidate is a latch stuck transparent: if a flip-flop's enable
//! pad reads high when the clock lever says low, then Q follows D, and D comes
//! back through the gate array from Q. That is a free-running loop, and it does
//! not care where any lever is - which is exactly the symptom.
//!
//! So read the pads rather than reasoning about them.
use ohmc::redstone::Sim;
use ohmc::{bitblast, layout, lower, parser};

fn main() {
    let path = std::env::args().nth(1).unwrap_or("examples/tick.ohm".into());
    let src = std::fs::read_to_string(&path).unwrap();
    let prog = parser::parse(&src).unwrap();
    let design = lower::lower_program(&prog).unwrap();
    let net = bitblast::blast(&design);
    let lay = layout::build(&net).expect("must place");
    let flops = &lay.flop_ports;

    for (clk, clkn, rst, label) in [
        (false, true, false, "clk=0 clkn=1 rst=0"),
        (true, false, false, "clk=1 clkn=0 rst=0"),
        (false, true, true, "clk=0 clkn=1 rst=1"),
    ] {
        let mut sim = Sim::new(&lay.grid);
        for (_, v) in &lay.input_levers {
            for &l in v {
                sim.set_lever(l, false);
            }
        }
        sim.set_lever(lay.clk_lever.unwrap(), clk);
        sim.set_lever(lay.clk_n_lever.unwrap(), clkn);
        sim.set_lever(lay.rst_lever.unwrap(), rst);
        let (t, ok) = sim.run_until_stable(600);
        println!("\n{label}  (settled={ok} after {t})");
        println!("  {:>3} {:>5} {:>6} {:>5} {:>5} {:>5}", "ff", "D", "EN", "EN_n", "CLR", "Q");
        let f = sim.field();
        for (i, p) in flops.iter().enumerate() {
            println!(
                "  {i:>3} {:>5} {:>6} {:>5} {:>5} {:>5}",
                f.dust_at(p.d_feeds[0]),
                f.dust_at(p.clk_feeds[0]),
                f.dust_at(p.clk_n_feeds[0]),
                p.clr_feeds.iter().map(|&c| f.dust_at(c)).max().unwrap_or(0),
                f.dust_at(p.q),
            );
        }
    }
}
