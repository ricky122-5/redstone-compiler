#!/bin/bash
# Drive one D latch through a sequence in a SINGLE world.
#
# The middle rung of the bisection. A single NOR cell toggles cleanly three
# times (tools/relight-probe.sh); the flip-flop freezes after one change. The
# D latch sits between them, so it decides whether the fault is in the latch
# macro or in composing two of them.
#
# Sequences must run in one world: mc-validate.sh restarts per case, which is
# why a freeze on the second edge was invisible to it.
set -u
cd /tmp/ohm-mc
JAVA="$HOME/Library/Application Support/minecraft/runtime/java-runtime-delta/mac-os-arm64/java-runtime-delta/jre.bundle/Contents/Home/bin/java"
[ -x "$JAVA" ] || JAVA=java

MAN=/tmp/dlatch.mcfunction.manifest
DS=$(awk '$1=="D"{print $2,$3,$4}' "$MAN")
ES=$(awk '$1=="CLK"{print $2,$3,$4}' "$MAN")
Q=$(awk '$1=="Q"{print $2,$3,$4}' "$MAN")

rm -rf world
PK=world/datapacks/ohm
mkdir -p "$PK/data/ohm/function"
cat > "$PK/pack.mcmeta" <<'EOF'
{"pack":{"description":"ohmc dlatch seq","pack_format":61,"supported_formats":{"min_inclusive":4,"max_inclusive":99}}}
EOF
cp /tmp/dlatch.mcfunction "$PK/data/ohm/function/circuit.mcfunction"
: > "$PK/data/ohm/function/probe.mcfunction"
awk '$1!="D" && $1!="CLK"{
  printf "execute if block %s %s %s minecraft:redstone_lamp[lit=true] run say OHMC_%s ON\n",$2,$3,$4,$1
  printf "execute if block %s %s %s minecraft:redstone_lamp[lit=false] run say OHMC_%s off\n",$2,$3,$4,$1
}' "$MAN" > "$PK/data/ohm/function/probe.mcfunction"
awk '$1=="D"||$1=="CLK"{
  printf "execute if block %s %s %s minecraft:lever[powered=true] run say OHMC_LEV_%s_%d ON\n",$2,$3,$4,$1,++n[$1]
  printf "execute if block %s %s %s minecraft:lever[powered=false] run say OHMC_LEV_%s_%d off\n",$2,$3,$4,$1,n[$1]
}' "$MAN" >> "$PK/data/ohm/function/probe.mcfunction"

rm -f server.log cmd; mkfifo cmd; exec 3<>cmd
"$JAVA" -Xmx2G -jar server.jar nogui < cmd > server.log 2>&1 &
SRV=$!
for _ in $(seq 1 150); do
  grep -q 'Done (' server.log && break
  kill -0 $SRV 2>/dev/null || { echo "server died"; tail -15 server.log; exit 1; }
  sleep 2
done

send() { echo "$1" >&3; sleep "${2:-0}"; }
setlev() { local v=$1 w=$2 list=$3
  while read -r p; do [ -n "$p" ] || continue
    send "setblock $p minecraft:lever[face=floor,facing=north,powered=$v]" 0
  done <<< "$list"; sleep "$w"; }
send "forceload add -32 -32 96 96" 2
send "reload" 3
send "execute positioned 0.0 0.0 0.0 run function ohm:circuit" 6
stage() { send "say OHMC_STAGE $1" 1; send "function ohm:probe" 2; }

# Q must follow D while enabled, twice in each direction. The second pass is
# the one that matters: the flip-flop's gates follow the first change and then
# stop.
setlev false 3 "$ES"; setlev false 3 "$DS"; stage init
setlev true 3 "$DS"; setlev true 5 "$ES"; stage open_d1_a
setlev false 5 "$DS";                     stage open_d0_a
setlev true 5 "$DS";                      stage open_d1_b
setlev false 5 "$DS";                     stage open_d0_b
setlev false 5 "$ES";                     stage closed
setlev true 5 "$DS";                      stage hold_d1

send "stop" 2
wait $SRV 2>/dev/null
echo "=== D latch, sequence in one world ==="
grep -oE "OHMC_(STAGE [a-z0-9_]+|[A-Z0-9_]+ (ON|off))" server.log | sed 's/OHMC_//'
