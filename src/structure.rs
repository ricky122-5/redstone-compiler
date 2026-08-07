//! Vanilla structure-block NBT export.
//!
//! The Sponge `.schem` format needs WorldEdit. This emits the format vanilla
//! Minecraft itself uses for structure blocks, so a generated circuit can be
//! loaded in an unmodded creative world:
//!
//! 1. put the file in `saves/<world>/generated/minecraft/structures/<name>.nbt`
//! 2. place a structure block, set it to Load mode, type `<name>`, hit Load.
//!
//! The layout is a palette of block states plus an explicit list of
//! `(state, pos)` pairs. Unlike the schematic format there is no dense array, so
//! air is simply omitted - which suits these circuits, since they are mostly
//! air. Structure blocks cap out at 48 blocks per axis.

use crate::nbt::{write_root, Tag};
use crate::schem::DATA_VERSION;
use crate::world::{Block, Grid};
use flate2::write::GzEncoder;
use flate2::Compression;
use std::collections::HashMap;
use std::io::Write;

/// Largest structure a structure block will load, per axis.
pub const MAX_AXIS: i32 = 48;

/// Split `minecraft:repeater[delay=1,facing=north]` into its name and
/// properties. Reusing the blockstate string keeps one source of truth for how
/// a block is spelled, rather than duplicating that knowledge here.
fn split_state(id: &str) -> (String, Vec<(String, String)>) {
    match id.split_once('[') {
        None => (id.to_string(), Vec::new()),
        Some((name, rest)) => {
            let rest = rest.trim_end_matches(']');
            let props = rest
                .split(',')
                .filter(|s| !s.is_empty())
                .filter_map(|kv| kv.split_once('='))
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
            (name.to_string(), props)
        }
    }
}

pub struct Structure {
    pub size: (i32, i32, i32),
    pub palette: Vec<String>,
    /// `(palette index, x, y, z)`, positions relative to the structure corner.
    pub blocks: Vec<(u32, i32, i32, i32)>,
}

impl Structure {
    pub fn from_grid(grid: &Grid) -> Structure {
        let Some((lo, hi)) = grid.bounds() else {
            return Structure { size: (1, 1, 1), palette: vec![], blocks: vec![] };
        };
        let size = (hi.0 - lo.0 + 1, hi.1 - lo.1 + 1, hi.2 - lo.2 + 1);

        let mut palette: Vec<String> = Vec::new();
        let mut index: HashMap<String, u32> = HashMap::new();
        let mut blocks = Vec::new();

        // Deterministic output: sort by position so two runs of the compiler
        // produce byte-identical files.
        let mut cells: Vec<_> = grid.iter().map(|(&p, &b)| (p, b)).collect();
        cells.sort_by_key(|&(p, _)| (p.1, p.2, p.0));

        for (p, b) in cells {
            if b == Block::Air {
                continue;
            }
            let shape = if matches!(b, Block::Dust { .. }) {
                grid.dust_shape(p)
            } else {
                Default::default()
            };
            let id = b.state_id(shape);
            let next = palette.len() as u32;
            let idx = *index.entry(id.clone()).or_insert_with(|| {
                palette.push(id);
                next
            });
            blocks.push((idx, p.0 - lo.0, p.1 - lo.1, p.2 - lo.2));
        }

        Structure { size, palette, blocks }
    }

    /// Whether a structure block can actually load this.
    pub fn fits(&self) -> bool {
        self.size.0 <= MAX_AXIS && self.size.1 <= MAX_AXIS && self.size.2 <= MAX_AXIS
    }

    pub fn to_nbt(&self) -> Tag {
        let palette = Tag::List(
            10,
            self.palette
                .iter()
                .map(|id| {
                    let (name, props) = split_state(id);
                    let mut fields = vec![("Name".to_string(), Tag::String(name))];
                    if !props.is_empty() {
                        fields.push((
                            "Properties".to_string(),
                            Tag::Compound(
                                props
                                    .into_iter()
                                    .map(|(k, v)| (k, Tag::String(v)))
                                    .collect(),
                            ),
                        ));
                    }
                    Tag::Compound(fields)
                })
                .collect(),
        );

        let blocks = Tag::List(
            10,
            self.blocks
                .iter()
                .map(|&(state, x, y, z)| {
                    Tag::Compound(vec![
                        ("state".into(), Tag::Int(state as i32)),
                        (
                            "pos".into(),
                            Tag::List(3, vec![Tag::Int(x), Tag::Int(y), Tag::Int(z)]),
                        ),
                    ])
                })
                .collect(),
        );

        Tag::Compound(vec![
            ("DataVersion".into(), Tag::Int(DATA_VERSION)),
            (
                "size".into(),
                Tag::List(3, vec![Tag::Int(self.size.0), Tag::Int(self.size.1), Tag::Int(self.size.2)]),
            ),
            ("palette".into(), palette),
            ("blocks".into(), blocks),
            ("entities".into(), Tag::List(10, vec![])),
        ])
    }

