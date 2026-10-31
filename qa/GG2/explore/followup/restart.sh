#!/bin/bash
# usage: restart.sh <variant> <tag>  -- restart own fake server + daemon with a config variant
H=/tmp/claude-0/gg2-followup/h; P=/tmp/claude-0/gg2-followup
cd "$(dirname "$0")"
for f in d fake; do [ -f $P/$f.pid ] && kill $(cat $P/$f.pid) 2>/dev/null; done
while kill -0 $(cat $P/d.pid) 2>/dev/null; do sleep 0.5; done
python3 mkconfig.py $H $1 18431 > out/config_$2.txt
nohup python3 fake_llm.py 18431 out/fake_$2.jsonl > /dev/null 2>&1 & echo $! > $P/fake.pid
rm -f $H/data/ipc/nuggetd.sock
nohup /home/user/nugget/target/release/nuggetd --home $H > out/nuggetd_$2.log 2>&1 & echo $! > $P/d.pid
until [ -S $H/data/ipc/nuggetd.sock ]; do sleep 0.5; done
