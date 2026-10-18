# Skills and message recall

## Runtime skills

Ask Sense to create or update a skill and specify its scope:

- Project: `<workspace>/.agents/skills/<name>/SKILL.md`
- Global: `~/.agents/skills/<name>/SKILL.md`, available across projects

Skills are ordinary files. Adding or editing one does not require rebuilding
Sense or fx. The agent discovers them with `capability_search` and reads them
with `skill`, including skills added during the current conversation. New chats
include instructions for authoring and rediscovering skills. Chats created
before those instructions were added keep their saved system prompt; explicitly
ask the agent to search for and load a newly created skill in those chats.

A minimal skill looks like this:

```markdown
---
name: review-changes
description: Review a local diff for correctness and missing tests.
---

Read the diff, check the affected callers, and run focused tests.
Report findings with file paths and concrete reproduction steps.
```

Use a lowercase kebab-case name matching the directory. Supporting files belong
beside `SKILL.md` and can be referenced by relative path. Skill writes use the
normal file tools and permission checks. When scope is omitted, the agent is
instructed to use project scope. Existing fx-compatible skill directories remain
supported; files do not need to be moved into these paths.

## Message recall

With the chat composer focused:

- Up recalls previous user inputs in the current conversation, newest first.
- Down moves forward and restores the unsent draft after the newest input.
- Repeated Up stays at the oldest loaded input rather than wrapping around.
- Typing into a recalled input starts a fresh browse on the next Up.
- Up inside a multiline draft moves the caret until it reaches the first line.
  While browsing history, arrows cycle through whole inputs, including multiline
  messages. Shift and other modified arrows keep their normal editing behavior.
- Option+Up still takes a queued message back for editing, with its attachments.

Recall changes text only. It does not send anything or reattach past images;
images already attached to the current draft remain attached. Loaded messages
from a reopened conversation are available, but recall does not fetch older
transcript pages or search other chats.

## Focused checks

```sh
cargo test -p sense-agent
cargo test -p sense input_history
cargo build -p sense --features automation
python3 scripts/e2e.py input-history queue
(cd vendor/fx && ../../.toolchain/zig/zig build)
python3 scripts/test-runtime-skills.py
```

The skills smoke test uses the checkout's built fx binary and a local fake model.
It creates project and global skills under an isolated temporary home, loads
both, edits and reloads a skill without restarting, and verifies that another
project sees the global skill but not the first project's skill.
