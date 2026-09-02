//! What is oscillating in the placed sequential circuit?
//!
//! `tick` places, and then will not settle: reset asserted, the simulator runs
//! its whole budget with something still toggling. A ring is a wiring fault, not
//! a logic one, so the useful question is *which* cells move, and whether they
//! belong to one net (a route that closed a loop on itself) or to several (two
//! nets shorted together).
//!
//! Torch burnout is the cheap instrument for this. A torch only burns out if it
//! toggled eight times inside thirty ticks, which is precisely what a ring makes
//! it do - so the burnt-out list *is* the ring, and it costs nothing to read,
//! where sampling every cell means recomputing a 60,000-block field every tick.
use ohmc::redstone::Sim;
use ohmc::world::Pos;
use ohmc::{bitblast, layout, lower, parser};

fn main() {
    let path = std::env::args().nth(1).unwrap_or("examples/tick.ohm".into());
    let warm: u64 = std::env::var("OHMC_WARM").ok().and_then(|v| v.parse().ok()).unwrap_or(300);
    let src = std::fs::read_to_string(&path).unwrap();
    let prog = parser::parse(&src).unwrap();
    let design = lower::lower_program(&prog).unwrap();
    let net = bitblast::blast(&design);
    let t0 = std::time::Instant::now();
    let lay = layout::build(&net).expect("must place");
    eprintln!("placed in {:?}", t0.elapsed());

    // Which static lever setting rings? If the circuit settles with everything
    // held still and only misbehaves when reset moves, the fault is in the reset
    // distribution; if it rings with every lever static, it is a combinational
    // wiring fault and the clock has nothing to do with it.
    let levers: Vec<Pos> = lay.input_levers.iter().flat_map(|(_, v)| v.clone()).collect();
    for (label, clk, clkn, rst) in [
        ("idle       (clk=0 clkn=1 rst=0)", false, true, false),
        ("reset held (clk=0 clkn=1 rst=1)", false, true, true),
        ("clk high   (clk=1 clkn=0 rst=0)", true, false, false),
        ("both clocks low", false, false, false),
    ] {
        let mut sim = Sim::new(&lay.grid);
        for &l in &levers {
            sim.set_lever(l, false);
        }
        sim.set_lever(lay.clk_lever.unwrap(), clk);
        sim.set_lever(lay.clk_n_lever.unwrap(), clkn);
        sim.set_lever(lay.rst_lever.unwrap(), rst);
        if let Some(r) = lay.net_reset_lever {
            sim.set_lever(r, rst);
        }
        let (t, ok) = sim.run_until_stable(warm);
        println!(
            "{label}: settled={ok} after {t} ticks, {} pending, {} torches burnt",
            sim.state.pending_len(),
            sim.burned_out().len()
        );
    }

    let mut sim = Sim::new(&lay.grid);
    for &l in &levers {
        sim.set_lever(l, false);
    }
    sim.set_lever(lay.clk_lever.unwrap(), false);
    sim.set_lever(lay.clk_n_lever.unwrap(), true);
    sim.set_lever(lay.rst_lever.unwrap(), true);
    if let Some(r) = lay.net_reset_lever {
        sim.set_lever(r, true);
    }
    let t1 = std::time::Instant::now();
    sim.run(warm);
    eprintln!("ran {warm} ticks in {:?}, {} still pending", t1.elapsed(), sim.state.pending_len());

    let burned = sim.burned_out();
    println!("{} torches burned out (i.e. toggling) after {warm} ticks", burned.len());

    // The cells with a transition still scheduled *are* the oscillator.
    println!("\npending transitions ({}):", sim.state.pending_len());
    for (p, target) in sim.state.pending_cells() {
        println!("  {p:?} -> {target}  {:?}  owner {:?}", lay.grid.get(p), lay.wire_owner.get(&p));
    }

    // Which nets do they belong to? A ring inside one net is a routing loop; a
    // ring spanning several is a short between nets. A torch is a gate's own
    // cell, so look at the neighbourhood rather than the torch itself.
    let owner_near = |p: Pos| -> Vec<u32> {
        let mut v: Vec<u32> = (-2..=2i32)
            .flat_map(|dx| (-2..=2i32).flat_map(move |dy| (-2..=2i32).map(move |dz| (dx, dy, dz))))
            .filter_map(|(dx, dy, dz)| lay.wire_owner.get(&(p.0 + dx, p.1 + dy, p.2 + dz)).copied())
            .collect();
        v.sort_unstable();
        v.dedup();
        v
    };
    println!("\nburning torches:");
    for &p in burned.iter().take(15) {
        println!("  {p:?}  nets nearby {:?}", owner_near(p));
    }
}
