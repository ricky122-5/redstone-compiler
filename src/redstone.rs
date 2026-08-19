//! Redstone power semantics and a tick-accurate simulator.
//!
//! This module is the single source of truth for "what does Minecraft do".
//! Every rule the compiler relies on is written down here explicitly so it can
//! be audited in one place rather than being smeared across the layout code.
//!
//! # The rules we model
//!
//! Power comes in two flavours:
//!
//! * **Strong power.** A block is strongly powered by a lit torch directly
//!   beneath it, by a powered repeater facing into it, or by a lever attached to
//!   it. A strongly powered block energises *all* adjacent dust to level 15.
//! * **Weak power.** A block is weakly powered by dust sitting on top of it, or
//!   by dust pointing into it. A weakly powered block does **not** energise
//!   adjacent dust, but it *does* extinguish a torch attached to it and it does
//!   drive a repeater reading from it.
//!
//! The compiler's cell library is deliberately built to depend only on the
//! unambiguous subset of these interactions:
//!
//! 1. dust on top of a block weakly powers that block;
//! 2. a torch is lit exactly when its support block is unpowered (weak counts);
//! 3. a lit torch strongly powers the block above it;
//! 4. a repeater strongly powers the block it faces into and restores level 15.
//!
//! # Timing model
//!
//! We simulate at redstone-tick granularity (1 redstone tick = 2 game ticks).
//! A torch inverts with 1 tick of delay; a repeater delays by its `delay`
//! setting. Dust propagates combinationally within a tick, which is what
//! Minecraft does for all practical purposes.
//!
//! We model **torch burnout**, and it is load-bearing. A torch driven past
//! roughly eight toggles in 60 game ticks goes out and stays out until it is
//! left alone again. That is how real redstone damps a circulating pulse, and
//! skipping it was a mistake: without it this simulator rings forever on any
//! feedback circuit, which made it useless for debugging exactly the latches and
//! flip-flops the compiler needs. The original justification - that generated
//! circuits are clocked well below the burnout threshold - does not hold, because
//! a deep NOR network glitches several times as a wavefront passes through its
//! reconvergent paths.
//!
//! Burnout must be modelled as *recoverable*. A torch that stops being driven
//! fast comes back, as it does in game when a block update re-evaluates it. An
//! earlier version of this code pruned the toggle history only when a torch
//! toggled, so a burnt torch could never recover and stayed pinned off forever -
//! which reported five dead torches in the flip-flop that the game does not have.
//!
//! Dust connects across a Y step only when the step is actually open: climbing
//! requires nothing opaque above the lower dust, and descending requires nothing
//! opaque above the lower dust either. Both checks were missing, which made this
//! model strictly **more connected** than Minecraft - it found wire paths the
//! game does not have. That is the exact signature of every "correct here, dead
//! in game" result this project has hit.
//!
//! We deliberately do **not** model sub-tick update ordering or
//! quasi-connectivity.
//!
//! # The freeze was never in the circuit: the server pauses without players
//!
//! Every "component follows one or two transitions and then freezes forever"
//! reading this project ever produced had one cause, and it was not redstone.
//! Modern servers ship `pause-when-empty-seconds=60`: sixty seconds after
//! starting with no player online, the world **pauses**. Console commands still
//! execute - `setblock` changes blocks, probe functions read states - but game
//! ticks stop, so every torch and repeater freezes at whatever state it held.
//! Dust still conducts (it has no scheduled tick), which made the freeze look
//! exactly like a component fault partway down a wire.
//!
//! The proof was in the timestamps: setblock #4 at start+54s propagated,
//! setblock #5 at start+64s did not, across a fresh-world run whose levers read
//! back correctly the whole way. With `pause-when-empty-seconds=-1` the same
//! world, same circuit, same sequence passes every stage - the D latch's RS
//! loop flips a third, fourth, fifth time exactly as the simulator says.
//!
//! Why it masqueraded so well: every small bring-up test (a NOR cell, a
//! repeater chain, a fan-out pair) finished inside sixty seconds and passed,
//! while every sequential test crossed the line mid-run and "froze" - which
//! looked precisely like complexity-dependent circuit failure. And because
//! `mc-validate.sh` boots a fresh world per input combination, each case reset
//! the clock, so the combinational suite never hit it at all.
//!
//! The harnesses now write `pause-when-empty-seconds=-1`. Nothing in this
//! module needed to change; the model was right about all of it.
//!
//! # setblock does not flip a lever the way a player does
//!
//! Replacing `lever[powered=true]` with `lever[powered=false]` via `setblock`
//! is a **same-block-type** replacement, so Minecraft skips `onRemove` and
//! notifies only the lever's direct neighbours. A real lever flip also updates
//! the neighbours of the block the lever is *attached to*, and a routed wire is
//! frequently adjacent to that support rather than to the lever itself - the
//! support is strongly powered while the lever is on, which is a legitimate and
//! useful way to enter a net at full strength. Those cells never hear the lever
//! turn off, and hold a stale 15 indefinitely.
//!
//! This produced a textbook false failure. `add2` in game read sum+1 on every
//! even input while all seventeen of its gate torches measured *correct*
//! against the netlist at every stage, and the block simulator - which
//! recomputes the whole field each tick and so cannot represent a missed update
//! - insisted the circuit was perfect. The giveaway was a power level that
//! *rose* along plain dust, 14 then 15, which no wire can do: the 15 was not
//! coming down the wire at all, it was the lever's support block, still stuck
//! on.
//!
//! The harnesses clear the lever cell to air before placing it in the wanted
//! state. Air-to-lever and lever-to-air both change block type, so the support
//! gets its update.
//!
//! Worth noting for any future harness: this had been latent for the whole
//! project. It only surfaced once the router fixes moved a wire from beside the
//! lever to beside its support.
//!
//! # Repeater orientation is verified against the game
//!
//! A repeater's output is `opposite(facing)`: `facing=North` drives the +Z end.
//! Measured, not assumed - `examples/rep_orient.rs` builds two
//! lever-dust-repeater-dust-lamp chains, one per orientation, and
//! `tools/rep-orient-run.sh` drives them through three on/off cycles in game.
//! The North chain tracks the lever every time; the South chain never lights.
//!
//! Worth measuring because a reversed repeater would have survived nearly every
//! in-game test this project has: `mc-validate.sh` builds a **fresh world per
//! input combination**, so almost every hardware pass we own is a *first*
//! transition after placement. That test also settles the more useful question -
//! repeaters do relay repeated transitions correctly - so a wire that carries
//! one change and then stops is not a repeater going stale.
//!
//! # Repeater locking is not modelled - but it is not the current fault
//!
//! A repeater whose *side* is driven by another powered repeater is **locked**:
//! it freezes at its current output and ignores its input. `world.rs` writes
//! `locked=false`, but that is only an initial value; the game recomputes
//! locking from whatever neighbours it finds, so an unintended lock would appear
//! in the built circuit and never here.
//!
//! It was the leading suspect for the flip-flop's stuck clock inverter and it is
//! ruled out: `examples/lock_audit.rs` checks the geometry directly and finds
//! **zero** locked repeaters across the RS latch, D latch and flip-flop (7, 29
//! and 75 repeaters respectively). Worth keeping as a standing audit, since the
//! router could introduce one at any time.
//!
//! # What the flip-flop does in game, and what is left
//!
//! The per-flop clock inverter that used to stick is gone - both clock phases
//! are inputs now - and with it the stuck-inverter symptom. In game the
//! flip-flop resets to a known state and its master captures a 1.
//!
//! What remains is an asymmetry that names its own cause. The master **sets**
//! but will not **reset**: MQ goes high on the clock edge with D=1 and stays
//! high on the next edge with D=0. Meanwhile the asynchronous clear does work.
//! CLR and R feed *the same latch cell* - B takes the loop on one input, R on
//! the second, CLR on the third - so a cell that clears via CLR but not via R is
//! not a broken latch. The fault is the wire into R, the routed link from
//! `r_gate.out` to `reset_feed` in [`crate::tech::stamp_d_latch`].
//!
//! Dust decay was the obvious suspect and it is **ruled out**:
//! `examples/dlatch_seq.rs` reports arriving strength on both links, and they
//! land at S=8 and R=12 out of 15. The broken direction is the one with the
//! *stronger* margin, so this is not a wire dying.
//!
//! What the game traces actually show, across two separate runs and three
//! different gates, is a single pattern: **a gate responds to the first input
//! change and then freezes.** The master's `!E` inverter goes low correctly on
//! the first clock edge and never returns. The old per-flop clock inverter did
//! exactly the same before it was removed. In both cases the harness reads the
//! driving levers back at the correct values, so the input really is changing
//! and the gate really is not following it.
//!
//! That shape does not fit decay, locking, or a logic error, all of which would
//! be wrong from the first edge rather than the second. It fits a *missing block
//! update*: a redstone torch relights only when something tells it to
//! re-evaluate, and a circuit assembled by thousands of individual `setblock`
//! calls can end up in a state where that notification never arrives. It is also
//! consistent with the placement problem the reset line was added to work
//! around, which is evidence for the same underlying cause rather than a second
//! one.
//!
//! Tested by placing the circuit twice, so every `setblock` in the second pass
//! delivers a block update to its neighbours. The result is partial and worth
//! recording precisely: the master's `!E` inverter now freezes **off** where it
//! previously froze **on**. One placement pass and it sticks high, two and it
//! sticks low. The freeze itself survives both.
//!
//! So block updates do reach these gates and do change the outcome - the state
//! a gate settles into depends on how the circuit was assembled - but a blanket
//! re-place is not the fix. What stays constant is the shape: one transition,
//! then frozen, regardless of which state it froze in. That rules out any
//! explanation tied to a particular level being stuck.
//!
//! Driving the inputs with redstone blocks instead of `setblock`-ing levers
//! changes nothing either - same freeze, same state - so the drive method is
//! ruled out along with decay and locking. The lever readback confirms the
//! input is genuinely low while the inverter output stays low with it.
//!
//! A torch whose support is unpowered and which stays out anyway is, by this
//! module\'s own rules, either burnt out or supported by a block something else
//! is powering. Burnout is now worth taking seriously rather than dismissing on
//! the ten-second stage spacing: the harness places the whole circuit **twice**,
//! and each pass re-places every block in it, so every torch is re-evaluated
//! thousands of times within a few seconds during setup. That is exactly the
//! rate that burns a torch out, and it happens before the first stage is ever
//! probed. It also explains why one placement pass and two produce opposite
//! frozen states.
//!
//! # What has been ruled out, and what that leaves
//!
//! Six explanations, each killed by a local check rather than a server run:
//!
//! | Hypothesis | How it died |
//! |---|---|
//! | repeater locking | `lock_audit`: zero locked repeaters |
//! | dust decay on the R link | `dlatch_seq`: arrives at 12/15, stronger than the working S link at 8 |
//! | missing block updates | placing twice flips *which* state it freezes in, not the freeze |
//! | the harness drive method | redstone blocks behave identically to levers |
//! | probe lamps perturbing the circuit | no behaviour change in simulation |
//! | probe lamps landing on a torch support | `lock_audit`: none do |
//!
//! The geometry is clean by every static check available, and the circuit is
//! correct in simulation. Meanwhile the game shows a gate whose input is
//! *demonstrably* low - the harness reads the driving lever back at every stage -
//! whose torch is out and will not relight.
//!
//! By the rules at the top of this file that is impossible: a torch is lit
//! exactly when its support is unpowered. So one of those rules is wrong or
//! incomplete for the situation these macros create, and the next step is to
//! find which. `tools/relight-probe.sh` starts that bisection from the bottom: a
//! single NOR cell, driven high and low three times **in one world**, and it
//! inverts correctly every time. So the cell library is not at fault and the
//! freeze needs composition - routed wires, output spines, or the latch macro -
//! to appear.
//!
//! That distinction had never been tested, because `mc-validate.sh` creates a
//! fresh world for every input combination. No gate it has ever checked was
//! asked to respond to a *second* change, so a freeze on the second edge was
//! invisible to the entire in-game suite. Any new harness should drive
//! sequences in one world for that reason.
//!
//! The next rung up settles it. `tools/dlatch-seq-probe.sh` drives a single D
//! latch through a full sequence in one world and it is **correct**: Q follows D
//! high, low, high, low, then holds when the enable drops and stays put while D
//! moves underneath it. No freeze on the second pass, no freeze at all.
//!
//! So: NOR cell correct, D latch correct, flip-flop wrong. The fault is in
//! *composing* two latches.
//!
//! The sharpest form of the contradiction is one gate. `not_e_r`, the enable
//! inverter, appears in both latches. In the standalone D latch it tracks its
//! enable through a whole sequence in game. In the flip-flop **both** copies
//! freeze after a single transition - the master\'s and the slave\'s - even
//! though the slave\'s enable is driven straight from a lever, exactly as the
//! standalone latch\'s is, and the harness reads that lever back at the correct
//! value every stage.
//!
//! Same cell, same drive, different behaviour depending only on what else is in
//! the world. That is not a property any per-gate explanation can have, and it
//! is why decay, locking, drive method and probe placement all came back clean:
//! they are all local, and this is not.
//!
//! What is left is interaction at a distance between the two macros. The
//! remaining candidate this model does not represent is dust connecting
//! *diagonally* across a Y step: two wires a level apart and one cell over do
//! not overlap, do not violate keepout, and do not touch any torch support, but
//! they do join into one net in game. The flip-flop is the first circuit built
//! with two large macros stacked in Z with routed wires threading between them,
//! which is exactly the geometry that produces near-misses in Y.
//!
//! `lock_audit` now reports these, and the raw count is **not** the signal: 2 in
//! the RS latch, 11 in the D latch, 36 in the flip-flop - and the first two work
//! in game. Most are intentional, because a ramp climbs in Y by exactly this
//! mechanism; the run (20,-3,83) -> (20,-2,84) -> (20,-1,85) -> (20,0,86) is a
//! ramp doing its job.
//!
//! Separating accidental from intentional needs to know which *net* each dust
//! cell belongs to, and a `Grid` does not carry that - `Router` does, in its
//! `owner` map. So the audit has to run with the router\'s ownership in hand and
//! flag only diagonal pairs whose two cells belong to different nets.
//!
//! # The decisive experiment, and what it inverted
//!
//! The obvious reading of "one gate, two behaviours" is that the two latch
//! macros interfere, so the flip-flop was rebuilt with the gap between them
//! widened from 14 to 60 and driven in game. It got **worse**: the master
//! stopped capturing at all, though its own structure and its lever-driven
//! inputs were untouched and only the slave moved.
//!
//! The only thing that scales with that constant is the length of the two
//! routed links from the master\'s Q spine to the slave\'s D feeds. Longer links,
//! more damage. So the disturbance comes from the *links*, not from the macros
//! being near each other - which also explains why the D latch is fine alone
//! (its links are all internal and short) and why every static audit came back
//! clean (they check for overlap and adjacency, not for whatever a long routed
//! wire does as it threads past a macro).
//!
//! That reframes the fix. Rather than hunting the mechanism, give these links
//! nowhere to do harm: route the master-to-slave connection on a dedicated Y
//! layer above both macros, the way the D latch\'s own crossing problem was
//! solved, instead of letting the maze router thread it through occupied space.

