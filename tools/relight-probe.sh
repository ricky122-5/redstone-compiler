#!/bin/bash
# Does a single NOR cell relight after its input goes high and then low?
#
# This is the smallest circuit that could show the freeze seen in the flip-flop:
# a gate that follows its first input change and then stops. It is deliberately
# one cell, because bisecting a 1500-block flip-flop by ten-minute server runs is
# the slowest possible way to find a one-cell fault.
#
# mc-validate.sh cannot see this: it creates a fresh world for every input
# combination, so no gate there is ever asked to respond to a *second* change.
set -u
cd /tmp/ohm-mc
JAVA="$HOME/Library/Application Support/minecraft/runtime/java-runtime-delta/mac-os-arm64/java-runtime-delta/jre.bundle/Contents/Home/bin/java"
[ -x "$JAVA" ] || JAVA=java

MAN=/tmp/relight.mcfunction.manifest
LEV=$(awk '$1=="IN"{print $2, $3, $4}' "$MAN")
OUT=$(awk '$1=="OUT"{print $2, $3, $4}' "$MAN")
echo "lever=$LEV  lamp=$OUT"

rm -rf world
PK=world/datapacks/ohm
mkdir -p "$PK/data/ohm/function"
cat > "$PK/pack.mcmeta" <<'EOF'
{"pack":{"description":"ohmc relight","pack_format":61,"supported_formats":{"min_inclusive":4,"max_inclusive":99}}}
EOF
cp /tmp/relight.mcfunction "$PK/data/ohm/function/circuit.mcfunction"
cat > "$PK/data/ohm/function/probe.mcfunction" <<EOF
execute if block $OUT minecraft:redstone_lamp[lit=true] run say OHMC_OUT ON
execute if block $OUT minecraft:redstone_lamp[lit=false] run say OHMC_OUT off
execute unless block $OUT minecraft:redstone_lamp run say OHMC_OUT MISSING
execute if block $LEV minecraft:lever[powered=true] run say OHMC_IN ON
execute if block $LEV minecraft:lever[powered=false] run say OHMC_IN off
EOF

rm -f server.log cmd; mkfifo cmd; exec 3<>cmd
"$JAVA" -Xmx2G -jar server.jar nogui < cmd > server.log 2>&1 &
SRV=$!
for _ in $(seq 1 150); do
  grep -q 'Done (' server.log && break
  kill -0 $SRV 2>/dev/null || { echo "server died"; tail -15 server.log; exit 1; }
  sleep 2
done

send() { echo "$1" >&3; sleep "${2:-0}"; }
send "forceload add -16 -16 48 48" 2
send "reload" 3
send "execute positioned 0.0 0.0 0.0 run function ohm:circuit" 5
stage() { send "say OHMC_STAGE $1" 1; send "function ohm:probe" 2; }

stage placed
# Toggle several times in ONE world. A gate that follows the first change and
# then freezes shows up on the second.
for i in 1 2 3; do
  send "setblock $LEV minecraft:lever[face=floor,facing=north,powered=true]" 4
  stage "high_$i"
  send "setblock $LEV minecraft:lever[face=floor,facing=north,powered=false]" 4
  stage "low_$i"
done

send "stop" 2
wait $SRV 2>/dev/null
echo "=== single NOR cell, repeated toggles in one world ==="
grep -oE "OHMC_(STAGE [a-z0-9_]+|OUT (ON|off|MISSING)|IN (ON|off))" server.log | sed 's/OHMC_//'
