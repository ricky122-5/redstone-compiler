//! Drive the flip-flop through a capture sequence in the simulator.
//!
//! This is the local twin of `tools/dff-validate.sh`: same stages, same
//! readout. The server run takes ten minutes and has repeatedly produced
//! readings that turned out to be the harness rather than the circuit; this
//! runs in under a second and can see inside the latches, so it is the right
//! instrument for finding the fault. The server's job is to confirm the fix,
//! not to locate it.
use ohmc::redstone::Sim;
use ohmc::tech::stamp_dff;
use ohmc::world::{Block, Dir, Face, Grid, Material, Pos};

fn drive(g: &mut Grid, feed: Pos) -> Pos {
    let (x, y, z) = feed;
    g.force((x, y - 1, z), Block::Solid(Material::Wire));
    g.force((x, y, z), Block::Dust { power: 0 });
    g.force((x, y - 1, z - 1), Block::Solid(Material::Wire));
    g.force((x, y, z - 1), Block::Dust { power: 0 });
    let lever = (x, y, z - 2);
    g.force((x, y - 1, z - 2), Block::Solid(Material::PortIn));
    g.force(lever, Block::Lever { face: Face::Floor, facing: Dir::North, powered: false });
    lever
}

fn main() {
    let mut g = Grid::new();
    let p = stamp_dff(&mut g, (0, 0, 0)).unwrap();
    let dl: Vec<Pos> = p.d_feeds.iter().map(|&f| drive(&mut g, f)).collect();
    let cl: Vec<Pos> = p.clk_feeds.iter().map(|&f| drive(&mut g, f)).collect();
    let nl: Vec<Pos> = p.clk_n_feeds.iter().map(|&f| drive(&mut g, f)).collect();
    let rl: Vec<Pos> = p.clr_feeds.iter().map(|&f| drive(&mut g, f)).collect();

    // Insert the *same* probe lamps the exporter does, replacing each node's
    // support block. A lamp is not an ordinary conductor, so this is a circuit
    // modification and not a passive measurement - and the simulator has never
    // been run with them present. If the freeze seen in game reproduces here,
    // the probes are the fault rather than the flip-flop.
    if std::env::args().any(|a| a == "--probes") {
        for probe in [p.q, p.master_q, p.slave_not_e_r, p.slave_r_out, p.master_not_e_r, p.master_r_out] {
            g.force((probe.0, probe.1 - 1, probe.2), Block::Lamp { lit: false });
        }
        println!("[probe lamps inserted, as dff_export does]");
    }

    let mut sim = Sim::new(&g);
    // The same stages the in-game script drives, in the same order.
    let stages: [(&str, bool, bool); 6] = [
        ("d1_clk_low", true, false),
        ("d1_clk_high", true, true),
        ("d1_after_edge", true, false),
        ("d0_clk_idle", false, false),
        ("d0_clk_high", false, true),
        ("d0_after_edge", false, false),
    ];

    // Pulse reset first: the state chosen at stamp time does not survive
    // placement, so the circuit is driven into a known one instead.
    for &l in &rl {
        sim.set_lever(l, true);
    }
    sim.run_until_stable(20000);
    for &l in &rl {
        sim.set_lever(l, false);
    }
    sim.run_until_stable(20000);

    println!(
        "{:<16} {:>4} {:>4} {:>4} {:>8} {:>6} {:>6} {:>6}",
        "stage", "D", "CLK", "Q", "master_q", "sD_a", "sD_b", "burnt"
    );
    for (name, d, clk) in stages {
        for &l in &dl {
            sim.set_lever(l, d);
        }
        for &l in &cl {
            sim.set_lever(l, clk);
        }
        for &l in &nl {
            sim.set_lever(l, !clk);
        }
        let (_, stable) = sim.run_until_stable(5000);
        let f = sim.field();
        // A torch that burned out is stuck off and will not recover. If one
        // shows up here it is the fault, not a symptom.
        let burnt = sim.burned_out().len();
        println!(
            "{:<16} {:>4} {:>4} {:>4} {:>8} {:>6} {:>6} {:>6} {}",
            name,
            d as u8,
            clk as u8,
            f.dust_at(p.q),
            f.dust_at(p.master_q),
            f.dust_at(p.slave_d_feeds[0]),
            f.dust_at(p.slave_d_feeds[1]),
            burnt,
            if stable { "" } else { "UNSTABLE" }
        );
    }

    let b = sim.burned_out();
    if !b.is_empty() {
        println!("\nburned-out torches ({}):", b.len());
        for t in b.iter().take(12) {
            println!("  {t:?}");
        }
    }
}
