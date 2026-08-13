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

MAN=/tmp/dlatch.mcfunction.manifest
Q=$(awk '$1=="Q"{print $2, $3, $4}' "$MAN")
# bash 3.2 on macOS has no mapfile; newline-separated strings are enough.
DS=$(awk '$1=="D"{print $2, $3, $4}' "$MAN")
CS=$(awk '$1=="CLK"{print $2, $3, $4}' "$MAN")
echo "Q=$Q"; echo "D levers:"; echo "$DS"; echo "CLK levers:"; echo "$CS"

rm -rf world
PK=world/datapacks/ohm
mkdir -p "$PK/data/ohm/function"
cat > "$PK/pack.mcmeta" <<'EOF'
{"pack":{"description":"ohmc dff","pack_format":61,"supported_formats":{"min_inclusive":4,"max_inclusive":99}}}
EOF
cp /tmp/dlatch.mcfunction "$PK/data/ohm/function/circuit.mcfunction"
# Probe every node the exporter published, not just Q: the fault is somewhere
# along master -> inverted clock -> slave, and only the game can see inside.
: > "$PK/data/ohm/function/probe.mcfunction"
awk '$1!="D" && $1!="CLK"{
  printf "execute if block %s %s %s minecraft:redstone_lamp[lit=true] run say OHMC_%s ON\n", $2,$3,$4,$1
  printf "execute if block %s %s %s minecraft:redstone_lamp[lit=false] run say OHMC_%s off\n", $2,$3,$4,$1
  printf "execute unless block %s %s %s minecraft:redstone_lamp run say OHMC_%s MISSING\n", $2,$3,$4,$1
}' "$MAN" > "$PK/data/ohm/function/probe.mcfunction"

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
send "forceload add -48 -48 88 100" 2
send "reload" 3
send "execute positioned 0.0 0.0 0.0 run function ohm:circuit" 8

stage() { send "say OHMC_STAGE $1" 1; send "function ohm:probe" 2; }

setlev true 10 "$CS"              # enable high: latch transparent
setlev true 10 "$DS"              # D = 1
stage en1_d1
setlev false 10 "$DS"             # D = 0 while still transparent
stage en1_d0                      # Q must move the other way: this is reset
setlev false 10 "$CS"             # enable low
setlev true 10 "$DS"              # change D while opaque
stage en0_d1                      # Q must hold

# The case the flip-flop's master fails: D changed while the latch was shut,
# now open it. Every stage above changes D with the enable already high, which
# is a different path through the gates - so this was never tested in game.
setlev true 10 "$CS"              # reopen with D=1
stage reopen_d1                   # Q must take the 1
setlev false 10 "$CS"             # shut again
setlev false 10 "$DS"             # drop D while shut
stage shut_d0                     # Q must still hold the 1
setlev true 10 "$CS"              # reopen with D=0
stage reopen_d0                   # Q must take the 0

send "stop" 2
wait $SRV 2>/dev/null
echo "=== flip-flop in real Minecraft ==="
grep -oE "OHMC_(STAGE [a-z0-9_]+|[A-Z0-9]+ (ON|off|MISSING))" server.log | sed 's/OHMC_//'
