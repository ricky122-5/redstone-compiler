#!/bin/bash
#
# End-to-end validation against the real game.
#
# Compiles an Ohm program, boots a headless vanilla Minecraft server, places the
# circuit with a datapack, drives every input combination by toggling the lever
# blocks, and reads the output lamps back out of the server log. The observed
# results are diffed against `ohmc --truth`.
#
# This is the only check in the project that does not rely on our own model of
# redstone. Everything else - including the block-level simulator - is a model
# that could be wrong in the same way twice. Running it found two real bugs the
# unit tests could not: output lamps were mounted beside a wire whose shape
# pointed the other way and so never lit, and the simulator's lamp check was
# permissive enough to hide it.
#
# Usage: tools/mc-validate.sh examples/invert.ohm [server_dir]
set -u

OHM="${1:-examples/invert.ohm}"
D="${2:-/tmp/ohm-mc}"
JAR_URL="https://piston-data.mojang.com/v1/objects/4707d00eb834b446575d89a61a11b5d548d8c001/server.jar"
JAR_SHA="4707d00eb834b446575d89a61a11b5d548d8c001"
REPO="$(cd "$(dirname "$0")/.." && pwd)"
OHMC="$REPO/target/release/ohmc"

[ -x "$OHMC" ] || { echo "build first: cargo build --release"; exit 1; }
mkdir -p "$D"

# --- compile ---------------------------------------------------------------
INFO=$("$OHMC" "$OHM" --mcfn "$D/circuit.mcfunction") || exit 1
echo "$INFO"
TRUTH=$("$OHMC" "$OHM" --truth) || exit 1
# Settling time has to scale with the circuit, not be a fixed guess. Each NOR
# cell is a repeater plus a torch (2 redstone ticks = 0.2s) and routing adds
# more, so a deep circuit needs seconds. Too short and the probe reads a
# half-propagated result, which looks exactly like a logic bug.
DEPTH=$("$OHMC" "$OHM" --stats | sed -n 's/.*logic depth *\([0-9]*\).*/\1/p')
DEPTH=${DEPTH:-8}
SETTLE=$(( 4 + DEPTH / 2 ))
echo "logic depth $DEPTH -> settling ${SETTLE}s per case"

# Actual extents of the emitted circuit. A hardcoded clearing volume silently
# misses relay chains, which run far out in Z - and surviving leftovers corrupt
# the next case exactly like a logic bug.
read -r BX BY BZ < <(python3 - "$D/circuit.mcfunction" <<'PYX'
import re,sys
mx=my=mz=0
for l in open(sys.argv[1]):
    m=re.match(r'setblock ~(-?\d+) ~(-?\d+) ~(-?\d+)', l)
    if m:
        x,y,z=(int(g) for g in m.groups())
        mx=max(mx,x); my=max(my,y); mz=max(mz,z)
print(mx+8, my+8, mz+8)
PYX
)
echo "circuit extends to ${BX}x${BY}x${BZ}"

# Port coordinates, as reported by the compiler in the same frame as the
# generated commands.
LEVERS=$(echo "$INFO" | sed -n 's/.*lever at ~\([0-9-]*\) ~\([0-9-]*\) ~\([0-9-]*\).*/\1 \2 \3/p')
LAMPS=$(echo "$INFO"  | sed -n 's/.*lamp  at ~\([0-9-]*\) ~\([0-9-]*\) ~\([0-9-]*\).*/\1 \2 \3/p')
NLEV=$(echo "$LEVERS" | grep -c .)
[ "$NLEV" -gt 0 ] || { echo "no input levers reported"; exit 1; }
# Designs wider than a handful of inputs are sampled rather than enumerated.
#
# The old cap of six refused anything larger outright, which meant the largest
# design that had ever been checked on hardware was a 51-gate adder. `add` is
# 195 gates and sixteen levers - 65536 combinations - and was never validated in
# game at all, so three router bugs that made it compute wrong answers went
# unnoticed until a sampled sweep in the simulator found them. Sampling a
# stride through the space visits pairs of cases that differ in many bits,
# which is where those bugs showed.
MAXCASES=${OHMC_MAXCASES:-24}

# --- server ----------------------------------------------------------------
cd "$D"
if [ ! -f server.jar ]; then curl -sSL -o server.jar "$JAR_URL"; fi
echo "$JAR_SHA  server.jar" | shasum -c - >/dev/null || { echo "server.jar SHA mismatch"; exit 1; }

JAVA="$HOME/Library/Application Support/minecraft/runtime/java-runtime-delta/mac-os-arm64/java-runtime-delta/jre.bundle/Contents/Home/bin/java"
[ -x "$JAVA" ] || JAVA=java

echo "eula=true" > eula.txt
# pause-when-empty-seconds is the single most important line here. Modern
# servers pause the world 60 seconds after starting with no players online.
# Commands still run - setblock changes blocks, probes read states - but game
# ticks stop, so every torch and repeater freezes at whatever state it held.
# With a headless harness no player ever joins, so any run longer than a minute
# silently stops computing partway through. That one default produced every
# "gate responds once then freezes forever" reading in this project's history
# and cost days of debugging a circuit that was never broken.
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

