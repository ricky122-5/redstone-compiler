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
use std::collections::HashMap;
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

        // For each torch that settles two ways, show what powers its support in
        // each state. A torch is lit exactly when its support is unpowered, so
        // whatever differs here is the immediate cause - and following it back
        // has to close a loop somewhere.
        let fa = a.field();
        let fb = b.field();
        println!("\nwhy each differing torch differs:");
        for (t, fresh, swept) in diffs.iter().take(6) {
            let sup = match lay.grid.get(*t) {
                ohmc::world::Block::WallTorch { facing, .. } => {
                    ohmc::world::offset(*t, facing.opposite())
                }
                _ => ohmc::world::down(*t),
            };
            println!(
                "  torch {t:?} fresh={fresh} swept={swept}  support {sup:?} powered fresh={} swept={}",
                fb.block_powered(sup),
                fa.block_powered(sup)
            );
            for d in ohmc::world::Dir::ALL {
                let n = ohmc::world::offset(sup, d);
                for c in [n, ohmc::world::up(n), ohmc::world::up(sup)] {
                    let blk = lay.grid.get(c);
                    if matches!(blk, ohmc::world::Block::Air) {
                        continue;
                    }
                    let (df, ds) = (fb.dust_at(c), fa.dust_at(c));
                    if df != ds {
                        println!("      {c:?} {blk:?} dust fresh={df} swept={ds}");
                    }
                }
            }
        }

        let burnt = a.burned_out();
        println!("burned-out torches after the sweep: {}", burnt.len());
        for t in burnt.iter().take(12) {
            println!("  BURNT {t:?}");
        }
    }

    // Minimal reproduction. The descending pass is clean from 15 down to 8 and
    // wrong from 7 down, so the single transition 8 -> 7 is the trigger. That
    // step flips all four levers at once; doing the same change one lever at a
    // time, settling in between, says whether this is a simultaneous-change
    // hazard or a property of the destination state itself.
    if n > 8 {
        let step = |sim: &mut Sim, v: usize| {
            for (b, &l) in levers.iter().enumerate() {
                sim.set_lever(l, (v >> b) & 1 == 1);
            }
            sim.run_until_stable(20000);
        };
        let peek = |sim: &Sim| {
            let f = sim.field();
            lamps.iter().enumerate().fold(0usize, |a, (i, &p)| a | ((f.block_powered(p) as usize) << i))
        };

        let mut s1 = Sim::new(&lay.grid);
        step(&mut s1, 8);
        let at8 = peek(&s1);
        step(&mut s1, 7);
        println!("\nall four levers at once: 8 -> 7 gives {} (expected {})", peek(&s1), expected[7]);
        println!("  (case 8 read {} , expected {})", at8, expected[8]);

        let mut s2 = Sim::new(&lay.grid);
        step(&mut s2, 8);
        // 8 = 1000, 7 = 0111: walk one lever at a time, settling after each.
        let mut cur = 8usize;
        for b in 0..levers.len() {
            let want = (7 >> b) & 1;
            if (cur >> b) & 1 != want {
                cur = (cur & !(1 << b)) | (want << b);
                step(&mut s2, cur);
            }
        }
        println!("one lever at a time: 8 -> 7 gives {} (expected {})", peek(&s2), expected[7]);

        // And straight to 7 from power-up, for reference.
        let mut s3 = Sim::new(&lay.grid);
        step(&mut s3, 7);
        println!("from power-up:      7 gives {} (expected {})", peek(&s3), expected[7]);

        // Which torches hold the wrong state, and is the wrongness
        // self-sustaining? Flip one torch back to its correct value and settle.
        // If the whole circuit falls into the right answer, that torch is
        // inside the loop; if it snaps back, something else is holding it.
        let bad = s1.state.clone();
        let good = s3.state.clone();
        let mut wrong: Vec<Pos> = good
            .torch_lit
            .iter()
            .filter(|(p, v)| bad.torch_lit.get(p) != Some(*v))
            .map(|(&p, _)| p)
            .collect();
        wrong.sort();
        // All at once: if the six together are a self-sustaining loop, fixing
        // the whole set holds; if something outside drives them, it snaps back.
        {
            let mut probe = Sim::new(&lay.grid);
            probe.state = bad.clone();
            for t in good.torch_lit.keys() {
                if bad.torch_lit.get(t) != good.torch_lit.get(t) {
                    probe.state.torch_lit.insert(*t, good.torch_lit[t]);
                }
            }
            probe.run_until_stable(20000);
            println!(
                "\nall wrong torches forced good -> {} (expected {})",
                peek(&probe),
                expected[7]
            );
            let mut probe2 = Sim::new(&lay.grid);
            probe2.state = bad.clone();
            for r in good.repeater_powered.keys() {
                if bad.repeater_powered.get(r) != good.repeater_powered.get(r) {
                    probe2.state.repeater_powered.insert(*r, good.repeater_powered[r]);
                }
            }
            probe2.run_until_stable(20000);
            println!("all wrong repeaters forced good -> {}", peek(&probe2));
        }

        // Who actually powers the level-1 gate's input pad in the wrong state?
        //
        // Dust loses exactly one level per step, so the chain back to a source
        // is unambiguous: from a cell at level L, the cell that fed it is a
        // neighbour at L+1. Walk that until it runs out, then name whatever
        // component sits next to the end. A level-1 gate's input must come from
        // a lever; if this trace ends at a torch, that torch is feeding
        // backwards and is the loop.
        {
            let fbad = s1.field();
            // The shallowest wrong torch is the one worth tracing.
            let backtrace_from = wrong.iter().min_by_key(|p| -p.1).copied();
            if backtrace_from.is_none() {
                println!("\nno torch holds the wrong state - nothing to backtrace");
            }
            let t = backtrace_from.unwrap_or((0, 0, 0));
            if backtrace_from.is_some() {
            let sup = match lay.grid.get(t) {
                ohmc::world::Block::WallTorch { facing, .. } => {
                    ohmc::world::offset(t, facing.opposite())
                }
                _ => ohmc::world::down(t),
            };
            let mut cur = ohmc::world::up(sup);
            println!("\nbacktrace from {t:?} (support {sup:?}, pad {cur:?}):");
            for _ in 0..40 {
                let lv = fbad.dust_at(cur);
                println!("  {cur:?} level {lv} {:?}", lay.grid.get(cur));
                if lv == 0 {
                    break;
                }
                let mut nxt = None;
                for d in ohmc::world::Dir::ALL {
                    let n = ohmc::world::offset(cur, d);
                    for c in [n, ohmc::world::up(n), ohmc::world::down(n)] {
                        if fbad.dust_at(c) > lv {
                            nxt = Some(c);
                        }
                    }
                    // A source sitting right next to this cell ends the walk.
                    match lay.grid.get(n) {
                        ohmc::world::Block::WallTorch { .. } | ohmc::world::Block::Torch { .. } => {
                            println!("    <- driven by TORCH {n:?} lit={:?}", s1.state.torch_lit.get(&n));
                        }
                        ohmc::world::Block::Repeater { facing, .. }
                            if ohmc::world::offset(n, facing.opposite()) == cur =>
                        {
                            println!("    <- driven by REPEATER {n:?} powered={:?}", s1.state.repeater_powered.get(&n));
                        }
                        ohmc::world::Block::Lever { .. } => {
                            println!("    <- driven by LEVER {n:?} on={:?}", s1.state.lever_on.get(&n));
                        }
                        _ => {}
                    }
                }
                // Ending at a repeater is not the end of the chain: hop to
                // whatever it reads and keep going, or the trace stops one step
                // short of the answer every time.
                if nxt.is_none() {
                    for d in ohmc::world::Dir::ALL {
                        let n = ohmc::world::offset(cur, d);
                        if let ohmc::world::Block::Repeater { facing, .. } = lay.grid.get(n) {
                            if ohmc::world::offset(n, facing.opposite()) == cur {
                                let src = ohmc::world::offset(n, facing);
                                println!("    hop through repeater {n:?} to its input {src:?} {:?}", lay.grid.get(src));
                                nxt = Some(src);
                            }
                        }
                    }
                }
                match nxt {
                    Some(n) => cur = n,
                    None => break,
                }
            }
            }
        }

        // Discover the real driver graph by perturbation rather than geometry.
        // From the correct state, flip one torch and run a couple of ticks:
        // whatever else moves is genuinely downstream of it, by the simulator's
        // own rules. Hand-rolled geometry has missed this loop three times.
        {
            let torches: Vec<Pos> = good.torch_lit.keys().copied().collect();
            let mut edges: HashMap<Pos, Vec<Pos>> = HashMap::new();
            for &t in &torches {
                let mut probe = Sim::new(&lay.grid);
                probe.state = good.clone();
                let flipped = !good.torch_lit[&t];
                probe.state.torch_lit.insert(t, flipped);
                probe.state.clear_for_probe();
                // Hold it forced. `step` recomputes every torch from its
                // support, so a flip that is not re-applied is undone on the
                // next tick and downstream sees only a one-tick pulse - which
                // is why the first version of this probe found nothing.
                for _ in 0..6 {
                    probe.state.torch_lit.insert(t, flipped);
                    probe.step();
                }
                probe.state.torch_lit.insert(t, flipped);
                let mut moved: Vec<Pos> = probe
                    .state
                    .torch_lit
                    .iter()
                    .filter(|(p, v)| **p != t && good.torch_lit.get(p) != Some(*v))
                    .map(|(&p, _)| p)
                    .collect();
                moved.sort();
                edges.insert(t, moved);
            }
            // Any cycle in that graph is the bug.
            let mut colour: HashMap<Pos, u8> = HashMap::new();
            let mut stk: Vec<Pos> = Vec::new();
            fn dfs(n: Pos, e: &HashMap<Pos, Vec<Pos>>, c: &mut HashMap<Pos, u8>, s: &mut Vec<Pos>) -> Option<Vec<Pos>> {
                c.insert(n, 1);
                s.push(n);
                for &m in e.get(&n).into_iter().flatten() {
                    match c.get(&m).copied().unwrap_or(0) {
                        1 => { let at = s.iter().position(|&x| x == m).unwrap(); return Some(s[at..].to_vec()); }
                        0 => { if let Some(r) = dfs(m, e, c, s) { return Some(r); } }
                        _ => {}
                    }
                }
                s.pop();
                c.insert(n, 2);
                None
            }
            let mut ks: Vec<Pos> = edges.keys().copied().collect();
            ks.sort();
            let mut found = None;
            for k in ks {
                if colour.get(&k).copied().unwrap_or(0) == 0 {
                    if let Some(cyc) = dfs(k, &edges, &mut colour, &mut stk) { found = Some(cyc); break; }
                }
            }
            match found {
                Some(c) => {
                    println!("\nFEEDBACK LOOP, {} gate(s):", c.len());
                    for t in &c { println!("  torch {t:?}"); }
                }
                None => println!("\nperturbation finds no loop either"),
            }
        }

        println!("\n{} torch(es) hold the wrong state at input 7:", wrong.len());
        for t in &wrong {
            let mut probe = Sim::new(&lay.grid);
            probe.state = bad.clone();
            let fixed = good.torch_lit[t];
            probe.state.torch_lit.insert(*t, fixed);
            probe.run_until_stable(20000);
            let got = peek(&probe);
            println!(
                "  {t:?} forced to {fixed} -> circuit reads {got} ({})",
                if got == expected[7] { "RECOVERS - inside the loop" } else { "still wrong" }
            );
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
