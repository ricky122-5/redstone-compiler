#!/bin/bash
# Probe the enable net cell by cell, by blockstate - no lamps in the wire, no
# trust in assumptions: levers are read back at every stage.
set -u
cd /tmp/ohm-mc
JAVA="$HOME/Library/Application Support/minecraft/runtime/java-runtime-delta/mac-os-arm64/java-runtime-delta/jre.bundle/Contents/Home/bin/java"
[ -x "$JAVA" ] || JAVA=java
MAN=/tmp/dlatch.mcfunction.manifest
DS=$(awk '$1=="D"{print $2, $3, $4}' "$MAN")
CS=$(awk '$1=="CLK"{print $2, $3, $4}' "$MAN")

rm -rf world
PK=world/datapacks/ohm
mkdir -p "$PK/data/ohm/function"
cat > "$PK/pack.mcmeta" <<'EOF'
{"pack":{"description":"ohmc en trace","pack_format":61,"supported_formats":{"min_inclusive":4,"max_inclusive":99}}}
EOF
cp /tmp/dlatch.mcfunction "$PK/data/ohm/function/circuit.mcfunction"

# Wire cells: report the actual power level bucket. Repeaters: powered or not.
# Levers: read back. Q lamp: as before.
: > "$PK/data/ohm/function/probe.mcfunction"
awk '
$1 ~ /^EW/ {
  printf "execute if block %s %s %s minecraft:redstone_wire[power=0] run say OHMC_%s 0\n", $2,$3,$4,$1
  printf "execute if block %s %s %s minecraft:redstone_wire run execute unless block %s %s %s minecraft:redstone_wire[power=0] run say OHMC_%s P\n", $2,$3,$4,$2,$3,$4,$1
  printf "execute unless block %s %s %s minecraft:redstone_wire run say OHMC_%s MISSING\n", $2,$3,$4,$1
}
$1 ~ /^ER/ {
  printf "execute if block %s %s %s minecraft:repeater[powered=true] run say OHMC_%s P\n", $2,$3,$4,$1
  printf "execute if block %s %s %s minecraft:repeater[powered=false] run say OHMC_%s 0\n", $2,$3,$4,$1
  printf "execute unless block %s %s %s minecraft:repeater run say OHMC_%s MISSING\n", $2,$3,$4,$1
}
$1=="Q" {
  printf "execute if block %s %s %s minecraft:redstone_lamp[lit=true] run say OHMC_Q ON\n", $2,$3,$4
  printf "execute if block %s %s %s minecraft:redstone_lamp[lit=false] run say OHMC_Q off\n", $2,$3,$4
}
$1=="D"||$1=="CLK" {
  printf "execute if block %s %s %s minecraft:lever[powered=true] run say OHMC_LEV_%s P\n", $2,$3,$4,$1
  printf "execute if block %s %s %s minecraft:lever[powered=false] run say OHMC_LEV_%s 0\n", $2,$3,$4,$1
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
setlev() {
  local val=$1 wait=$2 list=$3
  while read -r p; do
    [ -n "$p" ] || continue
    send "setblock $p minecraft:lever[face=floor,facing=north,powered=$val]" 0
  done <<< "$list"
  sleep "$wait"
}
stage() { send "say OHMC_STAGE $1" 1; send "function ohm:probe" 2; }

send "forceload add -48 -48 88 100" 2
send "reload" 3
send "execute positioned 0.0 0.0 0.0 run function ohm:circuit" 8

setlev true 8 "$DS"               # D high throughout; only the enable moves
setlev true 8 "$CS";  stage up1
setlev false 8 "$CS"; stage down1
setlev true 8 "$CS";  stage up2   # the transition that dies

# Poke: force a block update beside the pad hub and each repeater by placing
# and removing stone above the wire. If the net revives, the game is sitting on
# stale state that no update reached; if not, conduction is broken.
for c in $(awk '$1 ~ /^ER/ {print $2"_"$3"_"$4}' "$MAN"); do
  x=${c%%_*}; rest=${c#*_}; y=${rest%%_*}; z=${rest##*_}
  send "setblock $x $((y+1)) $z minecraft:stone" 1
  send "setblock $x $((y+1)) $z minecraft:air" 1
done
stage poked

send "stop" 2
wait $SRV 2>/dev/null
echo "=== enable net, cell by cell ==="
grep -oE "OHMC_(STAGE [a-z0-9_]+|[A-Z0-9_]+ (P|0|ON|off|MISSING))" server.log | sed 's/OHMC_//'
