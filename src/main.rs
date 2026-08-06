//! `ohmc` - command line driver. All the compiler logic lives in the library.


use ohmc::{bitblast, layout, lower, machine, parser, schem};
use std::process::ExitCode;

const USAGE: &str = "\
ohmc - compile Ohm to redstone

USAGE:
    ohmc <file.ohm> [OPTIONS]

OPTIONS:
    --run a=1,b=2      execute the program on the golden model and print outputs
    -o <file.schem>    write a Sponge v3 schematic (combinational programs only)
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
    let mut stats = false;

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

    if let Some(out_path) = out {
        let comb = bitblast::blast_combinational(&design).map_err(|e| {
            format!(
                "{e}\n\
                 note: programs with loops or branches synthesise to a verified gate\n\
                 note: netlist, but placing them needs a sequential floorplan\n\
                 note: (clock spine + flip-flop cells) that is not implemented yet."
            )
        })?;
        let layout = layout::build(&comb)?;
        let s = schem::Schematic::from_grid(&layout.grid);
        let bytes = s.to_bytes().map_err(|e| e.to_string())?;
        std::fs::write(&out_path, &bytes).map_err(|e| format!("{out_path}: {e}"))?;
        println!(
            "wrote {out_path}: {}x{}x{} ({} blocks, {} gates, {} levels, {} KiB)",
            s.width,
            s.height,
            s.length,
            layout.grid.len(),
            layout.gates,
            layout.levels,
            bytes.len() / 1024
        );
    }
    Ok(())
}
