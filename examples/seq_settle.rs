//! Does a placed sequential circuit settle at all, and at what size does it stop?
//!
//! `tick` rings with every lever held still. Debugging that directly means a
//! two-minute placement and a 60,000-block simulation per experiment, so the
//! first job is to find the smallest design that shows the same symptom.
use ohmc::netlist::Netlist;
use ohmc::redstone::Sim;
use ohmc::{layout, world::Pos};

fn report(name: &str, net: &Netlist) {
    let lay = match layout::build(net) {
        Ok(l) => l,
        Err(e) => {
            println!("{name}: DID NOT PLACE: {e}");
            return;
        }
    };
    let levers: Vec<Pos> = lay.input_levers.iter().flat_map(|(_, v)| v.clone()).collect();
    let mut sim = Sim::new(&lay.grid);
    for &l in &levers {
        sim.set_lever(l, false);
    }
    sim.set_lever(lay.clk_lever.unwrap(), false);
    sim.set_lever(lay.clk_n_lever.unwrap(), true);
    sim.set_lever(lay.rst_lever.unwrap(), false);
    let (t, ok) = sim.run_until_stable(3000);
    println!(
        "{name}: {} flops, {} gates -> settled={ok} after {t} ticks ({} pending, {} torches burnt)",
        lay.flops,
        lay.gates,
        sim.state.pending_len(),
        sim.burned_out().len()
    );
    if !ok {
        for (p, target) in sim.state.pending_cells().into_iter().take(12) {
            println!("    pending {p:?} -> {target}  {:?}", lay.grid.get(p));
        }
    }
}

fn main() {
    // One flop fed its own inverted output: the smallest real sequential circuit.
    let mut net = Netlist::new();
    let (idx, q) = net.add_dff("r");
    let nq = net.nor(&[q]);
    net.set_dff_d(idx, nq);
    net.outputs.push(("q".into(), vec![q]));
    report("1 flop, self-inverting", &net);

    // Two flops, each fed the other's inverted output.
    let mut net = Netlist::new();
    let (a, qa) = net.add_dff("a");
    let (b, qb) = net.add_dff("b");
    let na = net.nor(&[qa]);
    let nb = net.nor(&[qb]);
    net.set_dff_d(a, nb);
    net.set_dff_d(b, na);
    net.outputs.push(("q".into(), vec![qa, qb]));
    report("2 flops, cross-coupled", &net);

    // A flop with no feedback at all: D tied to a constant-ish gate of an input.
    let mut net = Netlist::new();
    let inp = net.input(0, 0);
    let (c, qc) = net.add_dff("c");
    let d = net.nor(&[inp]);
    net.set_dff_d(c, d);
    net.outputs.push(("q".into(), vec![qc]));
    report("1 flop, no feedback", &net);

    // Deeper logic in the loop, closer to what an FSM does.
    let mut net = Netlist::new();
    let (e, qe) = net.add_dff("e");
    let mut s = qe;
    for _ in 0..4 {
        s = net.nor(&[s]);
    }
    net.set_dff_d(e, s);
    net.outputs.push(("q".into(), vec![qe]));
    report("1 flop, 4 gates deep", &net);

    // Scale up: N flops in a ring, each fed through a chain of gates from the
    // previous one, so the design has both a wide bank and deep logic.
    for (n, depth) in [(3usize, 3usize), (6, 4), (9, 5), (11, 6), (11, 11)] {
        let mut net = Netlist::new();
        let mut idx = Vec::new();
        let mut qs = Vec::new();
        for i in 0..n {
            let (a, q) = net.add_dff(&format!("r{i}"));
            idx.push(a);
            qs.push(q);
        }
        for i in 0..n {
            let mut s = qs[(i + n - 1) % n];
            for _ in 0..depth {
                s = net.nor(&[s]);
            }
            net.set_dff_d(idx[i], s);
        }
        net.outputs.push(("q".into(), qs));
        report(&format!("{n} flops, {depth} deep (inverter chains)"), &net);
    }

    // The same sizes, but with *multi-input* gates and reconvergent fan-out -
    // which is what a real FSM looks like and what the inverter chains above
    // deliberately are not. Every gate here reads two different flip-flops, so
    // Q values travel much further and cross each other constantly.
    for (n, depth) in [(6usize, 3usize), (11, 4), (11, 8)] {
        let mut net = Netlist::new();
        let mut idx = Vec::new();
        let mut qs = Vec::new();
        for i in 0..n {
            let (a, q) = net.add_dff(&format!("r{i}"));
            idx.push(a);
            qs.push(q);
        }
        let mut layer: Vec<u32> = qs.clone();
        for _ in 0..depth {
            let mut next = Vec::new();
            for i in 0..n {
                next.push(net.nor(&[layer[i], layer[(i + 3) % n]]));
            }
            layer = next;
        }
        for i in 0..n {
            net.set_dff_d(idx[i], layer[i]);
        }
        net.outputs.push(("q".into(), qs));
        report(&format!("{n} flops, {depth} deep (2-input, reconvergent)"), &net);
    }
}
