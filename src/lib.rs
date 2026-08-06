//! Compile the Ohm language into a Minecraft redstone circuit.
//!
//! See the README for the pipeline overview. The short version:
//! [`parser`] and [`lower`] build a finite state machine with a datapath,
//! [`bitblast`] turns it into a NOR-only gate netlist, [`tech`] and [`layout`]
//! turn that into blocks, and [`schem`] writes the schematic.
//!
//! Three independent simulators keep the whole thing honest: [`machine`]
//! (word-level golden model), [`netlist`] (gate level), and [`redstone`]
//! (the actual blocks, with Minecraft's power rules).

pub mod ast;
pub mod bitblast;
pub mod ir;
pub mod layout;
pub mod lexer;
pub mod lower;
pub mod machine;
pub mod nbt;
pub mod netlist;
pub mod parser;
pub mod redstone;
pub mod route;
pub mod schem;
pub mod tech;
pub mod world;
