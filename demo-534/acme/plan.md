# acme: five tasks

| task | title | gut (min) | waits on | brief |
|---|---|---|---|---|
| docs | FEATURE: write docs/usage.md | 4 | nothing | .claude/briefs/docs.md |
| bench | FEATURE: benchmark greet() | 1 | nothing | .claude/briefs/bench.md |
| server | FEATURE: start the preview server | 3 | nothing | .claude/briefs/server.md |
| examples | FEATURE: add examples to docs/usage.md | 3 | after docs (same file) | .claude/briefs/examples.md |
| deploy | FEATURE: write the deploy notes | 2 | hold: "waits on the docs review" | .claude/briefs/deploy.md |

Every worker needs 1G: `claim <task> --ram 1G`.
docs lands for review: `land docs --review "you — read the usage guide — docs/usage.md"`. The others land Done.
