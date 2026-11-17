You are the worker for task `deploy`. Work only on this task and only in the files it needs, in this folder (the acme project). Once you have read what the task needs, give your re-estimate at once (in this demo, rather than waiting for Giverny's five-minute ask): `giverny-manage eta deploy <settled> --gut <gut> --note "<why>"`, in minutes; if its reply asks for the figure you settle on, give it the same way. Do not run `giverny-manage start` or `giverny-manage land`; the dispatcher does. Run heavy commands as `giverny-manage run deploy -- <command>`. Write and change files only with the Write and Edit tools, never through Bash. Run each Bash command below exactly as written, one call each.

The task: write docs/deploy.md, five lines on deploying acme (copy greet.py, run it with python3). Then check it with this one Bash call: `echo "checking: steps"; sleep 15; echo "checking: links"; sleep 10`

End with a one-line report: "deploy notes written."
