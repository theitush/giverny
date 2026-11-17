#!/bin/bash
# hashes.sh save|check: ~/.claude/settings.json and plugins/known_marketplaces.json (lastUpdated left out) must not move.
R=/home/ita/giverny/.claude/review/534/rig
now=$(sha256sum ~/.claude/settings.json | cut -c1-16)-$(python3 -c '
import json,hashlib,os
d=json.load(open(os.path.expanduser("~/.claude/plugins/known_marketplaces.json")))
for v in d.values():
    if isinstance(v,dict): v.pop("lastUpdated",None)
print(hashlib.sha256(json.dumps(d,sort_keys=True).encode()).hexdigest()[:12])')
case $1 in
  save) [ -f $R/hashes.txt ] && [ "$now" != "$(cat $R/hashes.txt)" ] && { echo "HASH MOVED since last save: was $(cat $R/hashes.txt) now $now"; exit 1; }
        echo "$now" > $R/hashes.txt; echo "saved $now";;
  check) if [ "$now" = "$(cat $R/hashes.txt)" ]; then echo "hashes ok"; else echo "HASH MOVED: was $(cat $R/hashes.txt) now $now"; exit 1; fi;;
esac