# Wipe the world on every run. Circuits are placed at fixed coordinates, so
# leftovers from a previous run sit underneath the new one and silently corrupt
# the reading - which cost a whole debugging cycle chasing a bug that was not
# there.
rm -rf world
PK=world/datapacks/ohm
mkdir -p "$PK/data/ohm/function"
cat > "$PK/pack.mcmeta" <<'EOF'
{"pack":{"description":"ohmc validation","pack_format":61,"supported_formats":{"min_inclusive":4,"max_inclusive":99}}}
EOF
cp circuit.mcfunction "$PK/data/ohm/function/circuit.mcfunction"

# One probe per output bit, emitting a parseable marker.
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

# Read the input levers back. Three separate harness bugs have now produced
# "logic mismatches" that were really the harness failing to drive the circuit,
# so the harness verifies its own inputs rather than assuming they took.
j=0
while read -r vx vy vz; do
  [ -n "$vx" ] || continue
  cat >> "$PK/data/ohm/function/probe.mcfunction" <<EOF
execute if block $vx $vy $vz minecraft:lever[powered=true] run say OHMC_LEV $j 1
execute if block $vx $vy $vz minecraft:lever[powered=false] run say OHMC_LEV $j 0
execute unless block $vx $vy $vz minecraft:lever run say OHMC_LEV $j missing
EOF
  j=$((j+1))
done <<< "$LEVERS"

rm -f server.log cmd; mkfifo cmd; exec 3<>cmd
"$JAVA" -Xmx2G -jar server.jar nogui < cmd > server.log 2>&1 &
SRV=$!
for _ in $(seq 1 150); do
  grep -q 'Done (' server.log && break
  kill -0 $SRV 2>/dev/null || { echo "server died"; tail -20 server.log; exit 1; }
  sleep 2
done
grep -q 'Done (' server.log || { echo "server timeout"; tail -20 server.log; exit 1; }

send() { echo "$1" >&3; sleep "${2:-1}"; }
# Force-load the region the circuit actually occupies.
#
# This used to be hardcoded to a 144-block square. `add` is 187x189 in X and Z,
# so most of it sat in unticked chunks - where commands still place blocks and
# probes still read them, but redstone never runs. The adder read a fixed wrong
# answer on every sampled input, consistently across both passes, with all
# sixteen levers verified correct.
#
# forceload caps at 256 chunks, so check rather than assume: a silent failure
# here looks exactly like a compiler bug.
FL_X2=$((BX + 16)); FL_Z2=$((BZ + 16))
FL_CHUNKS=$(( ((FL_X2 + 48) / 16 + 1) * ((FL_Z2 + 48) / 16 + 1) ))
echo "forceload -48 -48 $FL_X2 $FL_Z2 (~$FL_CHUNKS chunks)"
[ "$FL_CHUNKS" -le 256 ] || { echo "circuit needs $FL_CHUNKS chunks, over the 256 forceload cap"; exit 1; }
# A function stops after `maxCommandChainLength` commands and says nothing
# about it. The default is 65536, and a placed design passes that easily -
# `count.ohm` is 87226 setblock commands, so the last quarter of the build was
# silently never placed. Because the emitter writes dust last, what went missing
# was exactly the wiring, and the result looked like a dead circuit rather than
# an unfinished one.
send "gamerule maxCommandChainLength 10000000" 2
send "forceload add -48 -48 $FL_X2 $FL_Z2" 2
send "reload" 3
send "execute positioned 0.0 0.0 0.0 run function ohm:circuit" 3

# --- sweep every input combination, twice, in one world ---------------------
#
# One server, one world, every case. The old design booted a fresh server per
# case, which cost a server start each and - far worse - meant *every* case was
# a first transition after placement. Nothing in this suite ever asked a gate to
# respond to a second change.
#
# That mattered enormously. Servers ship `pause-when-empty-seconds=60`: with no
# player online the world stops ticking a minute in, while commands keep
# working, so a long single-world run looked exactly like circuits freezing.
# Three faster schemes were tried and abandoned over readings that were really
# this. With the pause disabled above, one world is both correct and quicker.
#
# The second pass is the point. It repeats every case in reverse order, so each
# one is reached from a different predecessor. A combinational circuit must give
# the same answer regardless of what it computed before; anything that passes
# ascending and fails descending is holding state it should not.
FULL=$((1 << NLEV))
if [ "$FULL" -le "$MAXCASES" ]; then
  CASE_LIST=$(seq 0 $((FULL-1)))
