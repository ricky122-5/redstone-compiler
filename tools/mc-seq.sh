#!/bin/bash
#
# End-to-end validation of a *sequential* design against the real game.
#
# `mc-validate.sh` sweeps input combinations of a combinational circuit and
# diffs the lamps against `ohmc --truth`. A design with a control FSM cannot be
# checked that way: it has no truth table, it has a *trace*. Placing it is not
# enough either - it has to be reset and then clocked, and the reset is two
# distinct things that have to happen in the right order:
#
#   * the flip-flops' asynchronous clear, which zeroes every register the
#     instant it is asserted, and is what puts the build into a known state
#     after a `.mcfunction` places it one block at a time;
#
#   * the netlist's own `Src::Reset`, which is *synchronous*. All it does is
#     raise the entry block's D input, so it must be clocked in. Assert both
#     with the clock still - the obvious thing to do - and the state vector is
#     cleared and then never loaded, so no block is ever active and the machine
#     sits there computing nothing while looking perfectly healthy.
#
# Usage: tools/mc-seq.sh examples/tick.ohm [cycles] [server_dir]
set -u

OHM="${1:-examples/tick.ohm}"
CYCLES="${2:-14}"
D="${3:-/tmp/ohm-seq}"
JAR_URL="https://piston-data.mojang.com/v1/objects/4707d00eb834b446575d89a61a11b5d548d8c001/server.jar"
JAR_SHA="4707d00eb834b446575d89a61a11b5d548d8c001"
REPO="$(cd "$(dirname "$0")/.." && pwd)"
OHMC="$REPO/target/release/ohmc"

[ -x "$OHMC" ] || { echo "build first: cargo build --release"; exit 1; }
mkdir -p "$D"

INFO=$("$OHMC" "$OHM" --mcfn "$D/circuit.mcfunction") || exit 1
echo "$INFO"
DEPTH=$("$OHMC" "$OHM" --stats | sed -n 's/.*logic depth *\([0-9]*\).*/\1/p')
DEPTH=${DEPTH:-8}
# A clock phase has to be held long enough for the whole cone to settle. The
# block simulator measures ~116 ticks per cycle once halted and up to ~220 while
# the FSM is moving; at 20 ticks a second that is a good few seconds, and too
# short reads a half-propagated state that looks exactly like a logic bug.
# A clock phase must be long enough in *game ticks*, and the server does not
# necessarily run at twenty a second: this build is 60,000 blocks across a
# thousand ticking chunks, and it runs several times slower than real time. The
# block simulator measures 116 ticks per cycle once halted and up to 220 while
# the FSM is moving - six to eleven seconds at full speed, and much longer here.
# Too short reads a half-propagated state, which looks exactly like a logic bug.
PHASE=${OHMC_PHASE:-$(( 12 + DEPTH ))}
echo "logic depth $DEPTH -> ${PHASE}s per clock phase, $CYCLES cycles"

# Port coordinates, in the same frame as the emitted commands.
# Data levers only. The compiler prints these as "  input  port0[0] lever at
# ...", and the control levers on their own lines - matching on "lever at"
# alone would sweep the clock as though it were an input bit.
LEVERS=$(echo "$INFO" | sed -n 's/^  input .* lever at ~\([0-9-]*\) ~\([0-9-]*\) ~\([0-9-]*\).*/\1 \2 \3/p')
LAMPS=$(echo "$INFO"  | sed -n 's/.*lamp  at ~\([0-9-]*\) ~\([0-9-]*\) ~\([0-9-]*\).*/\1 \2 \3/p')
CLK=$(echo "$INFO"    | sed -n 's/.*clock   lever at ~\([0-9-]*\) ~\([0-9-]*\) ~\([0-9-]*\).*/\1 \2 \3/p')
CLKN=$(echo "$INFO"   | sed -n 's/.*clock_n lever at ~\([0-9-]*\) ~\([0-9-]*\) ~\([0-9-]*\).*/\1 \2 \3/p')
CLR=$(echo "$INFO"    | sed -n 's/.*reset   lever at ~\([0-9-]*\) ~\([0-9-]*\) ~\([0-9-]*\).*/\1 \2 \3/p')
# The state vector, so a wrong lamp can be told from a stuck machine.
QS=$(echo "$INFO"     | sed -n 's/^  state  q\[[0-9]*\] dust  at ~\([0-9-]*\) ~\([0-9-]*\) ~\([0-9-]*\).*/\1 \2 \3/p')
# Where the clock and clear arrive at each register, so a frozen machine can be
# traced to the trunk that failed rather than guessed at.
FEEDS=$(echo "$INFO"  | sed -n 's/^  feed   \([a-z]*\)\[\([0-9]*\)\] dust  at ~\([0-9-]*\) ~\([0-9-]*\) ~\([0-9-]*\).*/\1\2 \3 \4 \5/p')
SRST=$(echo "$INFO"   | sed -n 's/.*state reset lever at ~\([0-9-]*\) ~\([0-9-]*\) ~\([0-9-]*\).*/\1 \2 \3/p')
for n in CLK CLKN CLR SRST; do
  eval "v=\$$n"
  [ -n "$v" ] || { echo "no $n lever reported - is this a sequential design?"; exit 1; }
