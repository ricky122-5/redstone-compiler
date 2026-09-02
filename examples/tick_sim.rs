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
    let (clk, clkn) = (lay.clk_lever.unwrap(), lay.clk_n_lever.unwrap());
    // Both resets, pulsed together. `rst_lever` clears the flip-flops;
    // `net_reset_lever` drives the netlist's own `Src::Reset`, which is what
    // forces the one-hot state vector to the entry block. Clearing without it
    // leaves every state bit at zero, so no block is active and the machine
    // sits there for ever - which looks exactly like a dead circuit.
    let rsts: Vec<Pos> = [lay.rst_lever, lay.net_reset_lever].into_iter().flatten().collect();
    println!("reset levers: {rsts:?}");

    for v in 0..(1usize << levers.len()).min(4) {
        let mut sim = Sim::new(&lay.grid);
        // Inputs first, so they are stable across the whole run.
        for (b, &l) in levers.iter().enumerate() {
            sim.set_lever(l, (v >> b) & 1 == 1);
        }
        // The two resets are different mechanisms and need different handling.
        //
        // `rst_lever` is the flip-flops' *asynchronous* clear: assert it and
        // every register goes to zero immediately, no clock required. That is
        // what puts the build into a known state after placement, since the
        // lit/powered flags chosen at stamp time do not survive setblock.
        //
        // `net_reset_lever` drives the netlist's `Src::Reset`, and that one is
        // *synchronous*: all it does is make the entry block's D input high. It
        // has to be clocked in. Holding both with the clock still - which is the
        // obvious thing to do, and what this harness did - clears the state
        // vector to zero and then never loads anything, so no block is ever
        // active and the machine sits at Q=00000000000 for ever while settling
        // perfectly at every step.
        //
        // So: clear asynchronously, release, then assert the synchronous reset
        // and clock it in.
        let clear = |sim: &mut Sim, on: bool| {
            if let Some(r) = lay.rst_lever {
                sim.set_lever(r, on);
            }
        };
        let tick = |sim: &mut Sim| -> (u64, bool) {
            sim.set_lever(clk, true);
            sim.set_lever(clkn, false);
            let (a, oka) = sim.run_until_stable(budget);
            sim.set_lever(clk, false);
            sim.set_lever(clkn, true);
            let (b, okb) = sim.run_until_stable(budget);
            (a + b, oka && okb)
        };
        sim.set_lever(clk, false);
        sim.set_lever(clkn, true);
        clear(&mut sim, true);
        let (rt, ok_r) = sim.run_until_stable(budget);
        clear(&mut sim, false);
        let (rt2, ok_r2) = sim.run_until_stable(budget);
        eprintln!("  async clear: {rt}/{rt2} ticks, settled={ok_r}/{ok_r2}");
        // Now load the entry state: hold Src::Reset and clock once.
        if let Some(r) = lay.net_reset_lever {
            sim.set_lever(r, true);
        }
        let (lt, ok_l) = tick(&mut sim);
        if let Some(r) = lay.net_reset_lever {
            sim.set_lever(r, false);
        }
        let (lt2, ok_l2) = sim.run_until_stable(budget);
        eprintln!("  state load:  {lt}/{lt2} ticks, settled={ok_l}/{ok_l2}");

        let read = |sim: &Sim| -> usize {
            let f = sim.field();
            lamps.iter().enumerate().fold(0, |a, (i, &p)| a | ((f.block_powered(p) as usize) << i))
        };

        let mut trace = Vec::new();
        let mut settled = true;
        for c in 0..cycles {
            let t = std::time::Instant::now();
            let (ticks, ok) = tick(&mut sim);
            settled &= ok;
            let now = read(&sim);
            // The state register itself, so a machine that is not advancing is
            // visible as a stuck vector rather than as a lamp that never lights.
            let f = sim.field();
            let state: String = lay
                .flop_ports
                .iter()
                .map(|p| if f.dust_at(p.q) > 0 { '1' } else { '0' })
                .collect();
            eprintln!("  cycle {c:>3}: lamps={now} Q={state} ticks={ticks} settled={ok} {:?}", t.elapsed());
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
