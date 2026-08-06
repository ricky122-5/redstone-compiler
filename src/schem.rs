//! Sponge Schematic v3 emitter.
//!
//! Layout of the format (see the SpongePowered/Schematic-Specification repo):
//! a gzip-compressed NBT file whose root compound holds a single `Schematic`
//! compound. Block indices live in `Blocks.Data` as a flat varint stream ordered
//! `x + z * Width + y * Width * Length`, indexing into `Blocks.Palette`.

use crate::nbt::{write_root, write_varint, Tag};
use crate::world::{Block, Grid};
use flate2::write::GzEncoder;
use flate2::Compression;
use std::collections::HashMap;
use std::io::Write;

/// Minecraft 1.20.4. Any reasonably recent value works; WorldEdit uses it to
/// decide whether the blocks need upgrading before pasting.
pub const DATA_VERSION: i32 = 3700;

pub struct Schematic {
    pub width: u16,
    pub height: u16,
    pub length: u16,
    pub palette: Vec<String>,
    pub data: Vec<u32>,
}

impl Schematic {
    pub fn from_grid(grid: &Grid) -> Schematic {
        let Some((lo, hi)) = grid.bounds() else {
            return Schematic {
                width: 1,
                height: 1,
                length: 1,
                palette: vec!["minecraft:air".into()],
                data: vec![0],
            };
        };
        let width = (hi.0 - lo.0 + 1) as usize;
        let height = (hi.1 - lo.1 + 1) as usize;
        let length = (hi.2 - lo.2 + 1) as usize;

        let mut palette: Vec<String> = Vec::new();
        let mut index: HashMap<String, u32> = HashMap::new();
        // Air is index 0 so the (overwhelmingly common) empty cell is a 1-byte varint.
        index.insert("minecraft:air".to_string(), 0);
        palette.push("minecraft:air".to_string());

        let mut data = vec![0u32; width * height * length];
        for (&p, &b) in grid.iter() {
            if b == Block::Air {
                continue;
            }
            // Dust connection shape has to be baked in: it is a real blockstate
            // property that determines connectivity, and a pasted schematic may
            // not receive the neighbour updates that would otherwise fix it.
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
            let (x, y, z) = ((p.0 - lo.0) as usize, (p.1 - lo.1) as usize, (p.2 - lo.2) as usize);
            data[x + z * width + y * width * length] = idx;
        }

        Schematic {
            width: width as u16,
            height: height as u16,
            length: length as u16,
            palette,
            data,
        }
    }

    pub fn to_nbt(&self) -> Tag {
        let mut block_data = Vec::with_capacity(self.data.len());
        for &v in &self.data {
            write_varint(&mut block_data, v);
        }
        // NBT byte arrays are signed; the bit pattern is what matters.
        let block_data: Vec<u8> = block_data;

        let palette = Tag::Compound(
            self.palette
                .iter()
                .enumerate()
                .map(|(i, s)| (s.clone(), Tag::Int(i as i32)))
                .collect(),
        );

        let blocks = Tag::Compound(vec![
            ("Palette".into(), palette),
            ("Data".into(), Tag::ByteArray(block_data)),
            ("BlockEntities".into(), Tag::List(10, vec![])),
        ]);

        let metadata = Tag::Compound(vec![
            ("Name".into(), Tag::String("ohmc output".into())),
            ("Author".into(), Tag::String("ohmc".into())),
        ]);

        let schematic = Tag::Compound(vec![
            ("Version".into(), Tag::Int(3)),
            ("DataVersion".into(), Tag::Int(DATA_VERSION)),
            ("Width".into(), Tag::Short(self.width as i16)),
            ("Height".into(), Tag::Short(self.height as i16)),
            ("Length".into(), Tag::Short(self.length as i16)),
            ("Offset".into(), Tag::IntArray(vec![0, 0, 0])),
            ("Metadata".into(), metadata),
            ("Blocks".into(), blocks),
        ]);

        Tag::Compound(vec![("Schematic".into(), schematic)])
    }

    /// Gzip-compressed NBT, ready to drop in a WorldEdit schematics folder.
    pub fn to_bytes(&self) -> std::io::Result<Vec<u8>> {
        let raw = write_root("", &self.to_nbt());
        let mut enc = GzEncoder::new(Vec::new(), Compression::default());
        enc.write_all(&raw)?;
        enc.finish()
    }

    pub fn volume(&self) -> usize {
        self.width as usize * self.height as usize * self.length as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::{Block, Material};
    use flate2::read::GzDecoder;
    use std::io::Read;

    #[test]
    fn single_block_schematic_has_expected_extent() {
        let mut g = Grid::new();
        g.force((5, 7, 9), Block::Solid(Material::Gate));
        let s = Schematic::from_grid(&g);
        assert_eq!((s.width, s.height, s.length), (1, 1, 1));
        assert_eq!(s.palette, vec!["minecraft:air", "minecraft:stone"]);
        assert_eq!(s.data, vec![1]);
    }

    #[test]
    fn indices_follow_x_z_y_order() {
        let mut g = Grid::new();
        // 2x2x2 box so we can check the index formula unambiguously.
        g.force((0, 0, 0), Block::Solid(Material::Gate));
        g.force((1, 0, 0), Block::Solid(Material::Wire));
        g.force((0, 0, 1), Block::Solid(Material::Shield));
        g.force((0, 1, 0), Block::Solid(Material::Clock));
        let s = Schematic::from_grid(&g);
        assert_eq!((s.width, s.height, s.length), (2, 2, 2));
        let at = |x: usize, y: usize, z: usize| s.data[x + z * 2 + y * 2 * 2];
        assert_eq!(s.palette[at(0, 0, 0) as usize], "minecraft:stone");
        assert_eq!(s.palette[at(1, 0, 0) as usize], "minecraft:polished_andesite");
        assert_eq!(s.palette[at(0, 0, 1) as usize], "minecraft:smooth_stone");
        assert_eq!(s.palette[at(0, 1, 0) as usize], "minecraft:blue_concrete");
        assert_eq!(at(1, 1, 1), 0, "unset cells are air");
    }

    #[test]
    fn output_is_valid_gzip_with_nbt_header() {
        let mut g = Grid::new();
        g.force((0, 0, 0), Block::Solid(Material::Gate));
        let bytes = Schematic::from_grid(&g).to_bytes().unwrap();
        assert_eq!(&bytes[..2], &[0x1f, 0x8b], "gzip magic");

        let mut raw = Vec::new();
        GzDecoder::new(&bytes[..]).read_to_end(&mut raw).unwrap();
        // TAG_Compound, empty name, then the "Schematic" compound.
        assert_eq!(raw[0], 0x0a);
        assert_eq!(&raw[1..3], &[0x00, 0x00]);
        assert_eq!(raw[3], 0x0a);
        assert_eq!(&raw[6..15], b"Schematic");
    }
}