done
echo "clock $CLK | clock_n $CLKN | clear $CLR | state reset $SRST"

read -r BX BY BZ MINX MINZ < <(python3 - "$D/circuit.mcfunction" <<'PYX'
import re,sys
mx=my=mz=0; nx=nz=0
for l in open(sys.argv[1]):
    m=re.match(r'setblock ~(-?\d+) ~(-?\d+) ~(-?\d+)', l)
    if m:
        x,y,z=(int(g) for g in m.groups())
        mx=max(mx,x); my=max(my,y); mz=max(mz,z)
        nx=min(nx,x); nz=min(nz,z)
print(mx+8, my+8, mz+8, nx-8, nz-8)
PYX
)
echo "circuit spans x ${MINX}..${BX}  z ${MINZ}..${BZ}"

cd "$D"
if [ ! -f server.jar ]; then curl -sSL -o server.jar "$JAR_URL"; fi
echo "$JAR_SHA  server.jar" | shasum -c - >/dev/null || { echo "server.jar SHA mismatch"; exit 1; }
JAVA="$HOME/Library/Application Support/minecraft/runtime/java-runtime-delta/mac-os-arm64/java-runtime-delta/jre.bundle/Contents/Home/bin/java"
[ -x "$JAVA" ] || JAVA=java

echo "eula=true" > eula.txt
# pause-when-empty-seconds is load-bearing: modern servers stop ticking the
# world 60s after starting with nobody online. Commands keep working, so a long
# headless run silently freezes partway through and reads as a broken circuit.
cat > server.properties <<'EOF'
level-type=minecraft\:flat
gamemode=creative
online-mode=false
spawn-protection=0
max-tick-time=-1
spawn-npcs=false
spawn-animals=false
spawn-monsters=false
pause-when-empty-seconds=-1
EOF

rm -rf world
PK=world/datapacks/ohm
mkdir -p "$PK/data/ohm/function"
cat > "$PK/pack.mcmeta" <<'EOF'
{"pack":{"description":"ohmc sequential validation","pack_format":61,"supported_formats":{"min_inclusive":4,"max_inclusive":99}}}
EOF
cp circuit.mcfunction "$PK/data/ohm/function/circuit.mcfunction"

: > "$PK/data/ohm/function/probe.mcfunction"
i=0
while read -r lx ly lz; do
  [ -n "$lx" ] || continue
  cat >> "$PK/data/ohm/function/probe.mcfunction" <<EOF
execute if block $lx $ly $lz minecraft:redstone_lamp[lit=true] run say OHMC_BIT $i 1
execute unless block $lx $ly $lz minecraft:redstone_lamp[lit=true] run say OHMC_BIT $i 0
EOF
  i=$((i+1))
