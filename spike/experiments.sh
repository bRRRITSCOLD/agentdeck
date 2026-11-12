#!/usr/bin/env bash
# PROTOTYPE (issue #2): the commands behind the spike's answers. Run pieces by hand; the
# daemon must be running (`cargo run -p spike-daemon &`). Needs a Vulkan driver for the
# headless PNGs (lavapipe works), no display server.
set -euo pipefail
G=./target/debug/spike-gui
OUT=${OUT:-/tmp/agentdeck-spike}
mkdir -p "$OUT" /tmp/spike-demo/src
api() { # api METHOD PATH [curl args]
  local d="$XDG_RUNTIME_DIR/agentdeck-spike/daemon.json"
  local port token
  port=$(python3 -c "import json;print(json.load(open('$d'))['port'])")
  token=$(python3 -c "import json;print(json.load(open('$d'))['token'])")
  local m=$1 p=$2; shift 2
  curl -s -X "$m" -H "authorization: Bearer $token" -H 'content-type: application/json' "http://127.0.0.1:$port$p" "$@"
}
shell='{"argv":["bash","--noprofile","--norc"],"cwd":"/tmp/spike-demo","cols":80,"rows":24,"env":{"PS1":"\\[\\e[1;32m\\]spike\\[\\e[0m\\]:\\[\\e[1;34m\\]\\w\\[\\e[0m\\]$ ","HOME":"/tmp/spike-demo"}}'

# Q1/Q2: colours, then vi on the alternate screen typed into live, then a reattach replay.
id=$(api POST /sessions -d "$shell" | python3 -c "import json,sys;print(json.load(sys.stdin)['id'])")
$G attach --session "$id" --out "$OUT/shell" --send 'ls --color=always -F\r' --send 'printf "\e[31mred \e[1;33mbold \e[44;37m bg \e[0m \e[38;5;208m256\e[0m \e[38;2;180;120;255mtruecolor\e[0m\n"\r'
$G attach --session "$id" --out "$OUT/vi-live" --send 'vi src/main.rs\r' --send 'jjA // typed over the WebSocket\e' --send ':set number\r'
$G attach --session "$id" --out "$OUT/vi-replay"          # replay only
diff "$OUT/vi-live.txt" "$OUT/vi-replay.txt" && echo "vi: replay identical"

# Q2: resize mid-session; replay with in-band resize frames vs at one fixed size.
id=$(api POST /sessions -d '{"argv":["bash","--noprofile","--norc"],"cols":80,"rows":24,"env":{"PS1":"$ "}}' | python3 -c "import json,sys;print(json.load(sys.stdin)['id'])")
$G attach --session "$id" --out "$OUT/r0" --send 'clear; printf "\e[5;60Hpositioned-at-col-60\e[8;1H"\r'
$G attach --session "$id" --out "$OUT/r-live" --resize 50x24
$G attach --session "$id" --out "$OUT/r-inband"
$G attach --session "$id" --out "$OUT/r-naive" --naive 50x24
diff "$OUT/r-live.txt" "$OUT/r-inband.txt" && echo "in-band: identical"
diff "$OUT/r-live.txt" "$OUT/r-naive.txt" || echo "naive: differs"

# Q2: top, a client attached throughout, a client attached mid-redraw, then capped replays.
# Freeze top with SIGSTOP so every client compares the same final screen.
id=$(api POST /sessions -d '{"argv":["top","-d","0.2","-u","root"],"cols":80,"rows":24}' | python3 -c "import json,sys;print(json.load(sys.stdin)['id'])")
rm -f "$OUT/flag"
$G attach --session "$id" --out "$OUT/top-A" --hold-until "$OUT/flag" --max-ms 60000 >/dev/null &
sleep 3
$G attach --session "$id" --out "$OUT/top-M" --hold-until "$OUT/flag" --max-ms 60000 >/dev/null &
sleep 17
pkill -STOP -f '^top -d 0.2 -u root'; sleep 0.5; touch "$OUT/flag"; wait
diff -q "$OUT/top-A.txt" "$OUT/top-M.txt" && echo "mid-redraw attach: identical"
for n in 1024 4096 16384 65536; do
  $G attach --session "$id" --out "$OUT/top-$n" --replay-limit "$n" --quiet-ms 300 >/dev/null
  echo "cap $n: $(diff "$OUT/top-A.txt" "$OUT/top-$n.txt" | grep -c '^<' || true) lines differ"
done
$G attach --session "$id" --out "$OUT/top-full" --quiet-ms 300 --snapshot-check >/dev/null
pkill -CONT -f '^top -d 0.2 -u root'; pkill -f '^top -d 0.2 -u root' || true

# Q3/Q4 (scope changed: no hooks): chosen session id, then --resume in a fresh PTY.
# Costs a few cents of Haiku: run deliberately.
# U=$(python3 -c "import uuid;print(uuid.uuid4())")
# claude --session-id $U --model haiku --setting-sources project -p 'reply with the word ok'
# claude --resume $U --model haiku --setting-sources project -p 'what word did you reply with?'
# (both spawned through POST /sessions with argv ["sh","-c","unset $(env | grep -oE '^CLAUDE[A-Z_]*'); exec claude ..."])