use crate::world::{down, offset, up, Block, Conn, Dir, Grid, Pos};
use std::collections::{HashMap, VecDeque};

pub const MAX_POWER: u8 = 15;

/// A torch burns out after this many toggles inside [`BURNOUT_WINDOW`]. It
/// recovers once the window passes quietly, as it does in game when a block
/// update re-evaluates it - so burnout damps a pulse rather than permanently
/// killing the torch.
pub const BURNOUT_TOGGLES: usize = 8;
/// The window burnout is measured over, in redstone ticks (60 game ticks).
pub const BURNOUT_WINDOW: u64 = 30;

/// Combinational snapshot: dust levels and block power, derived from the
/// current state of the active components.
pub struct Field {
    pub dust: HashMap<Pos, u8>,
    strong: HashMap<Pos, bool>,
    weak: HashMap<Pos, bool>,
}

impl Field {
    pub fn dust_at(&self, p: Pos) -> u8 {
        self.dust.get(&p).copied().unwrap_or(0)
    }

    /// True if the block at `p` is powered by any means. This is the predicate
    /// that decides whether a torch attached to `p` goes out.
    pub fn block_powered(&self, p: Pos) -> bool {
        self.strong.get(&p).copied().unwrap_or(false)
            || self.weak.get(&p).copied().unwrap_or(false)
    }

