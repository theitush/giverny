#!/bin/bash
# setup.sh: start every tab's work, off camera. Tabs are counted from 0.
R=/home/ita/giverny/.claude/review/530/rig; D=$R/drive.sh
$D 'select 0'; sleep 2.5; $D 'type clear; claude --session-id aa48defd-c821-4e3f-8ba7-56b5b8586aa2 --append-system-prompt-file /home/ita/giverny/.claude/review/530/rig/mgr-system.md\r'
for t in "1|python3 tests.py" \
         "2|echo \"checking 214 files\"; sleep 30; echo \"error: unused import \\\`std::fmt\\\` --> src/view.rs:12\"; false" \
         "3|echo \"Compiling acme v0.1.0\"; sleep 32; echo \"Finished release [optimized] target(s) in 32.0s\"" \
         "4|python3 -m http.server 8765 --bind 127.0.0.1" \
         "5|clear; claude --session-id 09b1f9d8-8f07-4c6e-83f0-3d8989ee0cfe \"Reply with just the word: ready\"" \
         "6|python3 db.py" "7|python3 queue.py"; do
  $D "select ${t%%|*}"; sleep 2.5; $D "type ${t#*|}\r"; sleep 0.5
done
$D 'select 0'
