//! Skills stay on disk; fx's discovery and skill tools read them at runtime.

pub(crate) const GUIDANCE: &str = "# Runtime skills

You can create, edit, discover, and load skills without rebuilding Sense or fx.
Skills are directories containing SKILL.md and optional supporting files.

- Project scope: write <workspace_root>/.agents/skills/<name>/SKILL.md, using the session's workspace, not the process launch directory.
- Global scope: write ~/.agents/skills/<name>/SKILL.md, shared across projects. Resolve the user's home directory at runtime; never write a literal ~ directory.
- Use the scope the user requests. If scope is unspecified, use project scope. Check for an existing skill before creating one and preserve unrelated files.
- SKILL.md starts with YAML frontmatter containing name and description, followed by the instructions. Use a lowercase kebab-case name matching the directory, and explain when the skill applies in its description. Store supporting resources beside SKILL.md and reference them with relative paths.
- Create or edit skills with the normal file tools, subject to the same permission rules as other file writes. Do not change source code, rebuild the app, or install a package to author a skill.
- Before substantive work, use capability_search to look for applicable skills, including when the user names a skill. This scans the current files, so skills added or edited during this conversation can be discovered without restarting.
- After creating or editing a skill, use capability_search with its exact name to rediscover it, then load the exact returned location with the skill tool to verify its contents. Load applicable skills completely, including their required resources, before following them. Never treat a description as the full instructions.
- If matching names exist in more than one scope, use the exact advertised location for the requested scope; do not guess a location or silently load the other copy.
";
