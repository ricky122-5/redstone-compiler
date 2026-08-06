//! The 3D block world: directions, block states, and a sparse voxel grid.
//!
//! This is the shared substrate between the schematic emitter and the redstone
//! simulator. Both read the same `Grid`, which is what makes the differential
//! test meaningful: we simulate exactly the blocks we ship.

use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Dir {
    North, // -Z
    South, // +Z
    West,  // -X
    East,  // +X
}

impl Dir {
    pub const ALL: [Dir; 4] = [Dir::North, Dir::South, Dir::West, Dir::East];

    pub fn delta(self) -> (i32, i32, i32) {
        match self {
            Dir::North => (0, 0, -1),
            Dir::South => (0, 0, 1),
            Dir::West => (-1, 0, 0),
            Dir::East => (1, 0, 0),
        }
    }

    pub fn opposite(self) -> Dir {
        match self {
            Dir::North => Dir::South,
            Dir::South => Dir::North,
            Dir::West => Dir::East,
            Dir::East => Dir::West,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Dir::North => "north",
            Dir::South => "south",
            Dir::West => "west",
            Dir::East => "east",
        }
    }
}

pub type Pos = (i32, i32, i32);

pub fn offset(p: Pos, d: Dir) -> Pos {
    let (dx, dy, dz) = d.delta();
    (p.0 + dx, p.1 + dy, p.2 + dz)
}

pub fn up(p: Pos) -> Pos {
    (p.0, p.1 + 1, p.2)
}

pub fn down(p: Pos) -> Pos {
    (p.0, p.1 - 1, p.2)
}

/// Opaque full-cube materials. All of these conduct redstone power identically;
/// the distinction is purely visual so the finished build is legible in-game.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Material {
    /// Substrate under gate cells.
    Gate,
    /// Substrate under routing channels.
    Wire,
    /// Shield blocks that deliberately break dust up-connections.
    Shield,
    /// Marks an input port.
    PortIn,
    /// Marks an output port.
    PortOut,
    /// Clock distribution.
    Clock,
}

impl Material {
    pub fn id(self) -> &'static str {
        match self {
            Material::Gate => "minecraft:stone",
            Material::Wire => "minecraft:polished_andesite",
            Material::Shield => "minecraft:smooth_stone",
            Material::PortIn => "minecraft:lime_concrete",
            Material::PortOut => "minecraft:red_concrete",
            Material::Clock => "minecraft:blue_concrete",
        }
    }
}

/// Which surface a lever is mounted on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Face {
    Floor,
    Wall,
    Ceiling,
}

impl Face {
    pub fn name(self) -> &'static str {
        match self {
            Face::Floor => "floor",
            Face::Wall => "wall",
            Face::Ceiling => "ceiling",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Block {
    Air,
    Solid(Material),
    /// Redstone dust. `power` is a render/emit hint; the simulator recomputes it.
    Dust { power: u8 },
    /// Torch mounted on the side of a block. `facing` is the direction the torch
    /// points *away* from its support, so the support block is at `facing.opposite()`.
    WallTorch { facing: Dir, lit: bool },
    /// Torch standing on top of a block; its support is the block below.
    Torch { lit: bool },
    /// `facing` is the direction from the OUTPUT side to the INPUT side, matching
    /// Minecraft's blockstate convention. So the repeater reads from `facing` and
    /// drives toward `facing.opposite()`.
    Repeater { facing: Dir, delay: u8, powered: bool },
    Lever { face: Face, facing: Dir, powered: bool },
    Lamp { lit: bool },
}

impl Block {
    /// A full opaque cube. Opaque blocks block dust up-connections and can carry
    /// redstone power.
    pub fn is_opaque(self) -> bool {
        matches!(self, Block::Solid(_) | Block::Lamp { .. })
    }

    /// Whether this block can be powered and then re-emit that power to dust.
    /// Same set as `is_opaque` in our restricted cell library, but named
    /// separately because the concepts diverge for e.g. slabs and glass.
    pub fn conducts(self) -> bool {
        matches!(self, Block::Solid(_))
    }

    /// The Minecraft blockstate string, used as a schematic palette key.
    pub fn state_id(self, shape: DustShape) -> String {
        match self {
            Block::Air => "minecraft:air".to_string(),
            Block::Solid(m) => m.id().to_string(),
            Block::Dust { power } => format!(
                "minecraft:redstone_wire[east={},north={},power={},south={},west={}]",
                shape.get(Dir::East),
                shape.get(Dir::North),
                power.min(15),
                shape.get(Dir::South),
                shape.get(Dir::West),
            ),
            Block::WallTorch { facing, lit } => {
                if lit {
                    format!("minecraft:redstone_wall_torch[facing={},lit=true]", facing.name())
                } else {
                    format!("minecraft:redstone_wall_torch[facing={},lit=false]", facing.name())
                }
            }
            Block::Torch { lit } => {
                format!("minecraft:redstone_torch[lit={lit}]")
            }
            Block::Repeater { facing, delay, powered } => format!(
                "minecraft:repeater[delay={},facing={},locked=false,powered={}]",
                delay.clamp(1, 4),
                facing.name(),
                powered
            ),
            Block::Lever { face, facing, powered } => format!(
                "minecraft:lever[face={},facing={},powered={}]",
                face.name(),
                facing.name(),
                powered
            ),
            Block::Lamp { lit } => format!("minecraft:redstone_lamp[lit={lit}]"),
        }
    }
}

/// How a dust block visually/functionally connects on each horizontal side.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DustShape {
    pub north: Conn,
    pub south: Conn,
    pub west: Conn,
    pub east: Conn,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Conn {
    #[default]
    None,
    Side,
    Up,
}

impl Conn {
    fn name(self) -> &'static str {
        match self {
            Conn::None => "none",
            Conn::Side => "side",
            Conn::Up => "up",
        }
    }
}

impl DustShape {
    pub fn get(self, d: Dir) -> &'static str {
        match d {
            Dir::North => self.north.name(),
            Dir::South => self.south.name(),
            Dir::West => self.west.name(),
            Dir::East => self.east.name(),
        }
    }