    pub fn block_strong(&self, p: Pos) -> bool {
        self.strong.get(&p).copied().unwrap_or(false)
    }
}

/// Mutable state of every active component in the grid.
#[derive(Clone, Debug, Default)]
pub struct SimState {
    pub torch_lit: HashMap<Pos, bool>,
    pub repeater_powered: HashMap<Pos, bool>,
    pub lever_on: HashMap<Pos, bool>,
    /// Scheduled transitions: position -> (tick at which it fires, new value).
    pending: HashMap<Pos, (u64, bool)>,
    /// When each torch last toggled, most recent first. Redstone torches burn
    /// out if driven too fast, and that is the mechanism the real game uses to
    /// damp a circulating pulse.
    torch_toggles: HashMap<Pos, Vec<u64>>,
    pub tick: u64,
}

impl SimState {
    /// How many transitions are still scheduled. Zero is what
    /// `run_until_stable` treats as settled, so a state with zero pending that
    /// still disagrees with a fresh run is a genuine fixed point, not a
    /// half-finished one.
    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }

    /// Drop every scheduled transition. Used when a probe forces a component
    /// into a chosen state and wants the circuit to react to that alone,
    /// without a stale schedule undoing it on the next tick.
    pub fn clear_for_probe(&mut self) {
        self.pending.clear();
    }
}

