//! Does a placed combinational circuit answer the same twice?
//!
//! `tools/mc-validate.sh` now sweeps every input combination in one world, then
//! sweeps them again in reverse. `add2` fails that: cases 0-7 are correct
//! ascending and wrong descending, every one with `sum[1]` inverted. A
//! combinational adder cannot legitimately depend on what it computed before.
//!
//! This is the same experiment against the block simulator, which unlike the
//! game runs in a second and can be stepped. If it reproduces, the fault is in
//! the placed circuit and findable here. If it does not, the model is missing
//! whatever the game is doing.
use ohmc::redstone::{Sim, BURNOUT_WINDOW};
use ohmc::world::Pos;
use ohmc::{bitblast, layout, lower, parser};

fn main() {
    let path = std::env::args().nth(1).unwrap_or_else(|| "examples/add2.ohm".into());
    let src = std::fs::read_to_string(&path).unwrap();
    let prog = parser::parse(&src).unwrap();
    let design = lower::lower_program(&prog).unwrap();
    let net = bitblast::blast_combinational(&design).unwrap();
    let lay = layout::build(&net).unwrap();

    // Flatten the per-port lever and lamp lists into one LSB-first vector, the
    // same order the in-game harness drives and reads them in.
    let levers: Vec<Pos> = lay.input_levers.iter().flat_map(|(_, v)| v.clone()).collect();
    let lamps: Vec<Pos> = lay.output_lamps.iter().flat_map(|(_, v)| v.clone()).collect();
    println!("{path}: {} levers, {} lamps", levers.len(), lamps.len());

    // Truth from the gate netlist, which is the reference the game is checked
    // against too.
    let gs = ohmc::netlist::GateSim::new(&net);
    let mut expected = Vec::new();
    for v in 0..(1usize << levers.len()) {
        // Split the flat counter across the ports exactly as `--truth` does.
        let (mut vals, mut rest) = (Vec::new(), v as u64);
        for p in &design.inputs {
            vals.push(rest & ((1u64 << p.width) - 1));
            rest >>= p.width;
        }
        let mut got = 0usize;
        let mut bit = 0;
        for (n, sigs) in &net.outputs {
            let val = gs.read_output(n, &vals, false);
            for k in 0..sigs.len() {
                got |= (((val >> k) & 1) as usize) << bit;
                bit += 1;
            }
        }
        expected.push(got);
    }

    // One persistent simulator for the whole sweep, exactly as one world is one
    // circuit in game.
    let mut sim = Sim::new(&lay.grid);
    let read = |sim: &mut Sim, v: usize| -> (usize, bool) {
        for (b, &l) in levers.iter().enumerate() {
            sim.set_lever(l, (v >> b) & 1 == 1);
        }
        let (_, stable) = sim.run_until_stable(20000);
        // Idle, so the burnout window empties between cases. In game each case
        // is ten seconds apart - two hundred redstone ticks - but
        // `run_until_stable` only advances while something is pending, so
        // without this the toggles from several cases fall in one window and
        // the simulator burns torches the game never would.
        for _ in 0..BURNOUT_WINDOW * 2 {
            sim.step();
        }
        let f = sim.field();
        let got = lamps
            .iter()
            .enumerate()
            .fold(0usize, |a, (i, &p)| a | ((f.block_powered(p) as usize) << i));
        (got, stable)
    };

    let n = 1usize << levers.len();
    let up: Vec<(usize, bool)> = (0..n).map(|v| read(&mut sim, v)).collect();
    let mut down = vec![(0usize, true); n];
    for v in (0..n).rev() {
        down[v] = read(&mut sim, v);
    }

    // Same inputs, two histories: diff the whole machine. Whatever differs is
    // the state the circuit is holding that it should not.
    {
        let mut a = Sim::new(&lay.grid);
        for v in 0..n {
            for (b, &l) in levers.iter().enumerate() {
                a.set_lever(l, (v >> b) & 1 == 1);
            }
            a.run_until_stable(20000);
        }
        for v in (0..n).rev() {
            for (b, &l) in levers.iter().enumerate() {
                a.set_lever(l, (v >> b) & 1 == 1);
            }
            a.run_until_stable(20000);
        }
        let mut b = Sim::new(&lay.grid);
        for (bit, &l) in levers.iter().enumerate() {
            b.set_lever(l, (0 >> bit) & 1 == 1);
        }
        b.run_until_stable(20000);

        let mut diffs: Vec<(Pos, bool, bool)> = a
            .state
            .torch_lit
            .iter()
            .filter_map(|(&p, &va)| {
                let vb = b.state.torch_lit.get(&p).copied().unwrap_or(va);
                (va != vb).then_some((p, vb, va))
            })
            .collect();
        diffs.sort();
        println!("\ntorches differing at case 0, fresh vs after a full sweep: {}", diffs.len());
        for (p, fresh, swept) in diffs.iter().take(12) {
            println!("  {p:?} fresh={fresh} swept={swept}");
        }
        // Is the swept state actually a fixed point? A DAG of gates cannot hold
        // state, and the audit finds no loop - so if simply running longer
        // converges to the fresh answer, `run_until_stable` is stopping early
        // and the fault is in the simulator, not the circuit.
        let before: Vec<bool> = lamps.iter().map(|&p| a.field().block_powered(p)).collect();
        for _ in 0..20000 {
            a.step();
        }
        let after: Vec<bool> = lamps.iter().map(|&p| a.field().block_powered(p)).collect();
        let fresh: Vec<bool> = lamps.iter().map(|&p| b.field().block_powered(p)).collect();
        println!("lamps swept={before:?}\n      +20k={after:?}\n     fresh={fresh:?}");
        println!("pending after settle: {}", a.state.pending_len());

        // Repeaters hold state too. If they differ with nothing pending, the
        // memory is in the delay elements rather than the gates.
        let mut rdiff: Vec<(Pos, bool, bool)> = a
            .state
            .repeater_powered
            .iter()
            .filter_map(|(&p, &va)| {
                let vb = b.state.repeater_powered.get(&p).copied().unwrap_or(va);
                (va != vb).then_some((p, vb, va))
            })
            .collect();
        rdiff.sort();
        println!("repeaters differing: {}", rdiff.len());
        for (p, fresh, swept) in rdiff.iter().take(10) {
            println!("  {p:?} fresh={fresh} swept={swept} block={:?}", lay.grid.get(*p));
        }

        let burnt = a.burned_out();
        println!("burned-out torches after the sweep: {}", burnt.len());
        for t in burnt.iter().take(12) {
            println!("  BURNT {t:?}");
        }
    }

    let mut bad = 0;
    println!("{:<5} {:>8} {:>7} {:>7}", "in", "expected", "pass1", "pass2");
    for v in 0..n {
        let (g1, s1) = up[v];
        let (g2, s2) = down[v];
        let ok = g1 == expected[v] && g2 == expected[v];
        bad += !ok as usize;
        let mut note = if ok { "ok".to_string() } else { "MISMATCH".to_string() };
        if g1 != g2 {
            note += "  HISTORY-DEPENDENT";
        }
        if !s1 || !s2 {
            note += "  UNSTABLE";
        }
        println!("{v:<5} {:>8} {g1:>7} {g2:>7}  {note}", expected[v]);
    }
    println!("\n{bad} of {n} case(s) wrong");
}