    pub fn set(&mut self, d: Dir, c: Conn) {
        match d {
            Dir::North => self.north = c,
            Dir::South => self.south = c,
            Dir::West => self.west = c,
            Dir::East => self.east = c,
        }
    }

    /// Dust with no horizontal connections renders as a dot, but Minecraft
    /// stores that as all-`none`, which is what `Default` already gives us.
    pub fn is_dot(self) -> bool {
        self == DustShape::default()
    }
}

/// A sparse voxel grid. Circuits are mostly air, so a hash map beats a dense
/// array until the very end, where we materialize a dense array for emission.
#[derive(Debug, Default, Clone)]
pub struct Grid {
    cells: HashMap<Pos, Block>,
}

impl Grid {
    pub fn new() -> Self {
        Grid::default()
    }

    pub fn get(&self, p: Pos) -> Block {
        self.cells.get(&p).copied().unwrap_or(Block::Air)
    }

    /// Place a block, refusing to silently clobber an existing non-air block.
    /// Placement/routing bugs otherwise show up as mysterious logic errors much
    /// later, so we fail loudly at the point of the collision.
    pub fn set(&mut self, p: Pos, b: Block) -> Result<(), String> {
        if let Some(&existing) = self.cells.get(&p) {
            if existing != b && existing != Block::Air {
                return Err(format!(
                    "block collision at {p:?}: tried to place {b:?} over {existing:?}"
                ));
            }
        }
        self.cells.insert(p, b);
        Ok(())
    }

    /// Place without the collision check, for deliberate overwrites.
    pub fn force(&mut self, p: Pos, b: Block) {
        self.cells.insert(p, b);
    }

    pub fn is_free(&self, p: Pos) -> bool {
        !self.cells.contains_key(&p) || self.cells[&p] == Block::Air
    }

    pub fn iter(&self) -> impl Iterator<Item = (&Pos, &Block)> {
        self.cells.iter()
    }

    pub fn len(&self) -> usize {
        self.cells.len()
    }

    pub fn is_empty(&self) -> bool {
        self.cells.is_empty()
    }

    /// Inclusive bounding box (min, max), or None for an empty grid.
    pub fn bounds(&self) -> Option<(Pos, Pos)> {
        let mut it = self.cells.keys();
        let first = *it.next()?;
        let mut lo = first;
        let mut hi = first;
        for &(x, y, z) in it {
            lo = (lo.0.min(x), lo.1.min(y), lo.2.min(z));
            hi = (hi.0.max(x), hi.1.max(y), hi.2.max(z));
        }
        Some((lo, hi))
    }