pub struct Sim<'g> {
    pub grid: &'g Grid,
    pub state: SimState,
    torches: Vec<Pos>,
    repeaters: Vec<Pos>,
}

impl<'g> Sim<'g> {
    pub fn new(grid: &'g Grid) -> Self {
        let mut state = SimState::default();
        let mut torches = Vec::new();
        let mut repeaters = Vec::new();
        for (&p, &b) in grid.iter() {
            match b {
                Block::WallTorch { lit, .. } | Block::Torch { lit } => {
                    state.torch_lit.insert(p, lit);
                    torches.push(p);
                }
                Block::Repeater { powered, .. } => {
                    state.repeater_powered.insert(p, powered);
                    repeaters.push(p);
                }
                Block::Lever { powered, .. } => {
                    state.lever_on.insert(p, powered);
                }
                _ => {}
            }
        }
        // Deterministic iteration order keeps runs reproducible.
        torches.sort();
        repeaters.sort();
        Sim { grid, state, torches, repeaters }
    }

    pub fn set_lever(&mut self, p: Pos, on: bool) {
        self.state.lever_on.insert(p, on);
    }

    /// Support block of a torch: the block it is mounted on.
    fn torch_support(&self, p: Pos) -> Pos {
        match self.grid.get(p) {
            // `facing` points away from the support, so the support is behind it.
            Block::WallTorch { facing, .. } => offset(p, facing.opposite()),
            Block::Torch { .. } => down(p),
            _ => down(p),
        }
    }