done <<< "$LAMPS"
NLAMP=$i
# Read the *data* levers back as well as the control ones.
#
# Only the control levers were verified, which left the one input that actually
# distinguishes one test case from another completely unchecked. If a data lever
# silently fails to take, every case computes whatever the default input is and
# the sweep reports a uniform wrong answer - which looks exactly like a broken
# circuit and is not. This project has lost whole debugging cycles to precisely
# that, four times in the `add2` hunt alone, so nothing that drives the circuit
# goes unverified.
i=0
while read -r lx ly lz; do
  [ -n "$lx" ] || continue
  cat >> "$PK/data/ohm/function/probe.mcfunction" <<EOF
execute if block $lx $ly $lz minecraft:lever[powered=true] run say OHMC_IN $i 1
execute if block $lx $ly $lz minecraft:lever[powered=false] run say OHMC_IN $i 0
execute unless block $lx $ly $lz minecraft:lever run say OHMC_IN $i missing
EOF
  i=$((i+1))
done <<< "$LEVERS"

# The state vector, register by register.
#
# A lamp that reads wrong could be anything from the input levers to the output
# spine, and from outside the machine there is no way to tell which. The block
# simulator prints Q every cycle for exactly that reason; without the same trace
# here, a disagreement between simulator and game has no common ground to be
# compared on. `power=0` on the Q dust means the register is clear.
i=0
while read -r qx qy qz; do
  [ -n "$qx" ] || continue
  cat >> "$PK/data/ohm/function/probe.mcfunction" <<EOF
execute if block $qx $qy $qz minecraft:redstone_wire[power=0] run say OHMC_Q $i 0
execute unless block $qx $qy $qz minecraft:redstone_wire[power=0] run say OHMC_Q $i 1
EOF
  i=$((i+1))
done <<< "$QS"

# The control feeds, reported with their actual power level rather than a bit.
#
# A frozen machine says only that something upstream died; the level at the pad
# says whether the signal arrived at all, arrived weak, or arrived fine and was
# ignored - three different faults that look identical from the lamps. Reported
# as "missing" when the block is not dust at all, which is what an unsupported
# wire looks like after Minecraft drops it.
while read -r nm fx fy fz; do
  [ -n "$nm" ] || continue
  cat >> "$PK/data/ohm/function/probe.mcfunction" <<EOF
execute unless block $fx $fy $fz minecraft:redstone_wire run say OHMC_FEED $nm missing
EOF
  for lvl in 0 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15; do
    cat >> "$PK/data/ohm/function/probe.mcfunction" <<EOF
execute if block $fx $fy $fz minecraft:redstone_wire[power=$lvl] run say OHMC_FEED $nm $lvl
EOF
  done
done <<< "$FEEDS"

# Three separate harness bugs in this project
# have produced "logic mismatches" that were really the harness failing to drive
# the circuit, so it verifies its own inputs rather than assuming they took.
for nm in CLK CLKN CLR SRST; do
  eval "set -- \$$nm"
  cat >> "$PK/data/ohm/function/probe.mcfunction" <<EOF
execute if block $1 $2 $3 minecraft:lever[powered=true] run say OHMC_CTL $nm 1
execute if block $1 $2 $3 minecraft:lever[powered=false] run say OHMC_CTL $nm 0
execute unless block $1 $2 $3 minecraft:lever run say OHMC_CTL $nm missing
EOF
done

rm -f server.log cmd; mkfifo cmd; exec 3<>cmd
"$JAVA" -Xmx3G -jar server.jar nogui < cmd > server.log 2>&1 &
SRV=$!
for _ in $(seq 1 150); do
  grep -q 'Done (' server.log && break
  kill -0 $SRV 2>/dev/null || { echo "server died"; tail -20 server.log; exit 1; }
  sleep 2
done
grep -q 'Done (' server.log || { echo "server timeout"; tail -20 server.log; exit 1; }

send() { echo "$1" >&3; sleep "${2:-1}"; }

