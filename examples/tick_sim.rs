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
    let mut net = bitblast::blast(&design);
    // The same netlist splitting `main` applies, so this simulates the design
    // the compiler actually places rather than a different one that happens to
    // share a source file.
    if let Some(t) = std::env::var("OHMC_CLONE").ok().and_then(|v| v.parse::<usize>().ok()) {
        eprintln!("cloned {} gate copy(s)", net.clone_high_fanout(t));
    }
    if let Some(t) = std::env::var("OHMC_BUFFER").ok().and_then(|v| v.parse::<usize>().ok()) {
        eprintln!("buffered {} reader group(s)", net.buffer_high_fanout(t));
    }
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

    // Which input vectors to drive. The default sweep is the first few, which
    // is right for a one-bit design and useless for one with sixteen levers:
    // `gcd` wants a=48,b=18, not a=0,b=0. `OHMC_SIM_V` names the vectors
    // outright, packed the way the levers are - port by port, bit 0 first.
    let vectors: Vec<usize> = match std::env::var("OHMC_SIM_V") {
        Ok(s) => s.split(',').filter_map(|v| v.trim().parse().ok()).collect(),
        Err(_) => (0..(1usize << levers.len()).min(4)).collect(),
    };
    for v in vectors {
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
            // The clock-high half of `OHMC_FLOP_PROBE`: what each named flop
            // sees while its master is meant to be open.
            if let Ok(fs) = std::env::var("OHMC_FLOP_PROBE") {
                let f = sim.field();
                for i in fs.split(',').filter_map(|v| v.parse::<usize>().ok()) {
                    let fp = &lay.flop_ports[i];
                    let pw = |v: &Vec<Pos>| v.iter().map(|&p| f.dust_at(p)).collect::<Vec<u8>>();
                    eprintln!(
                        "      [clk high] flop {i} d={:?} clk={:?} clk_n={:?} master_q={}",
                        pw(&fp.d_feeds), pw(&fp.clk_feeds), pw(&fp.clk_n_feeds), f.dust_at(fp.master_q)
                    );
                    // Every powered cell of the macro, relative to its D feed,
                    // so a dead flop can be diffed against a live one.
                    if std::env::var("OHMC_FLOP_CELLS").is_ok() {
                        let all: Vec<Pos> = [fp.d_feeds.clone(), fp.clk_feeds.clone(), fp.clk_n_feeds.clone(), fp.clr_feeds.clone(), vec![fp.q, fp.master_q, fp.q_port]].concat();
                        let lo = (all.iter().map(|p| p.0).min().unwrap() - 2, all.iter().map(|p| p.1).min().unwrap() - 4, all.iter().map(|p| p.2).min().unwrap() - 2);
                        let hi = (all.iter().map(|p| p.0).max().unwrap() + 2, all.iter().map(|p| p.1).max().unwrap() + 4, all.iter().map(|p| p.2).max().unwrap() + 2);
                        let o = fp.d_feeds[0];
                        for x in lo.0..=hi.0 { for y in lo.1..=hi.1 { for z in lo.2..=hi.2 {
                            let p = (x, y, z);
                            if matches!(lay.grid.get(p), ohmc::world::Block::Dust { .. }) && lay.wire_owner.get(&p).map_or(true, |&w| w == u32::MAX) {
                                eprintln!("        cell flop {i} rel ({},{},{}) power {}", x - o.0, y - o.1, z - o.2, f.dust_at(p));
                            }
                        }}}
                    }
                }
            }
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
            // `OHMC_FLOP_PROBE=i,j`: power at each named flop's D feeds, every
            // cycle - whether a flop that never loads is being given its 1.
            if let Ok(fs) = std::env::var("OHMC_FLOP_PROBE") {
                for i in fs.split(',').filter_map(|v| v.parse::<usize>().ok()) {
                    let fp = &lay.flop_ports[i];
                    let d: Vec<u8> = fp.d_feeds.iter().map(|&p| f.dust_at(p)).collect();
                    let pw = |v: &Vec<Pos>| v.iter().map(|&p| f.dust_at(p)).collect::<Vec<u8>>();
                    eprintln!(
                        "      flop {i} d {:?}={d:?} clk={:?} clk_n={:?} clr={:?} q={}",
                        fp.d_feeds, pw(&fp.clk_feeds), pw(&fp.clk_n_feeds), pw(&fp.clr_feeds), f.dust_at(fp.q)
                    );
                }
            }
            trace.push(now);
        }

        // Golden model, driven with the *same bits the levers got*.
        //
        // This gave each port one bit of the sweep value - port 0 gets bit 0,
        // port 1 gets bit 1 - which is right only when every port is one bit
        // wide. `tick.ohm`'s single `u1 go` is, so it went unnoticed;
        // `count.ohm`'s `u2 n` is not, so the model was asked for n=0 while the
        // circuit was driven with n=2. It reported `lamps -> 2  want: c=0` and
        // the circuit was the one telling the truth.
        //
        // This project has lost whole debugging cycles to harness faults dressed
        // up as logic faults, so: split the value across ports by their declared
        // widths, in the same order the levers are flattened.
        let refs: Vec<(&str, u64)> = {
            let mut shift = 0u32;
            design
                .inputs
                .iter()
                .map(|p| {
                    let mask = if p.width >= 64 { u64::MAX } else { (1u64 << p.width) - 1 };
                    let val = ((v as u64) >> shift) & mask;
                    shift += p.width;
                    (p.name.as_str(), val)
                })
                .collect()
        };
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

        // `OHMC_DUMP_NET=n`: where net n's signal stands once the run ends, cell
        // by cell along the wire. A lamp that is dark with its register set
        // can then be traced to the cell where the signal stops.
        if let Some(n) = std::env::var("OHMC_DUMP_NET").ok().and_then(|v| v.parse::<u32>().ok()) {
            let f = sim.field();
            let mut cells: Vec<_> =
                lay.wire_owner.iter().filter(|&(_, &o)| o == n).map(|(&p, _)| p).collect();
            cells.sort_by_key(|p| (p.2, p.0, p.1));
            println!("net {n} after cycle {}:", cycles - 1);
            for c in cells {
                println!("  {c:?} {:?} power={}", lay.grid.get(c), f.dust_at(c));
            }
        }

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
