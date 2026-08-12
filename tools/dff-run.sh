#!/bin/bash
# Drive the flip-flop in a real Minecraft server and read Q off its lamp.
#
# Port coordinates come from the manifest the exporter writes, not from anything
# hardcoded here. Stale coordinates have already produced two false readings in
# this project, both of which looked like circuit faults.
set -u
cd /tmp/ohm-mc
JAVA="$HOME/Library/Application Support/minecraft/runtime/java-runtime-delta/mac-os-arm64/java-runtime-delta/jre.bundle/Contents/Home/bin/java"
[ -x "$JAVA" ] || JAVA=java

MAN=/tmp/dff.mcfunction.manifest
Q=$(awk '$1=="Q"{print $2, $3, $4}' "$MAN")
# bash 3.2 on macOS has no mapfile; newline-separated strings are enough.
DS=$(awk '$1=="D"{print $2, $3, $4}' "$MAN")
CS=$(awk '$1=="CLK"{print $2, $3, $4}' "$MAN")
RS=$(awk '$1=="RST"{print $2, $3, $4}' "$MAN")
NS=$(awk '$1=="CLKN"{print $2, $3, $4}' "$MAN")
echo "Q=$Q"; echo "D levers:"; echo "$DS"; echo "CLK levers:"; echo "$CS"; echo "RST levers:"; echo "$RS"

rm -rf world
PK=world/datapacks/ohm
mkdir -p "$PK/data/ohm/function"
cat > "$PK/pack.mcmeta" <<'EOF'
{"pack":{"description":"ohmc dff","pack_format":61,"supported_formats":{"min_inclusive":4,"max_inclusive":99}}}
EOF
cp /tmp/dff.mcfunction "$PK/data/ohm/function/circuit.mcfunction"
# Probe every node the exporter published, not just Q: the fault is somewhere
# along master -> inverted clock -> slave, and only the game can see inside.
: > "$PK/data/ohm/function/probe.mcfunction"
awk '$1!="D" && $1!="CLK" && $1!="RST" && $1!="CLKN"{
  printf "execute if block %s %s %s minecraft:redstone_lamp[lit=true] run say OHMC_%s ON\n", $2,$3,$4,$1
  printf "execute if block %s %s %s minecraft:redstone_lamp[lit=false] run say OHMC_%s off\n", $2,$3,$4,$1
  printf "execute unless block %s %s %s minecraft:redstone_lamp run say OHMC_%s MISSING\n", $2,$3,$4,$1
}' "$MAN" > "$PK/data/ohm/function/probe.mcfunction"

# Read the input levers back with every probe. A harness that cannot show its
# inputs took has no business reporting circuit faults - five readings this
# session turned out to be the measurement rather than the circuit.
awk '$1=="D"||$1=="CLK"||$1=="RST"||$1=="CLKN"{
  printf "execute if block %s %s %s minecraft:lever[powered=true] run say OHMC_LEV_%s_%d ON\n", $2,$3,$4,$1,++n[$1]
  printf "execute if block %s %s %s minecraft:lever[powered=false] run say OHMC_LEV_%s_%d off\n", $2,$3,$4,$1,n[$1]
  printf "execute unless block %s %s %s minecraft:lever run say OHMC_LEV_%s_%d MISSING\n", $2,$3,$4,$1,n[$1]
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
setlev() { # value wait positions-as-one-string
  local val=$1 wait=$2 list=$3
  while read -r p; do
    [ -n "$p" ] || continue
    send "setblock $p minecraft:lever[face=floor,facing=north,powered=$val]" 0
  done <<< "$list"
  sleep "$wait"
}

# forceload must stay under 256 chunks or it fails and leaves the far end of the
# build in unticked chunks, where setblock quietly does nothing.
send "forceload add -48 -48 88 180" 2
send "reload" 3
send "execute positioned 0.0 0.0 0.0 run function ohm:circuit" 8
# Place a second time. Every setblock notifies its neighbours, so a re-place
# delivers a block update to every gate in the build. If gates are freezing
# because the update that would make them re-evaluate never arrived, this is
# both the test and the fix.
send "execute positioned 0.0 0.0 0.0 run function ohm:circuit" 8

stage() { send "say OHMC_STAGE $1" 1; send "function ohm:probe" 2; }

setlev false 2 "$RS"              # reset idle
setlev false 0 "$CS"              # clock low so the master is closed
setlev true 2 "$NS"               # ...and the slave phase high
setlev true 6 "$RS"               # pulse reset
setlev false 8 "$RS"
stage after_reset                 # Q must be off here, whatever placement left

setlev true 2 "$DS"               # D = 1
setlev false 0 "$CS"             # clock low: master open, slave shut
setlev true 10 "$NS"
stage d1_clk_low
setlev true 0 "$CS"              # clock high
setlev false 10 "$NS"
stage d1_clk_high
setlev false 0 "$CS"             # falling edge: the 1 should be captured
setlev true 10 "$NS"
stage d1_after_edge
setlev false 10 "$DS"             # drop D with the clock idle
stage d0_clk_idle                 # Q must NOT follow
setlev true 0 "$CS"
setlev false 10 "$NS"
stage d0_clk_high                 # inside the capture window: master must reset
setlev false 0 "$CS"             # clock the 0 through
setlev true 10 "$NS"
stage d0_after_edge

send "stop" 2
wait $SRV 2>/dev/null
echo "=== flip-flop in real Minecraft ==="
grep -oE "OHMC_(STAGE [a-z0-9_]+|[A-Z0-9_]+ (ON|off|MISSING))" server.log | sed 's/OHMC_//'