# Keep the whole build ticking.
#
# `forceload` caps at 256 chunks *per dimension*, which is a 256x256 square, and
# a sequential build is bigger than that - a register bank sits behind the gate
# array, so the two together are a few hundred blocks each way. Parts outside
# the loaded region still accept setblock and still answer probes; they just
# never run redstone, which reads as a fixed wrong answer rather than as a
# failure. That exact trap cost this project a whole debugging cycle on `add`.
#
# spawnChunkRadius has no such cap: it keeps a square of chunks around spawn
# loaded and ticking. 16 gives 33x33 chunks, 528 blocks each way, which covers
# anything this compiler currently emits.
# Radius chosen to cover the build and no more. Every loaded chunk is ticked
# every tick, so an oversized square costs real time on a server that is
# already the slow part of this loop.
RAD=$(python3 -c "
import math
span = max(abs($MINX), abs($MINZ), $BX, $BZ)
print(min(32, span // 16 + 2))")
echo "spawnChunkRadius $RAD (covers +/-$((RAD*16)) blocks)"
# A function stops after `maxCommandChainLength` commands, and says nothing.
#
# The default is 65536. `count.ohm` is 87226 setblock commands, so the last
# quarter of the build was silently never placed - and because the emitter
# writes dust last, what went missing was exactly the wiring: every control
# feed and every register\'s Q read back as "not redstone dust at all", the
# machine sat frozen with a state vector that never changed, and it looked for
# all the world like a dead circuit. `tick.ohm` is 60486 commands and fits,
# which is why it validated in game and this did not.
send "gamerule maxCommandChainLength 10000000" 2
send "gamerule spawnChunkRadius $RAD" 2
send "forceload add $MINX $MINZ $BX $BZ" 2
send "reload" 3
send "execute positioned 0.0 0.0 0.0 run function ohm:circuit" 5

# A lever is driven by clearing the cell to air first.
#
# setblock from lever[powered=true] straight to lever[powered=false] is a
# same-block-type replacement, so Minecraft skips onRemove and notifies only the
# lever's direct neighbours - not the neighbours of the block it is attached to,
# which is where a gate's wire usually sits. Without the air step those cells
# never hear the lever change and hold a stale value for ever.
lever() { # x y z state
  send "setblock $1 $2 $3 minecraft:air" 0
  send "setblock $1 $2 $3 minecraft:lever[face=floor,facing=north,powered=$4]" 0
}
set_ctl() { eval "set -- \$$1 \$2"; lever "$1" "$2" "$3" "$4"; }

# One full clock cycle: master transparent, then the falling edge the slave
# captures on. The two phases are driven as explicit complements rather than
# from one lever and an inverter, so the non-overlap is the harness's business.
cycle() {
  set_ctl CLK true;  set_ctl CLKN false; sleep "$PHASE"
  set_ctl CLK false; set_ctl CLKN true;  sleep "$PHASE"
}

run_input() {
  local v=$1 b=0
  while read -r x y z; do
    [ -n "$x" ] || continue
    st=false; [ $(( (v >> b) & 1 )) = 1 ] && st=true
    lever "$x" "$y" "$z" "$st"
    b=$((b+1))
  done <<< "$LEVERS"

  # Clock idle, then clear asynchronously.
  set_ctl CLK false; set_ctl CLKN true; set_ctl SRST false
  set_ctl CLR true;  sleep "$PHASE"
  set_ctl CLR false; sleep "$PHASE"
  # Then load the entry state: hold the synchronous reset and clock it in.
  set_ctl SRST true
  cycle
  set_ctl SRST false; sleep "$PHASE"

  send "say OHMC_INPUT $v" 1
  for c in $(seq 1 "$CYCLES"); do
    cycle
    send "say OHMC_CYCLE $c" 1
    send "function ohm:probe" 2
  done
}

NLEV=$(echo "$LEVERS" | grep -c .)
[ "$NLEV" -gt 0 ] || { echo "no data input levers parsed - the sweep would be vacuous"; exit 1; }
FULL=$((1 << NLEV))
CASES=${OHMC_CASES:-$FULL}
[ "$CASES" -le "$FULL" ] || CASES=$FULL
echo "running $CASES of $FULL input value(s), $CYCLES cycles each"
for v in $(seq 0 $((CASES-1))); do run_input "$v"; done

send "stop" 3
wait $SRV 2>/dev/null

echo
echo "=== observed in Minecraft vs. the golden model ==="
# Build the model's arguments from the *reported port names*, not a guess.
#
# This used to pass `go=$v` unconditionally, which happened to be right for
# `tick.ohm` and is wrong for everything else - `count.ohm`'s port is `n`, and
# the model simply errored out, leaving the run with nothing to compare against.
# The compiler now prints each lever as `input NAME[bit]`, so the names and the
# bit widths can both be read straight off it, and a value is split across ports
# in the same lever order the sweep drives them.
PORTS=$(echo "$INFO" | sed -n 's/^  input  \([A-Za-z_][A-Za-z0-9_]*\)\[\([0-9]*\)\].*/\1 \2/p')
for v in $(seq 0 $((CASES-1))); do
  ARGS=$(python3 - "$v" <<PYP
import sys, collections
v = int(sys.argv[1])
widths = collections.OrderedDict()
for line in """$PORTS""".strip().splitlines():
    if not line.strip():
        continue
    name, bit = line.split()
    widths[name] = max(widths.get(name, 0), int(bit) + 1)
parts, shift = [], 0
for name, w in widths.items():
    parts.append(f"{name}={(v >> shift) & ((1 << w) - 1)}")
    shift += w
print(",".join(parts))
PYP
)
  echo "--- input $v  ($ARGS) ---"
  "$OHMC" "$OHM" --run "$ARGS" 2>&1 | sed 's/^/  model: /'
done

python3 - "$NLAMP" "$CASES" <<'PYEOF'
import re, sys
nlamp, cases = int(sys.argv[1]), int(sys.argv[2])
log = open("server.log", encoding="utf8", errors="replace").read()
runs, cur, cyc = {}, None, None
ctl = {}
drove = {}
qvec = {}
for line in log.splitlines():
    m = re.search(r"OHMC_INPUT (\d+)", line)
    if m:
        cur = int(m.group(1)); runs[cur] = {}; continue
    m = re.search(r"OHMC_CYCLE (\d+)", line)
    if m:
        cyc = int(m.group(1)); continue
    m = re.search(r"OHMC_BIT (\d+) ([01])", line)
    if m and cur is not None and cyc is not None:
        runs[cur].setdefault(cyc, {})[int(m.group(1))] = int(m.group(2))
    m = re.search(r"OHMC_CTL (\w+) (\S+)", line)
    if m and cur is not None and cyc is not None:
        ctl.setdefault((cur, cyc), {})[m.group(1)] = m.group(2)
    m = re.search(r"OHMC_IN (\d+) (\S+)", line)
    if m and cur is not None and cyc is not None:
        drove.setdefault((cur, cyc), {})[int(m.group(1))] = m.group(2)
    m = re.search(r"OHMC_Q (\d+) ([01])", line)
    if m and cur is not None and cyc is not None:
        qvec.setdefault((cur, cyc), {})[int(m.group(1))] = int(m.group(2))

for v in sorted(runs):
    # What the circuit was actually driven with, read back off the levers.
    #
    # Printed before the answer, and checked against the value the sweep asked
    # for, because a lever that silently failed to take makes every case compute
    # the same wrong thing - indistinguishable from a logic fault by looking at
    # the lamps alone.
    seen = set()
    for (rv, _c), bits in drove.items():
        if rv == v:
            seen.add(sum(int(b) << i for i, b in bits.items() if b in ("0", "1")))
    if not seen:
        print(f"input {v}: [HARNESS: no lever readback]")
    elif seen != {v}:
        print(f"input {v}: [HARNESS: asked for {v}, levers read back {sorted(seen)}]")
    print(f"input {v}:")
    prev = None
    for c in sorted(runs[v]):
        bits = runs[v][c]
        val = sum(bits.get(i, 0) << i for i in range(nlamp))
        q = qvec.get((v, c), {})
        qs = "".join(str(q[k]) for k in sorted(q)) if q else "?"
        if val != prev:
            print(f"   cycle {c:>3}: lamps = {val}   Q={qs}")
            prev = val
    # The last state vector, so a machine that never moved is obvious even when
    # the lamps happen to read the right answer.
    last_c = max(runs[v]) if runs[v] else None
    lq = qvec.get((v, last_c), {})
    print(f"   final: {prev}   Q={''.join(str(lq[k]) for k in sorted(lq)) if lq else '?'}")
PYEOF
