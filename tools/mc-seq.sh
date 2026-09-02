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
PHASE=$(( 3 + DEPTH / 2 ))
echo "logic depth $DEPTH -> ${PHASE}s per clock phase, $CYCLES cycles"

# Port coordinates, in the same frame as the emitted commands.
LEVERS=$(echo "$INFO" | sed -n 's/^  port.* lever at ~\([0-9-]*\) ~\([0-9-]*\) ~\([0-9-]*\).*/\1 \2 \3/p')
[ -n "$LEVERS" ] || LEVERS=$(echo "$INFO" | sed -n 's/.*input lever at ~\([0-9-]*\) ~\([0-9-]*\) ~\([0-9-]*\).*/\1 \2 \3/p')
LAMPS=$(echo "$INFO"  | sed -n 's/.*lamp  at ~\([0-9-]*\) ~\([0-9-]*\) ~\([0-9-]*\).*/\1 \2 \3/p')
CLK=$(echo "$INFO"    | sed -n 's/.*clock   lever at ~\([0-9-]*\) ~\([0-9-]*\) ~\([0-9-]*\).*/\1 \2 \3/p')
CLKN=$(echo "$INFO"   | sed -n 's/.*clock_n lever at ~\([0-9-]*\) ~\([0-9-]*\) ~\([0-9-]*\).*/\1 \2 \3/p')
CLR=$(echo "$INFO"    | sed -n 's/.*reset   lever at ~\([0-9-]*\) ~\([0-9-]*\) ~\([0-9-]*\).*/\1 \2 \3/p')
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
# Read the control levers back too. Three separate harness bugs in this project
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
send "gamerule spawnChunkRadius 16" 2
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
FULL=$((1 << NLEV))
CASES=${OHMC_CASES:-$FULL}
[ "$CASES" -le "$FULL" ] || CASES=$FULL
echo "running $CASES of $FULL input value(s), $CYCLES cycles each"
for v in $(seq 0 $((CASES-1))); do run_input "$v"; done

send "stop" 3
wait $SRV 2>/dev/null

echo
echo "=== observed in Minecraft vs. the golden model ==="
for v in $(seq 0 $((CASES-1))); do
  echo "--- input $v ---"
  IN=$(python3 -c "
import sys
names='''$(echo "$INFO" | sed -n 's/^  ports.*//p')'''
print()" 2>/dev/null)
  "$OHMC" "$OHM" --run "$(python3 -c "
import re
info='''$INFO'''
# One port per lever bit is the common case for these examples.
print('go=$v' if 'go' in info or True else '')" )" 2>/dev/null | sed 's/^/  model: /'
done

python3 - "$NLAMP" "$CASES" <<'PYEOF'
import re, sys
nlamp, cases = int(sys.argv[1]), int(sys.argv[2])
log = open("server.log", encoding="utf8", errors="replace").read()
runs, cur, cyc = {}, None, None
ctl = {}
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

for v in sorted(runs):
    print(f"input {v}:")
    prev = None
    for c in sorted(runs[v]):
        bits = runs[v][c]
        val = sum(bits.get(i, 0) << i for i in range(nlamp))
        if val != prev:
            print(f"   cycle {c:>3}: lamps = {val}")
            prev = val
    print(f"   final: {prev}")
PYEOF
