#!/bin/bash
set -u
cd /tmp/ohm-mc
JAVA="$HOME/Library/Application Support/minecraft/runtime/java-runtime-delta/mac-os-arm64/java-runtime-delta/jre.bundle/Contents/Home/bin/java"
[ -x "$JAVA" ] || JAVA=java
MAN=/tmp/reporient.mcfunction.manifest
LS=$(awk '$1=="LEV"{print $2, $3, $4}' "$MAN")
rm -rf world
PK=world/datapacks/ohm
mkdir -p "$PK/data/ohm/function"
cat > "$PK/pack.mcmeta" <<'EOF'
{"pack":{"description":"rep","pack_format":61,"supported_formats":{"min_inclusive":4,"max_inclusive":99}}}
EOF
cp /tmp/reporient.mcfunction "$PK/data/ohm/function/circuit.mcfunction"
awk '$1!="LEV"{
  printf "execute if block %s %s %s minecraft:redstone_lamp[lit=true] run say OHMC_%s ON\n", $2,$3,$4,$1
  printf "execute if block %s %s %s minecraft:redstone_lamp[lit=false] run say OHMC_%s off\n", $2,$3,$4,$1
}' "$MAN" > "$PK/data/ohm/function/probe.mcfunction"
rm -f server.log cmd; mkfifo cmd; exec 3<>cmd
"$JAVA" -Xmx2G -jar server.jar nogui < cmd > server.log 2>&1 &
SRV=$!
for _ in $(seq 1 150); do grep -q 'Done (' server.log && break; sleep 2; done
send() { echo "$1" >&3; sleep "${2:-0}"; }
setlev() { while read -r p; do [ -n "$p" ] || continue; send "setblock $p minecraft:lever[face=floor,facing=north,powered=$1]" 0; done <<< "$LS"; sleep "$2"; }
send "forceload add -16 -16 32 32" 2
send "reload" 3
send "execute positioned 0.0 0.0 0.0 run function ohm:circuit" 6
stage() { send "say OHMC_STAGE $1" 1; send "function ohm:probe" 2; }
setlev true 8;  stage on1
setlev false 8; stage off1
setlev true 8;  stage on2
setlev false 8; stage off2
setlev true 8;  stage on3
send "stop" 2
wait $SRV 2>/dev/null
echo "=== repeater orientation ==="
grep -oE "OHMC_(STAGE [a-z0-9_]+|[A-Z_]+ (ON|off))" server.log | sed 's/OHMC_//'
