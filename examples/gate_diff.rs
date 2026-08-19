//! Which gate does a placed circuit first get wrong?
//!
//! `add.ohm` places cleanly and answers 44 of 64 sampled inputs incorrectly,
//! with whole high-order output bits missing. Comparing outputs only says the
//! circuit is wrong; comparing every *gate* against the netlist says where it
//! stops being right, which is the thing worth knowing.
//!
//! The netlist is the reference: `GateSim` evaluates each signal exactly, and
//! the placed gate's torch is lit precisely when its NOR output is high. Walking
//! in topological order, the first disagreement is the fault - everything after
//! it is downstream noise.
use ohmc::netlist::{GateSim, Src};
use ohmc::redstone::Sim;
use ohmc::world::Pos;
use ohmc::{bitblast, layout, lower, parser};

fn main() {
    let path = std::env::args().nth(1).unwrap_or_else(|| "examples/add.ohm".into());
    let case: usize = std::env::args().nth(2).and_then(|s| s.parse().ok()).unwrap_or(7175);

    let src = std::fs::read_to_string(&path).unwrap();
    let prog = parser::parse(&src).unwrap();
    let design = lower::lower_program(&prog).unwrap();
    let net = bitblast::blast_combinational(&design).unwrap();
    let lay = layout::build(&net).unwrap();

    let levers: Vec<Pos> = lay.input_levers.iter().flat_map(|(_, v)| v.clone()).collect();
    println!("{path} case {case}: {} levers, {} gates", levers.len(), lay.gates);

    // Exact per-signal values from the netlist.
    let (mut vals, mut rest) = (Vec::new(), case as u64);
    for p in &design.inputs {
        vals.push(rest & ((1u64 << p.width) - 1));
        rest >>= p.width;
    }
    let gs = GateSim::new(&net);
    let truth = gs.eval(&vals, false);

    // The same inputs in the placed circuit.
    let mut sim = Sim::new(&lay.grid);
    for (b, &l) in levers.iter().enumerate() {
        sim.set_lever(l, (case >> b) & 1 == 1);
    }
    let (_, stable) = sim.run_until_stable(20000);
    println!("settled: {stable}");
    let f = sim.field();

    // Topological order, so the first mismatch is the cause and not a symptom.
    let roots = net.roots();
    let mut wrong = 0;
    for sig in net.topo_order(&roots) {
        if !matches!(net.src(sig), Src::Nor(_)) {
            continue;
        }
        let Some((out, feeds)) = lay.gate_cells.get(&sig) else { continue };
        let want = truth[sig as usize];
        let got = f.dust_at(*out) > 0;
        if want == got {
            continue;
        }
        wrong += 1;
        if wrong > 6 {
            continue;
        }
        println!("\ngate sig {sig} at {out:?}: netlist says {want}, placed says {got}");
        // A NOR is high exactly when every input is low, so the inputs explain
        // the output. Show what each pad is receiving and what it should be.
        for (i, &fd) in feeds.iter().enumerate() {
            let operand = net.operands(sig).get(i).copied();
            let want_in = operand.map(|o| truth[o as usize]);
            // Also report the driver's own output. If the driver is correct but
            // the pad disagrees, the wire between them is at fault; if the
            // driver is already wrong, the problem is further upstream.
            let driver = operand
                .and_then(|o| lay.gate_cells.get(&o).map(|(out, _)| *out))
                .map(|out| f.dust_at(out));
            println!(
                "    input {i} pad {fd:?} level {:>2}  netlist says {:?}  src {:?}  gate-driver level {:?}",
                f.dust_at(fd),
                want_in,
                operand.map(|o| net.src(o)),
                driver
            );
        }
    }
    // Backtrace any pad that is powered while its own driver is not: dust drops
    // exactly one level per step, so following rising levels leads to whatever
    // is actually energising the wire.
    for sig in net.topo_order(&roots) {
        if !matches!(net.src(sig), Src::Nor(_)) {
            continue;
        }
        let Some((_, feeds)) = lay.gate_cells.get(&sig) else { continue };
        for (i, &fd) in feeds.iter().enumerate() {
            let Some(op) = net.operands(sig).get(i).copied() else { continue };
            let Some(drv) = lay.gate_cells.get(&op).map(|(o, _)| f.dust_at(*o)) else {
                // Driven by a lever, not a gate: there is no gate output to
                // compare against, and defaulting one to zero invented a
                // "driver outputs 0" report for every correctly-driven input.
                continue;
            };
            if f.dust_at(fd) == 0 || drv > 0 || truth[op as usize] {
                continue;
            }
            // Which signal is this pad *supposed* to carry? If the operand is a
            // NOR gate then a wire arriving from a lever net is a short; if it
            // is an input, the lever is the legitimate driver and the fault is
            // elsewhere.
            println!(
                "\npad {fd:?} of gate {sig} reads {} but its driver outputs {drv}\n  operand {op} is {:?}, netlist value {}",
                f.dust_at(fd),
                net.src(op),
                truth[op as usize]
            );
            let mut cur = fd;
            for _ in 0..200 {
                let lv = f.dust_at(cur);
                println!("  {cur:?} level {lv} {:?}", lay.grid.get(cur));
                let mut nxt = None;
                for d in ohmc::world::Dir::ALL {
                    let n = ohmc::world::offset(cur, d);
                    for c in [n, ohmc::world::up(n), ohmc::world::down(n)] {
                        if f.dust_at(c) > lv {
                            nxt = Some(c);
                        }
                    }
                    match lay.grid.get(n) {
                        ohmc::world::Block::WallTorch { .. } | ohmc::world::Block::Torch { .. } => {
                            println!("    <- TORCH {n:?} lit={:?}", sim.state.torch_lit.get(&n));
                        }
                        ohmc::world::Block::Repeater { facing, .. }
                            if ohmc::world::offset(n, facing.opposite()) == cur =>
                        {
                            let src = ohmc::world::offset(n, facing);
                            println!("    <- REPEATER {n:?} powered={:?}, reads {src:?}", sim.state.repeater_powered.get(&n));
                            if nxt.is_none() {
                                nxt = Some(src);
                            }
                        }
                        _ => {}
                    }
                }
                match nxt {
                    Some(x) => cur = x,
                    None => break,
                }
            }
            break;
        }
    }

    println!("\n{wrong} gate(s) disagree with the netlist");
}
