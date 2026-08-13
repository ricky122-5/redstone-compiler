#!/bin/bash
# Full register dump per stage: every torch and repeater by blockstate, levers
# read back, no lamps in the circuit except Q's. The failing sequence exactly.
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
{"pack":{"description":"ohmc state trace","pack_format":61,"supported_formats":{"min_inclusive":4,"max_inclusive":99}}}
EOF
cp /tmp/dlatch.mcfunction "$PK/data/ohm/function/circuit.mcfunction"

: > "$PK/data/ohm/function/probe.mcfunction"
awk '
$1 ~ /^T[0-9]+$/ {
  printf "execute if block %s %s %s minecraft:redstone_wall_torch[lit=true] run say OHMC_%s LIT\n", $2,$3,$4,$1
  printf "execute if block %s %s %s minecraft:redstone_wall_torch[lit=false] run say OHMC_%s dark\n", $2,$3,$4,$1
  printf "execute unless block %s %s %s minecraft:redstone_wall_torch run say OHMC_%s MISSING\n", $2,$3,$4,$1
}
$1 ~ /^R[0-9]+$/ {
  printf "execute if block %s %s %s minecraft:repeater[powered=true] run say OHMC_%s P\n", $2,$3,$4,$1
  printf "execute if block %s %s %s minecraft:repeater[powered=false] run say OHMC_%s 0\n", $2,$3,$4,$1
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
stage placed

setlev true 10 "$CS"
setlev true 10 "$DS";  stage en1_d1     # loop flip 1: set
setlev false 10 "$DS"; stage en1_d0     # loop flip 2: reset, while open
setlev false 10 "$CS"
setlev true 10 "$DS";  stage en0_d1     # hold; D rises while shut
setlev true 10 "$CS";  stage reopen_d1  # loop flip 3: the one that dies
send "stop" 2
wait $SRV 2>/dev/null
echo "=== latch state per stage ==="
grep -oE "OHMC_(STAGE [a-z0-9_]+|[A-Z0-9_]+ (P|0|LIT|dark|ON|off|MISSING))" server.log | sed 's/OHMC_//'