    /// Compute the combinational field from the current component states.
    pub fn field(&self) -> Field {
        let mut strong: HashMap<Pos, bool> = HashMap::new();
        let mut weak: HashMap<Pos, bool> = HashMap::new();

        // --- Strong power from active components ------------------------------
        for (&p, &b) in self.grid.iter() {
            match b {
                Block::WallTorch { .. } | Block::Torch { .. } => {
                    if self.state.torch_lit.get(&p).copied().unwrap_or(false) {
                        // A lit torch strongly powers the block above it.
                        let a = up(p);
                        if self.grid.get(a).conducts() {
                            strong.insert(a, true);
                        }
                    }
                }
                Block::Repeater { facing, .. } => {
                    if self.state.repeater_powered.get(&p).copied().unwrap_or(false) {
                        // Output is opposite the `facing` (input) side.
                        let out = offset(p, facing.opposite());
                        if self.grid.get(out).conducts() {
                            strong.insert(out, true);
                        }
                    }
                }
                Block::Lever { face, facing, .. } => {
                    if self.state.lever_on.get(&p).copied().unwrap_or(false) {
                        let support = match face {
                            crate::world::Face::Floor => down(p),
                            crate::world::Face::Ceiling => up(p),
                            crate::world::Face::Wall => offset(p, facing.opposite()),
                        };
                        if self.grid.get(support).conducts() {
                            strong.insert(support, true);
                        }
                    }
                }
                _ => {}
            }
        }

        // --- Dust levels ------------------------------------------------------
        let dust = self.solve_dust(&strong);

        // --- Weak power from dust --------------------------------------------
        for (&p, &level) in &dust {
            if level == 0 {
                continue;
            }
            for target in self.dust_powers(p) {
                // Any full block can *receive* weak power, including a lamp -
                // which is the whole point, since that is how output ports light
                // up. `conducts` is the narrower question of whether a block can
                // re-emit power onto dust, and only solid blocks do that; weak
                // power never propagates onward regardless.
                if self.grid.get(target).is_opaque() {
                    weak.insert(target, true);
                }
            }
        }

        Field { dust, strong, weak }
    }

    /// Which blocks a dust at `p` weakly powers: always the block beneath, plus
    /// the blocks it points into.
    ///
    /// Dust with a single connection renders as a straight line and therefore
    /// points at *both* ends of that axis - a real and frequently surprising
    /// Minecraft behaviour that we model faithfully so the layout checker can
    /// catch accidental couplings.
    fn dust_powers(&self, p: Pos) -> Vec<Pos> {
        let mut out = vec![down(p)];
        let shape = self.grid.dust_shape(p);
        let dirs: Vec<Dir> = Dir::ALL
            .into_iter()
            .filter(|&d| !matches!(shape_conn(&shape, d), Conn::None))
            .collect();
        match dirs.len() {
            0 => {}
            1 => {
                out.push(offset(p, dirs[0]));
                out.push(offset(p, dirs[0].opposite()));
            }
            _ => {
                for d in dirs {
                    out.push(offset(p, d));
                }
            }
        }
        out
    }

    /// Dust positions this dust conducts into (its electrical neighbours).
    fn dust_neighbors(&self, p: Pos) -> Vec<Pos> {
        let mut out = Vec::new();
        let shape = self.grid.dust_shape(p);
        for d in Dir::ALL {
            let n = offset(p, d);
            match shape_conn(&shape, d) {
                Conn::None => {}
                Conn::Up => {
                    // Dust only climbs a step when nothing opaque caps *this*
                    // dust: the wire runs up the side of the block, and a solid
                    // block overhead is what it would have to pass through.
                    if matches!(self.grid.get(up(n)), Block::Dust { .. })
                        && !self.grid.get(up(p)).conducts()
                    {
                        out.push(up(n));
                    }
                }
                Conn::Side => {
                    if matches!(self.grid.get(n), Block::Dust { .. }) {
                        out.push(n);
                    } else if matches!(self.grid.get(down(n)), Block::Dust { .. })
                        && !self.grid.get(n).conducts()
                    {
                        // Likewise going down: the step is only open if the
                        // block above the lower dust - which is `n` - is not
                        // solid.
                        out.push(down(n));
                    }
                }
            }
        }
        out
    }

