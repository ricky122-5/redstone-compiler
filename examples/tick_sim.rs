//! Does the *placed* tick.ohm actually compute?
//!
//! Placement is not correctness. `tick` is the first design with a real FSM to
//! get through the placer, so this drives the built circuit the way a player
//! would - pulse reset, then work the clock levers - and compares what the lamps
//! say against the golden model.
use ohmc::redstone::Sim;
use ohmc::world::Pos;
use ohmc::{bitblast, layout, lower, machine, parser};

fn main() {
    let path = std::env::args().nth(1).unwrap_or("examples/tick.ohm".into());
    let cycles: usize =
        std::env::var("OHMC_CYCLES").ok().and_then(|v| v.parse().ok()).unwrap_or(40);
    let budget: u64 =
        std::env::var("OHMC_BUDGET").ok().and_then(|v| v.parse().ok()).unwrap_or(40000);
    let src = std::fs::read_to_string(&path).unwrap();
    let prog = parser::parse(&src).unwrap();
    let design = lower::lower_program(&prog).unwrap();
    let net = bitblast::blast(&design);
    let lay = layout::build(&net).expect("must place");
    println!(
        "placed: {} flops, {} levers, {} lamps",
        lay.flops,
        lay.input_levers.iter().map(|(_, v)| v.len()).sum::<usize>(),
        lay.output_lamps.iter().map(|(_, v)| v.len()).sum::<usize>()
    );

    let levers: Vec<Pos> = lay.input_levers.iter().flat_map(|(_, v)| v.clone()).collect();
    let lamps: Vec<Pos> = lay.output_lamps.iter().flat_map(|(_, v)| v.clone()).collect();
    let (clk, clkn, rst) =
        (lay.clk_lever.unwrap(), lay.clk_n_lever.unwrap(), lay.rst_lever.unwrap());

    for v in 0..(1usize << levers.len()).min(4) {
        let mut sim = Sim::new(&lay.grid);
        // Inputs first, so they are stable across the whole run.
        for (b, &l) in levers.iter().enumerate() {
            sim.set_lever(l, (v >> b) & 1 == 1);
        }
        // Pulse reset with the clock low: state after placement is whatever
        // placement left, so it has to be driven in.
        sim.set_lever(clk, false);
        sim.set_lever(clkn, true);
        sim.set_lever(rst, true);
        let t0 = std::time::Instant::now();
        let (rt, ok_r) = sim.run_until_stable(budget);
        eprintln!("  reset assert: {rt} ticks, settled={ok_r}, {:?}", t0.elapsed());
        sim.set_lever(rst, false);
        let t1 = std::time::Instant::now();
        let (rt2, ok_r2) = sim.run_until_stable(budget);
        eprintln!("  reset release: {rt2} ticks, settled={ok_r2}, {:?}", t1.elapsed());

        let read = |sim: &Sim| -> usize {
            let f = sim.field();
            lamps.iter().enumerate().fold(0, |a, (i, &p)| a | ((f.block_powered(p) as usize) << i))
        };

        let mut trace = Vec::new();
        let mut settled = true;
        for c in 0..cycles {
            let t = std::time::Instant::now();
            // Master transparent, slave closed.
            sim.set_lever(clk, true);
            sim.set_lever(clkn, false);
            let (a, oka) = sim.run_until_stable(budget);
            // Falling edge: the slave takes the bit.
            sim.set_lever(clk, false);
            sim.set_lever(clkn, true);
            let (b, okb) = sim.run_until_stable(budget);
            settled &= oka && okb;
            let now = read(&sim);
            eprintln!(
                "  cycle {c:>3}: lamps={now} ticks={a}/{b} settled={} {:?}",
                oka && okb,
                t.elapsed()
            );
            trace.push(now);
        }

        // Golden model.
        let refs: Vec<(&str, u64)> = design
            .inputs
            .iter()
            .enumerate()
            .map(|(i, p)| (p.name.as_str(), ((v >> i) & 1) as u64))
            .collect();
        let want = machine::run_design(&design, &refs, 1_000_000)
            .map(|(m, c)| {
                let outs: Vec<String> = design
                    .outputs
                    .iter()
                    .map(|p| format!("{}={}", p.0, m.output(&design, &p.0).unwrap_or(0)))
                    .collect();
                format!("{} after {c} cycles", outs.join(" "))
            })
            .unwrap_or_else(|e| format!("model error: {e}"));

        let last = *trace.last().unwrap();
        let settledness = if ok_r && ok_r2 && settled { "" } else { "  [UNSTABLE]" };
        println!("\ninput {v}: lamps -> {last}   want: {want}{settledness}");
        // Print where it changes, so a run that is merely slow is visible.
        let mut prev = usize::MAX;
        for (i, &t) in trace.iter().enumerate() {
            if t != prev {
                println!("   cycle {i:>3}: {t}");
                prev = t;
            }
        }
    }
}