    pub fn to_bytes(&self) -> std::io::Result<Vec<u8>> {
        // Structure files are gzipped NBT with an unnamed root compound.
        let raw = write_root("", &self.to_nbt());
        let mut enc = GzEncoder::new(Vec::new(), Compression::default());
        enc.write_all(&raw)?;
        enc.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::{Block, Material};
    use flate2::read::GzDecoder;
    use std::io::Read;

    #[test]
    fn splits_block_states() {
        assert_eq!(split_state("minecraft:stone"), ("minecraft:stone".into(), vec![]));
        let (name, props) = split_state("minecraft:repeater[delay=1,facing=north]");
        assert_eq!(name, "minecraft:repeater");
        assert_eq!(
            props,
            vec![("delay".to_string(), "1".to_string()), ("facing".into(), "north".into())]
        );
    }

    #[test]
    fn air_is_omitted_and_palette_shared() {
        let mut g = Grid::new();
        g.force((0, 0, 0), Block::Solid(Material::Gate));
        g.force((2, 0, 0), Block::Solid(Material::Gate));
        let s = Structure::from_grid(&g);
        assert_eq!(s.size, (3, 1, 1));
        assert_eq!(s.palette.len(), 1, "identical blocks share a palette entry");
        assert_eq!(s.blocks.len(), 2, "the air gap is not listed");
    }

    #[test]
    fn positions_are_relative_to_the_corner() {
        let mut g = Grid::new();
        g.force((10, 20, 30), Block::Solid(Material::Gate));
        g.force((11, 21, 32), Block::Solid(Material::Wire));
        let s = Structure::from_grid(&g);
        assert_eq!(s.size, (2, 2, 3));
        assert!(s.blocks.contains(&(0, 0, 0, 0)));
        assert!(s.blocks.contains(&(1, 1, 1, 2)));
    }

    #[test]
    fn output_is_gzipped_nbt_with_the_expected_keys() {
        let mut g = Grid::new();
        g.force((0, 0, 0), Block::Solid(Material::Gate));
        let bytes = Structure::from_grid(&g).to_bytes().unwrap();
        assert_eq!(&bytes[..2], &[0x1f, 0x8b], "gzip magic");
        let mut raw = Vec::new();
        GzDecoder::new(&bytes[..]).read_to_end(&mut raw).unwrap();
        assert_eq!(raw[0], 0x0a, "unnamed root compound");
        for key in [&b"DataVersion"[..], b"size", b"palette", b"blocks", b"entities"] {
            assert!(
                raw.windows(key.len()).any(|w| w == key),
                "missing key {}",
                String::from_utf8_lossy(key)
            );
        }
    }

    #[test]
    fn size_limit_is_reported() {
        let mut g = Grid::new();
        g.force((0, 0, 0), Block::Solid(Material::Gate));
        g.force((60, 0, 0), Block::Solid(Material::Gate));
        let s = Structure::from_grid(&g);
        assert!(!s.fits(), "61 blocks wide exceeds a structure block's 48");
    }
}

/// Emit the circuit as a `.mcfunction`: one `setblock` per block, placed
/// relative to whoever runs it.
///
/// This is the no-mods, no-UI way to get a circuit in-game — `/function ohm:foo`
/// and it appears. It also sidesteps the structure block's 48-block axis limit.
///
/// **Order matters.** Redstone dust, torches and levers pop off the instant they
/// have no support, and `setblock` fires neighbour updates immediately. So all
/// solid blocks go down first, then the attached components, then dust last.
pub fn to_mcfunction(grid: &Grid, origin: (i32, i32, i32)) -> String {
    let Some((lo, _)) = grid.bounds() else { return String::new() };
    let mut cells: Vec<_> = grid.iter().map(|(&p, &b)| (p, b)).collect();
    cells.sort_by_key(|&(p, _)| (p.1, p.2, p.0));

    // 0 = supports, 1 = attached components, 2 = dust.
    let phase = |b: &Block| match b {
        Block::Dust { .. } => 2,
        Block::WallTorch { .. } | Block::Torch { .. } | Block::Repeater { .. }
        | Block::Lever { .. } => 1,
        _ => 0,
    };

    let mut out = String::new();
    out.push_str("# generated by ohmc - run with /function <namespace>:<name>\n");
    for want in 0..=2 {
        for &(p, b) in &cells {
            if b == Block::Air || phase(&b) != want {
                continue;
            }
            let shape = if matches!(b, Block::Dust { .. }) {
                grid.dust_shape(p)
            } else {
                Default::default()
            };
            // Relative coordinates so the circuit lands wherever the player is.
            out.push_str(&format!(
                "setblock ~{} ~{} ~{} {}\n",
                p.0 - lo.0 + origin.0,
                p.1 - lo.1 + origin.1,
                p.2 - lo.2 + origin.2,
                b.state_id(shape)
            ));
        }
    }
    out
}
