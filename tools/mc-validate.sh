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

# Port coordinates, as reported by the compiler in the same frame as the
# generated commands.
LEVERS=$(echo "$INFO" | sed -n 's/.*lever at ~\([0-9-]*\) ~\([0-9-]*\) ~\([0-9-]*\).*/\1 \2 \3/p')
LAMPS=$(echo "$INFO"  | sed -n 's/.*lamp  at ~\([0-9-]*\) ~\([0-9-]*\) ~\([0-9-]*\).*/\1 \2 \3/p')
NLEV=$(echo "$LEVERS" | grep -c .)
[ "$NLEV" -gt 0 ] || { echo "no input levers reported"; exit 1; }
[ "$NLEV" -le 6 ] || { echo "$NLEV inputs is too many to sweep in-game"; exit 1; }

# --- server ----------------------------------------------------------------
cd "$D"
if [ ! -f server.jar ]; then curl -sSL -o server.jar "$JAR_URL"; fi
echo "$JAR_SHA  server.jar" | shasum -c - >/dev/null || { echo "server.jar SHA mismatch"; exit 1; }

JAVA="$HOME/Library/Application Support/minecraft/runtime/java-runtime-delta/mac-os-arm64/java-runtime-delta/jre.bundle/Contents/Home/bin/java"
[ -x "$JAVA" ] || JAVA=java

echo "eula=true" > eula.txt
cat > server.properties <<'EOF'
level-type=minecraft\:flat
gamemode=creative
online-mode=false
spawn-protection=0
max-tick-time=-1
spawn-npcs=false
spawn-animals=false
spawn-monsters=false
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
send "forceload add -48 -48 96 96" 2
send "reload" 3
send "execute positioned 0.0 0.0 0.0 run function ohm:circuit" 3

# --- sweep every input combination -----------------------------------------
CASES=$((1 << NLEV))
for v in $(seq 0 $((CASES-1))); do
  b=0
  while read -r x y z; do
    [ -n "$x" ] || continue
    on=$(( (v >> b) & 1 ))
    st=false; [ "$on" = 1 ] && st=true
    send "setblock $x $y $z minecraft:lever[face=floor,facing=north,powered=$st]" 0
    b=$((b+1))
  done <<< "$LEVERS"
  sleep 3            # let the circuit settle
  send "say OHMC_CASE $v" 1
  send "function ohm:probe" 2
done
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

observed, case = {}, None
for line in log.splitlines():
    m = re.search(r"OHMC_CASE (\d+)", line)
    if m:
        case = int(m.group(1)); observed[case] = {}
    m = re.search(r"OHMC_BIT (\d+) ([01])", line)
    if m and case is not None:
        observed[case][int(m.group(1))] = int(m.group(2))

fails = 0
for v in sorted(expected):
    bits = observed.get(v, {})
    got = sum(bits.get(i, 0) << i for i in range(nlamp))
    exp_val = int(re.search(r"=(\d+)", expected[v]).group(1))
    ok = (got == exp_val)
    fails += not ok
    print(f"  in={v:<4} expected {expected[v]:<12} observed {got:<6} {'ok' if ok else 'MISMATCH'}")
print()
print("FAIL: %d case(s) disagree" % fails if fails else "PASS: Minecraft agrees with the compiler on all %d cases" % len(expected))
sys.exit(1 if fails else 0)
PYEOF