else
  # A stride, plus a small offset per step so successive cases differ widely.
  CASE_LIST=$(python3 -c "
full=$FULL; n=$MAXCASES
stride=full//n
print('\n'.join(str((i*stride+i)%full) for i in range(n)))")
fi
CASES=$(echo "$CASE_LIST" | grep -c .)
echo "sweeping $CASES of $FULL input combination(s), each way"

# Only touch levers whose value actually changes.
#
# `setblock` on a lever is a remove-then-place, so re-stating a lever it already
# agrees with still emits a brief unpower/repower glitch. Driving all N levers
# every case therefore fired 2N edges within a couple of ticks - and a redstone
# torch burns out after roughly eight toggles in 60 game ticks, then stays out
# until something updates it. On add2 that pinned the inverter for input bit 3:
# every lever read back correctly while the circuit answered as though bit 3
# were still high, for the whole second half of the run.
#
# CUR tracks what is on the levers now, so a case only writes the difference.
CUR=""
run_case() {
  local pass=$1 v=$2 b=0 next=""
  while read -r x y z; do
    [ -n "$x" ] || continue
    on=$(( (v >> b) & 1 ))
    st=false; [ "$on" = 1 ] && st=true
    # Was this bit already at that value? $CUR holds one flag per lever.
    local was
    was=$(echo "$CUR" | awk -v i=$((b+1)) '{print $i}')
    if [ "$was" != "$st" ]; then
      # Clear to air first, then place the lever in the wanted state.
      #
      # setblock from lever[powered=true] straight to lever[powered=false] is a
      # same-block-type replacement, so Minecraft skips onRemove and only
      # notifies the lever's direct neighbours. A real lever flip also updates
      # the neighbours of the block the lever is *attached to* - and a gate's
      # wire is often adjacent to that support rather than to the lever itself.
      # Without the air step those cells never hear the lever turn off and hold
      # a stale 15 forever.
      #
      # That is exactly what made add2 read sum+1 on the even inputs in game
      # while every one of its seventeen gate torches measured correct, and
      # while the simulator - which recomputes the whole field each tick and so
      # has no notion of a missed update - said the circuit was perfect.
      send "setblock $x $y $z minecraft:air" 0
      send "setblock $x $y $z minecraft:lever[face=floor,facing=north,powered=$st]" 0
    fi
    next="$next $st"
    b=$((b+1))
  done <<< "$LEVERS"
  CUR="$next"
  sleep "$SETTLE"
  send "say OHMC_CASE $pass $v" 1
  send "function ohm:probe" 2
}

for v in $CASE_LIST; do run_case 1 "$v"; done
for v in $(echo "$CASE_LIST" | tail -r); do run_case 2 "$v"; done

send "stop" 2
wait $SRV 2>/dev/null

# --- compare ---------------------------------------------------------------
echo
echo "=== observed in Minecraft vs. compiler truth table ==="
python3 - "$NLAMP" <<PYEOF
import re, sys
nlamp = int(sys.argv[1])
log = open("server.log", encoding="utf8", errors="replace").read()
expected = {}
for line in """$TRUTH""".splitlines():
    m = re.match(r"TRUTH (\d+) (.*)", line.strip())
    if m:
        expected[int(m.group(1))] = m.group(2).strip()

observed, levers, key = {}, {}, None
for line in log.splitlines():
    m = re.search(r"OHMC_CASE (\d+) (\d+)", line)
    if m:
        key = (int(m.group(1)), int(m.group(2))); observed[key] = {}
    m = re.search(r"OHMC_BIT (\d+) ([01])", line)
    if m and key is not None:
        observed[key][int(m.group(1))] = int(m.group(2))
    m = re.search(r"OHMC_LEV (\d+) (\S+)", line)
    if m and key is not None:
        levers.setdefault(key, {})[int(m.group(1))] = m.group(2)

def read(pass_no, v):
    bits = observed.get((pass_no, v), {})
    return sum(bits.get(i, 0) << i for i in range(nlamp))

sampled = {int(x) for x in """$CASE_LIST""".split()}
fails = hysteresis = 0
for v in sorted(k for k in expected if k in sampled):
    exp_val = int(re.search(r"=(\d+)", expected[v]).group(1))
    g1, g2 = read(1, v), read(2, v)
    # Did the harness actually drive the inputs it meant to?
    bad_drive = []
    for pass_no in (1, 2):
        seen = levers.get((pass_no, v), {})
        want_bits = {i: str((v >> i) & 1) for i in range(len(seen))}
        bad_drive += [(pass_no, i) for i, w in want_bits.items() if seen.get(i) != w]
    ok = (g1 == exp_val and g2 == exp_val)
    fails += not ok
    note = "ok" if ok else "MISMATCH"
    # Same inputs, different answer depending on what ran before: the circuit
    # is holding state a combinational design must not have.
    if g1 != g2:
        hysteresis += 1
        note += "  [HISTORY-DEPENDENT]"
    if bad_drive:
        note += f"  [HARNESS: lever(s) {bad_drive} read back wrong]"
    print(f"  in={v:<4} expected {expected[v]:<12} pass1 {g1:<6} pass2 {g2:<6} {note}")
print()
if hysteresis:
    print(f"{hysteresis} case(s) answered differently on the second pass")
print("FAIL: %d case(s) disagree" % fails if fails else
      "PASS: Minecraft agrees with the compiler on all %d sampled cases, both passes"
      % len(sampled))
sys.exit(1 if fails else 0)
PYEOF
