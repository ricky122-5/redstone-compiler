//! `ohmc` - command line driver. All the compiler logic lives in the library.


use ohmc::{bitblast, layout, lower, machine, parser, schem, structure};
use std::process::ExitCode;

const USAGE: &str = "\
ohmc - compile Ohm to redstone

USAGE:
    ohmc <file.ohm> [OPTIONS]

OPTIONS:
    --run a=1,b=2      execute the program on the golden model and print outputs
    -o <file.schem>    write a Sponge v3 schematic (needs WorldEdit)
    --nbt <file.nbt>   write a vanilla structure-block file (no mods needed)
    --mcfn <file>      write a .mcfunction of setblock commands (no mods needed)
    --truth            print the expected truth table (for in-game validation)
    --stats            print compilation statistics
    -h, --help         show this message
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() || args.iter().any(|a| a == "-h" || a == "--help") {
        print!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: &[String]) -> Result<(), String> {
    let mut path = None;
    let mut run_inputs = None;
    let mut out = None;
    let mut nbt_out = None;
    let mut mcfn_out = None;
    let mut stats = false;
    let mut truth = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--run" => {
                i += 1;
                run_inputs = Some(args.get(i).ok_or("--run needs a value")?.clone());
            }
            "-o" => {
                i += 1;
                out = Some(args.get(i).ok_or("-o needs a path")?.clone());
            }
            "--nbt" => {
                i += 1;
                nbt_out = Some(args.get(i).ok_or("--nbt needs a path")?.clone());
            }
            "--mcfn" => {
                i += 1;
                mcfn_out = Some(args.get(i).ok_or("--mcfn needs a path")?.clone());
            }
            "--truth" => truth = true,
            "--stats" => stats = true,
            a if a.starts_with('-') => return Err(format!("unknown flag `{a}`")),
            a => path = Some(a.to_string()),
        }
        i += 1;
    }
    let path = path.ok_or("no input file given")?;
    let src = std::fs::read_to_string(&path).map_err(|e| format!("{path}: {e}"))?;

    let program = parser::parse(&src)?;
    let design = lower::lower_program(&program)?;
    let net = bitblast::blast(&design);

    if stats {
        println!("{path}:");
        println!("  ports        {} in, {} out", design.inputs.len(), design.outputs.len());
        println!("  basic blocks {} (each is one clock cycle)", design.blocks.len());
        println!("  registers    {}", design.regs.len());
        println!("  IR nodes     {}", design.node_count());
        println!("  NOR gates    {}", net.gate_count());
        println!("  flip-flops   {}", net.dffs.len());
        println!("  logic depth  {} gate levels", net.logic_depth());
    }

    if let Some(spec) = run_inputs {
        let mut inputs: Vec<(String, u64)> = Vec::new();
        for part in spec.split(',').filter(|s| !s.is_empty()) {
            let (k, v) = part
                .split_once('=')
                .ok_or_else(|| format!("bad --run entry `{part}`, expected name=value"))?;
            let v: u64 = v.trim().parse().map_err(|_| format!("bad number in `{part}`"))?;
            inputs.push((k.trim().to_string(), v));
        }
        let refs: Vec<(&str, u64)> = inputs.iter().map(|(k, v)| (k.as_str(), *v)).collect();
        let (m, cycles) = machine::run_design(&design, &refs, 1_000_000)?;
        println!("ran {cycles} cycle(s):");
        for (name, _) in &design.outputs {
            println!("  {name} = {}", m.output(&design, name).unwrap());
        }
    }

    if truth {
        // Every input combination and the output it should produce, so an
        // external harness can drive the real game and diff against us.
        let comb = bitblast::blast_combinational(&design)?;
        let bits: u32 = design.inputs.iter().map(|p| p.width).sum();
        if bits > 16 {
            return Err(format!("{bits} input bits is too many to enumerate"));
        }
        let sim = ohmc::netlist::GateSim::new(&comb);
        for v in 0..(1u64 << bits) {
            // Split the flat counter across the ports, LSB-first.
            let mut vals = Vec::new();
            let mut rest = v;
            for p in &design.inputs {
                vals.push(rest & ((1u64 << p.width) - 1));
                rest >>= p.width;
            }
            let outs: Vec<String> = comb
                .outputs
                .iter()
                .map(|(n, _)| format!("{n}={}", sim.read_output(n, &vals, false)))
                .collect();
            println!("TRUTH {v} {}", outs.join(" "));
        }
    }

    if out.is_some() || nbt_out.is_some() || mcfn_out.is_some() {
        // Straight-line programs place from the combinational lowering, which
        // has no FSM and no registers. Anything with a loop or a branch needs
        // the sequential netlist and the register bank that goes with it.
        let mut placed_net = match bitblast::blast_combinational(&design) {
            Ok(comb) => comb,
            Err(_) => bitblast::blast(&design),
        };
        // Split any NOR gate with more readers than this between copies of it.
        if let Some(t) = std::env::var("OHMC_CLONE").ok().and_then(|v| v.parse::<usize>().ok()) {
            let before = placed_net.max_nor_fanout();
            let made = placed_net.clone_high_fanout(t);
            eprintln!(
                "cloned {made} gate copy(s): max NOR fan-out {before} -> {}",
                placed_net.max_nor_fanout()
            );
        }
        // Buffer wide flip-flop outputs, inputs and reset, which cannot be cloned.
        if let Some(t) = std::env::var("OHMC_BUFFER").ok().and_then(|v| v.parse::<usize>().ok()) {
            let made = placed_net.buffer_high_fanout(t);
            eprintln!("buffered {made} reader group(s) of wide non-NOR sources (threshold {t})");
        }
        let layout = layout::build(&placed_net)?;
        // The control levers are reported in the same frame as everything else
        // the caller is given, which means the *.mcfunction* frame when one is
        // being written.
        //
        // They used to print raw layout coordinates while the port levers and
        // lamps were translated, so a sequential design reported its clock at
        // ~-6 ~6 ~-4 and its input lever at ~15 ~74 ~201 - two different
        // origins, in the same block of output, with nothing to say so. Any
        // harness driving the circuit would set blocks in empty air and read a
        // dead machine.
        let lo = layout.grid.bounds().map(|(lo, _)| lo).unwrap_or((0, 0, 0));
        let origin = (0, 1, 0);
        let mcfn_frame = mcfn_out.is_some();
        let rel = |p: (i32, i32, i32)| {
            if mcfn_frame {
                (p.0 - lo.0 + origin.0, p.1 - lo.1 + origin.1, p.2 - lo.2 + origin.2)
            } else {
                p
            }
        };
        if layout.flops > 0 {
            println!("  sequential: {} flip-flop(s)", layout.flops);
            // Two resets, and they are different mechanisms. `reset` is the
            // flip-flops' asynchronous clear; `state reset` drives the netlist's
            // own Src::Reset, which only raises the entry block's D input and so
            // has to be *clocked in*. Assert both with the clock still and the
            // state vector is cleared and never loaded - the machine then sits
            // there doing nothing while looking perfectly healthy.
            if let Some(p) = layout.net_reset_lever.map(rel) {
                println!("  state reset lever at ~{} ~{} ~{}  (hold, then clock once)", p.0, p.1, p.2);
            }
            if let Some(p) = layout.clk_lever.map(rel) {
                println!("  clock   lever at ~{} ~{} ~{}", p.0, p.1, p.2);
            }
            if let Some(p) = layout.clk_n_lever.map(rel) {
                println!("  clock_n lever at ~{} ~{} ~{}", p.0, p.1, p.2);
            }
            if let Some(p) = layout.rst_lever.map(rel) {
                println!("  reset   lever at ~{} ~{} ~{}  (async clear)", p.0, p.1, p.2);
            }
            // Where each register's Q sits, so the state vector can be read in
            // game and not merely inferred from the lamps.
            //
            // The lamps are the last thing in the machine: if they read wrong,
            // the fault could be anywhere from the input levers to the output
            // spine, and there is no way to tell which from outside. The block
            // simulator prints Q every cycle for exactly this reason, and the
            // in-game harness could not - so a disagreement between them had no
            // common ground to be compared on. These make the two traces
            // directly comparable, register by register and cycle by cycle.
            for (i, f) in layout.flop_ports.iter().enumerate() {
                let p = rel(f.q);
                println!("  state  q[{i}] dust  at ~{} ~{} ~{}", p.0, p.1, p.2);
            }
            // Where the clock and the clear actually arrive, per register.
            //
            // The state vector alone says a machine is frozen; it does not say
            // why. `count.ohm` in game sits with every register high and never
            // advances, while the same placed grid clocks correctly in the block
            // simulator - so something on the way from the control levers to the
            // bank is dying, and the only way to find out where is to read the
            // signal at the far end. These are the pads the trunks deliver to.
            for (i, f) in layout.flop_ports.iter().enumerate() {
                if let Some(&c) = f.clk_feeds.first() {
                    let p = rel(c);
                    println!("  feed   clk[{i}] dust  at ~{} ~{} ~{}", p.0, p.1, p.2);
                }
                if let Some(&c) = f.clr_feeds.first() {
                    let p = rel(c);
                    println!("  feed   clr[{i}] dust  at ~{} ~{} ~{}", p.0, p.1, p.2);
                }
            }
        }

        if let Some(out_path) = out {
            let s = schem::Schematic::from_grid(&layout.grid);
            let bytes = s.to_bytes().map_err(|e| e.to_string())?;
            std::fs::write(&out_path, &bytes).map_err(|e| format!("{out_path}: {e}"))?;
            println!(
                "wrote {out_path}: {}x{}x{} ({} blocks, {} gates, {} levels)",
                s.width, s.height, s.length, layout.grid.len(), layout.gates, layout.levels
            );
        }

        if let Some(fn_path) = mcfn_out.clone() {
            // Lift the build clear of the ground so nothing is buried.
            let text = structure::to_mcfunction(&layout.grid, origin);
            let lines = text.lines().filter(|l| l.starts_with("setblock")).count();
            std::fs::write(&fn_path, &text).map_err(|e| format!("{fn_path}: {e}"))?;
            println!("wrote {fn_path}: {lines} setblock commands");
            // Say so when the file is longer than a function is allowed to run.
            //
            // Minecraft stops a function after `maxCommandChainLength` commands
            // and reports nothing at all. At the default of 65536 that silently
            // truncated `count.ohm`, and since this emitter writes dust last,
            // what went missing was the entire wiring layer - the built machine
            // sat frozen and looked like a dead circuit rather than an
            // unfinished one. It cost a long hunt through four layers of
            // symptom, and the file itself knew the answer the whole time.
            const DEFAULT_CHAIN: usize = 65536;
            if lines > DEFAULT_CHAIN {
                println!(
                    "  note: {lines} commands exceeds the default \
                     maxCommandChainLength of {DEFAULT_CHAIN}; run\n\
                     \x20       `gamerule maxCommandChainLength {}`\n\
                     \x20       before this function, or it stops partway with no error",
                    lines.next_power_of_two().max(1 << 20)
                );
            }

            // Port coordinates in the same relative frame as the commands, so
            // the circuit can actually be driven and read once it is placed.
            for (name, levers) in &layout.input_levers {
                for (bit, &p) in levers.iter().enumerate() {
                    let r = rel(p);
                    println!("  input  {name}[{bit}] lever at ~{} ~{} ~{}", r.0, r.1, r.2);
                }
            }
            for (name, lamps) in &layout.output_lamps {
                for (bit, &p) in lamps.iter().enumerate() {
                    let r = rel(p);
                    println!("  output {name}[{bit}] lamp  at ~{} ~{} ~{}", r.0, r.1, r.2);
                }
            }
        }

        if let Some(nbt_path) = nbt_out {
            let s = structure::Structure::from_grid(&layout.grid);
            if !s.fits() {
                return Err(format!(
                    "structure is {}x{}x{}, but a structure block only loads up to {} per axis",
                    s.size.0, s.size.1, s.size.2, structure::MAX_AXIS
                ));
            }
            let bytes = s.to_bytes().map_err(|e| e.to_string())?;
            std::fs::write(&nbt_path, &bytes).map_err(|e| format!("{nbt_path}: {e}"))?;
            println!(
                "wrote {nbt_path}: {}x{}x{} ({} blocks) - load with a structure block",
                s.size.0, s.size.1, s.size.2, s.blocks.len()
            );
        }
    }
    Ok(())
}