    /// Seed every directly driven dust at 15, then relax outward losing one
    /// level per block. A bucket queue would be marginally faster, but circuits
    /// have few sources and the BFS is not the bottleneck.
    fn solve_dust(&self, strong: &HashMap<Pos, bool>) -> HashMap<Pos, u8> {
        let mut level: HashMap<Pos, u8> = HashMap::new();
        let mut q: VecDeque<Pos> = VecDeque::new();

        let seed = |level: &mut HashMap<Pos, u8>, q: &mut VecDeque<Pos>, p: Pos, v: u8| {
            if matches!(self.grid.get(p), Block::Dust { .. })
                && level.get(&p).copied().unwrap_or(0) < v
            {
                level.insert(p, v);
                q.push_back(p);
            }
        };

        for (&p, &b) in self.grid.iter() {
            match b {
                Block::WallTorch { .. } | Block::Torch { .. } => {
                    if self.state.torch_lit.get(&p).copied().unwrap_or(false) {
                        for d in Dir::ALL {
                            seed(&mut level, &mut q, offset(p, d), MAX_POWER);
                        }
                        seed(&mut level, &mut q, up(p), MAX_POWER);
                    }
                }
                Block::Repeater { facing, .. } => {
                    if self.state.repeater_powered.get(&p).copied().unwrap_or(false) {
                        seed(&mut level, &mut q, offset(p, facing.opposite()), MAX_POWER);
                    }
                }
                Block::Lever { .. } => {
                    if self.state.lever_on.get(&p).copied().unwrap_or(false) {
                        for d in Dir::ALL {
                            seed(&mut level, &mut q, offset(p, d), MAX_POWER);
                        }
                        seed(&mut level, &mut q, down(p), MAX_POWER);
                        seed(&mut level, &mut q, up(p), MAX_POWER);
                    }
                }
                _ => {}
            }
        }

        // Strongly powered blocks energise all adjacent dust to 15.
        for (&bp, &is_strong) in strong {
            if !is_strong {
                continue;
            }
            for d in Dir::ALL {
                seed(&mut level, &mut q, offset(bp, d), MAX_POWER);
            }
            seed(&mut level, &mut q, up(bp), MAX_POWER);
            seed(&mut level, &mut q, down(bp), MAX_POWER);
        }

        while let Some(p) = q.pop_front() {
            let cur = level[&p];
            if cur <= 1 {
                continue;
            }
            for n in self.dust_neighbors(p) {
                if level.get(&n).copied().unwrap_or(0) < cur - 1 {
                    level.insert(n, cur - 1);
                    q.push_back(n);
                }
            }
        }
        level
    }

    /// Is there an active signal entering the repeater at `rp` from its input side?
    fn repeater_input(&self, rp: Pos, facing: Dir, f: &Field) -> bool {
        let src = offset(rp, facing);
        match self.grid.get(src) {
            Block::Dust { .. } => f.dust_at(src) > 0,
            Block::WallTorch { .. } | Block::Torch { .. } => {
                self.state.torch_lit.get(&src).copied().unwrap_or(false)
            }
            Block::Lever { .. } => self.state.lever_on.get(&src).copied().unwrap_or(false),
            Block::Repeater { facing: f2, .. } => {
                // Only if that repeater's output points at us.
                offset(src, f2.opposite()) == rp
                    && self.state.repeater_powered.get(&src).copied().unwrap_or(false)
            }
            // Repeaters read both strong and weak power from a solid block.
            b if b.conducts() => f.block_powered(src),
            _ => false,
        }
    }