    /// Compute the connection shape of the dust at `p`.
    ///
    /// Rules (Minecraft Java):
    /// - connect SIDE to an adjacent dust, or an adjacent component that accepts
    ///   dust (torch, lever, or a repeater whose input or output faces us);
    /// - connect UP if the neighbor is opaque, carries dust on top, and the block
    ///   directly above us is not opaque (an opaque roof blocks the climb);
    /// - connect DOWN (rendered as `side`) if the neighbor is non-opaque and has
    ///   dust one level below.
    pub fn dust_shape(&self, p: Pos) -> DustShape {
        let mut shape = DustShape::default();
        if !matches!(self.get(p), Block::Dust { .. }) {
            return shape;
        }
        let roofed = self.get(up(p)).is_opaque();
        for d in Dir::ALL {
            let n = offset(p, d);
            let nb = self.get(n);
            let c = if matches!(nb, Block::Dust { .. }) {
                Conn::Side
            } else if self.accepts_dust_side(n, d) {
                Conn::Side
            } else if nb.is_opaque()
                && matches!(self.get(up(n)), Block::Dust { .. })
                && !roofed
            {
                Conn::Up
            } else if !nb.is_opaque() && matches!(self.get(down(n)), Block::Dust { .. }) {
                Conn::Side
            } else {
                Conn::None
            };
            shape.set(d, c);
        }
        shape
    }

    /// Whether the component at `n` connects to dust approaching from direction `d`
    /// (i.e. the dust is at `n - d`).
    fn accepts_dust_side(&self, n: Pos, d: Dir) -> bool {
        match self.get(n) {
            Block::WallTorch { .. } | Block::Torch { .. } | Block::Lever { .. } => true,
            // A repeater only connects along its own axis: input at `facing`,
            // output at `facing.opposite()`. Dust beside it on the other axis
            // does not connect.
            Block::Repeater { facing, .. } => {
                let axis = d;
                axis == facing || axis == facing.opposite()
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dir_round_trips() {
        for d in Dir::ALL {
            assert_eq!(d.opposite().opposite(), d);
            let (dx, dy, dz) = d.delta();
            let (ox, oy, oz) = d.opposite().delta();
            assert_eq!((dx + ox, dy + oy, dz + oz), (0, 0, 0));
        }
    }

    #[test]
    fn collision_is_rejected() {
        let mut g = Grid::new();
        g.set((0, 0, 0), Block::Solid(Material::Gate)).unwrap();
        let err = g.set((0, 0, 0), Block::Dust { power: 0 }).unwrap_err();
        assert!(err.contains("collision"), "{err}");
    }

    #[test]
    fn identical_reblock_is_allowed() {
        let mut g = Grid::new();
        g.set((0, 0, 0), Block::Solid(Material::Gate)).unwrap();
        g.set((0, 0, 0), Block::Solid(Material::Gate)).unwrap();
    }

    #[test]
    fn dust_connects_to_adjacent_dust() {
        let mut g = Grid::new();
        g.force((0, 0, 0), Block::Solid(Material::Wire));
        g.force((0, 1, 0), Block::Dust { power: 0 });
        g.force((0, 0, 1), Block::Solid(Material::Wire));
        g.force((0, 1, 1), Block::Dust { power: 0 });
        let s = g.dust_shape((0, 1, 0));
        assert_eq!(s.south, Conn::Side);
        assert_eq!(s.north, Conn::None);
    }

    #[test]
    fn opaque_roof_blocks_up_connection() {
        // Dust at origin, opaque neighbor to the south carrying dust on top.
        let mut g = Grid::new();
        g.force((0, 0, 0), Block::Solid(Material::Wire));
        g.force((0, 1, 0), Block::Dust { power: 0 });
        g.force((0, 1, 1), Block::Solid(Material::Wire));
        g.force((0, 2, 1), Block::Dust { power: 0 });
        assert_eq!(g.dust_shape((0, 1, 0)).south, Conn::Up);

        // Now roof the source dust: the climb is blocked.
        g.force((0, 2, 0), Block::Solid(Material::Shield));
        assert_eq!(g.dust_shape((0, 1, 0)).south, Conn::None);
    }

    #[test]
    fn repeater_connects_only_along_its_axis() {
        let mut g = Grid::new();
        g.force((0, 0, 0), Block::Solid(Material::Wire));
        g.force((0, 1, 0), Block::Dust { power: 0 });
        // Repeater to the south, reading from the north (i.e. from our dust).
        g.force((0, 1, 1), Block::Repeater { facing: Dir::North, delay: 1, powered: false });
        assert_eq!(g.dust_shape((0, 1, 0)).south, Conn::Side);

        // A repeater on the east/west axis placed to our south does not connect.
        g.force((0, 1, 1), Block::Repeater { facing: Dir::East, delay: 1, powered: false });
        assert_eq!(g.dust_shape((0, 1, 0)).south, Conn::None);
    }
}
