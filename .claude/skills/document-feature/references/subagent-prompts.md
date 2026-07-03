# Subagent prompts

Exact prompts for the two verification lenses. Fill the `{{...}}` slots. Dispatch
all of them in one turn so they run in parallel.

## Lens A — blind user-simulation

Dispatch one per intended task. The subagent must be genuinely blind: hand it the
help text inline and forbid lookups. Its rationale is how you detect cheating — if
it cites anything not in the help you gave it, rerun clean.

```
You are an automation agent about to run a `rezolus` command. You have NEVER seen
this tool's source or docs beyond the help text below. Do not search the web, read
any files, or use prior knowledge of rezolus — rely ONLY on this help text.

--- BEGIN `rezolus {{subcommand}} --help` ---
{{rendered_help_text}}
--- END HELP ---

Task: {{plain_english_task}}

Return exactly:
COMMAND: <the single command line you would run>
WHY: <one sentence, citing which parts of the help text led you there>

If the help text does not let you determine the command with confidence, say so:
COMMAND: UNSURE
WHY: <what specifically is missing or ambiguous>
```

Grading (you do this, not the subagent): compare `COMMAND` to ground truth
semantically — correct subcommand, all required args present, correct
flags/values; ignore arg order and equivalent forms. `UNSURE`, a wrong flag, a
missing required arg, or a plausible-but-wrong reading all count as fails and each
is a specific finding to fix in step 4.

## Lens B — fresh-eyes critic

One subagent, same rendered help. It judges clarity, not correctness of any one
task.

```
You are an LLM agent that has never used `rezolus`. Below is the entire help text
you would have for this command. Judge whether it is enough for an agent to invoke
the command correctly on the first try. Rely ONLY on this text.

--- BEGIN `rezolus {{subcommand}} --help` ---
{{rendered_help_text}}
--- END HELP ---

Report findings in these categories (omit a category if it has no findings):
- AMBIGUOUS: flags/args whose meaning or value format is unclear
- MISSING_EXAMPLE: places where a concrete example invocation is needed but absent
- JARGON: terms used without definition an outside agent wouldn't know
- WHEN_TO_USE: unclear when to use this command/flag vs an alternative

For each finding: quote the exact text, say why it's a problem for an agent, and
suggest the minimal fix. If the help is genuinely sufficient, say "NO MATERIAL
FINDINGS" and stop.
```

Treat AMBIGUOUS / MISSING_EXAMPLE / JARGON as material; WHEN_TO_USE is material
only when two commands or flags genuinely compete. Non-material nits don't need a
revise round.