    /// Advance one redstone tick.
    pub fn step(&mut self) {
        self.state.tick += 1;
        let now = self.state.tick;

        // Fire everything scheduled for this tick.
        let due: Vec<(Pos, bool)> = self
            .state
            .pending
            .iter()
            .filter(|(_, (t, _))| *t <= now)
            .map(|(&p, &(_, v))| (p, v))
            .collect();
        for (p, v) in due {
            self.state.pending.remove(&p);
            match self.grid.get(p) {
                Block::WallTorch { .. } | Block::Torch { .. } => {
                    self.state.torch_lit.insert(p, v);
                    let hist = self.state.torch_toggles.entry(p).or_default();
                    hist.push(now);
                    hist.retain(|&t| now.saturating_sub(t) <= BURNOUT_WINDOW);
                }
                Block::Repeater { .. } => {
                    self.state.repeater_powered.insert(p, v);
                }
                _ => {}
            }
        }

        let f = self.field();

        // Re-evaluate every active component against the new field.
        for i in 0..self.torches.len() {
            let p = self.torches[i];
            // A torch driven past the burnout rate goes out and stays out. This
            // is what stops a pulse circulating forever round a non-inverting
            // loop in the real game; without it the simulator rings on any
            // feedback circuit and cannot be used to debug one.
            let burned = self.state.torch_toggles.get(&p).is_some_and(|h| {
                h.iter().filter(|&&t| now.saturating_sub(t) <= BURNOUT_WINDOW).count()
                    >= BURNOUT_TOGGLES
            });
            if burned {
                self.state.torch_lit.insert(p, false);
                self.state.pending.remove(&p);
                continue;
            }
            let support = self.torch_support(p);
            let target = !f.block_powered(support);
            self.schedule(p, target, self.state.torch_lit[&p], 1, now);
        }
        for i in 0..self.repeaters.len() {
            let p = self.repeaters[i];
            let (facing, delay) = match self.grid.get(p) {
                Block::Repeater { facing, delay, .. } => (facing, delay.max(1) as u64),
                _ => continue,
            };
            let target = self.repeater_input(p, facing, &f);
            self.schedule(p, target, self.state.repeater_powered[&p], delay, now);
        }
    }

    fn schedule(&mut self, p: Pos, target: bool, current: bool, delay: u64, now: u64) {
        if target == current {
            // The input went back to where it was before the change landed.
            self.state.pending.remove(&p);
        } else {
            match self.state.pending.get(&p) {
                Some(&(_, v)) if v == target => {} // already on its way
                _ => {
                    self.state.pending.insert(p, (now + delay, target));
                }
            }
        }
    }

    /// Torches that have burned out: driven past [`BURNOUT_TOGGLES`] inside
    /// [`BURNOUT_WINDOW`] and now stuck off. A burned-out torch in a settled
    /// circuit is a fault, not a transient - it will not recover on its own.
    pub fn burned_out(&self) -> Vec<Pos> {
        let mut v: Vec<Pos> = self
            .state
            .torch_toggles
            .iter()
            .filter(|(_, h)| {
                h.iter()
                    .filter(|&&t| self.state.tick.saturating_sub(t) <= BURNOUT_WINDOW)
                    .count()
                    >= BURNOUT_TOGGLES
            })
            .map(|(p, _)| *p)
            .collect();
        v.sort();
        v
    }

    /// Run until no transitions are pending, or `limit` ticks elapse.
    /// Returns the number of ticks actually run and whether it settled.
    pub fn run_until_stable(&mut self, limit: u64) -> (u64, bool) {
        let start = self.state.tick;
        for _ in 0..limit {
            self.step();
            if self.state.pending.is_empty() {
                return (self.state.tick - start, true);
            }
        }
        (self.state.tick - start, false)
    }

    pub fn run(&mut self, ticks: u64) {
        for _ in 0..ticks {
            self.step();
        }
    }

    /// Read a lamp's lit state, which is how output ports are observed.
    ///
    /// A lamp lights when it is powered, **or when any block touching it is
    /// powered**. That second clause is block powering: a powered block
    /// activates adjacent mechanisms even though weak power never spreads onto
    /// dust. Measured against the real game, not assumed - see the
    /// `lamp_beside_powered_block` case in `examples/conformance.rs`.
    ///
    /// Getting this wrong was subtle in both directions. An early version
    /// returned true for any adjacent *powered dust*, which was too permissive
    /// and hid a layout bug. Tightening it to "is this block powered" was
    /// correct for a lamp driven head-on but too strict here, and made every
    /// multi-bit output look right in simulation while unrelated wiring lit the
    /// lamps in-game.
    pub fn lamp_lit(&self, p: Pos) -> bool {
        let f = self.field();
        if f.block_powered(p) {
            return true;
        }
        for d in Dir::ALL {
            if f.block_powered(offset(p, d)) {
                return true;
            }
        }
        f.block_powered(up(p)) || f.block_powered(down(p))
    }
}

fn shape_conn(s: &crate::world::DustShape, d: Dir) -> Conn {
    match d {
        Dir::North => s.north,
        Dir::South => s.south,
        Dir::West => s.west,
        Dir::East => s.east,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::{Face, Material};

    /// A lever, a run of dust, and a lamp: checks seeding, decay, and readout.
    #[test]
    fn dust_decays_one_per_block() {
        let mut g = Grid::new();
        for z in 0..20 {
            g.force((0, 0, z), Block::Solid(Material::Wire));
            g.force((0, 1, z), Block::Dust { power: 0 });
        }
        g.force((0, 0, -1), Block::Solid(Material::Wire));
        g.force((0, 1, -1), Block::Lever { face: Face::Floor, facing: Dir::North, powered: true });

        let sim = Sim::new(&g);
        let f = sim.field();
        assert_eq!(f.dust_at((0, 1, 0)), 15);
        assert_eq!(f.dust_at((0, 1, 1)), 14);
        assert_eq!(f.dust_at((0, 1, 14)), 1);
        // Level 1 dust does not propagate further: it dies out.
        assert_eq!(f.dust_at((0, 1, 15)), 0);
    }

    /// The canonical inverter: dust on top of a block, torch on its side.
    /// This single interaction is what every gate in the compiler is built from.
    #[test]
    fn torch_inverts_its_support_block() {
        let mut g = Grid::new();
        // Support block with dust on top.
        g.force((0, 0, 0), Block::Solid(Material::Gate));
        g.force((0, 1, 0), Block::Dust { power: 0 });
        // Torch on the south face of the support.
        g.force((0, 0, 1), Block::WallTorch { facing: Dir::South, lit: true });
        // A lever feeding the input dust, initially off.
        g.force((0, 0, -1), Block::Solid(Material::Wire));
        g.force((0, 1, -1), Block::Lever { face: Face::Floor, facing: Dir::North, powered: false });

        let mut sim = Sim::new(&g);
        sim.run_until_stable(20);
        assert!(sim.state.torch_lit[&(0, 0, 1)], "input low => torch lit");

        sim.set_lever((0, 1, -1), true);
        sim.run_until_stable(20);
        assert!(!sim.state.torch_lit[&(0, 0, 1)], "input high => torch out");

        sim.set_lever((0, 1, -1), false);
        sim.run_until_stable(20);
        assert!(sim.state.torch_lit[&(0, 0, 1)], "input low again => torch relit");
    }

    /// A lit torch strongly powers the block above it, which then drives dust.
    #[test]
    fn torch_strongly_powers_block_above() {
        let mut g = Grid::new();
        g.force((0, 0, 0), Block::Solid(Material::Gate));
        g.force((0, 0, 1), Block::WallTorch { facing: Dir::South, lit: true });
        g.force((0, 1, 1), Block::Solid(Material::Gate));
        g.force((0, 2, 1), Block::Dust { power: 0 });

        let sim = Sim::new(&g);
        let f = sim.field();
        assert!(f.block_strong((0, 1, 1)));
        assert_eq!(f.dust_at((0, 2, 1)), 15);
    }

    /// Repeaters restore a decayed signal back to 15.
    #[test]
    fn repeater_restores_signal() {
        let mut g = Grid::new();
        for z in 0..14 {
            g.force((0, 0, z), Block::Solid(Material::Wire));
            g.force((0, 1, z), Block::Dust { power: 0 });
        }
        g.force((0, 0, -1), Block::Solid(Material::Wire));
        g.force((0, 1, -1), Block::Lever { face: Face::Floor, facing: Dir::North, powered: true });
        // Repeater reading from the north (from the dust run), driving south.
        g.force((0, 0, 14), Block::Solid(Material::Wire));
        g.force((0, 1, 14), Block::Repeater { facing: Dir::North, delay: 1, powered: false });
        g.force((0, 0, 15), Block::Solid(Material::Wire));
        g.force((0, 1, 15), Block::Dust { power: 0 });

        let mut sim = Sim::new(&g);
        sim.run_until_stable(20);
        assert!(sim.state.repeater_powered[&(0, 1, 14)]);
        assert_eq!(sim.field().dust_at((0, 1, 15)), 15);
    }

    /// Weak power must not leak onto dust: dust -> block -> dust is a dead end.
    #[test]
    fn weak_power_does_not_drive_dust() {
        let mut g = Grid::new();
        g.force((0, 0, 0), Block::Solid(Material::Wire));
        g.force((0, 1, 0), Block::Lever { face: Face::Floor, facing: Dir::North, powered: true });
        // Block to the south, with dust on top and dust beyond it.
        g.force((0, 1, 1), Block::Solid(Material::Gate));
        g.force((0, 1, 2), Block::Solid(Material::Wire));
        g.force((0, 2, 2), Block::Dust { power: 0 });

        let sim = Sim::new(&g);
        let f = sim.field();
        // The lever strongly powers its support, not the block to its side,
        // so nothing should reach the far dust.
        assert_eq!(f.dust_at((0, 2, 2)), 0);
    }
}
