---
name: skill-creator
description: Create or update a Whisply skill package in $WHISPLY_HOME/skills. Use when the person wants a new reusable skill, wants to edit an existing local skill, or asks you to write SKILL.md for Whisply.
metadata:
  short-description: Create or update a Whisply skill
---

# Whisply Skill Creator

Create a skill the Whisply runtime can discover. A skill is a folder with `SKILL.md`. Whisply loads it from `$WHISPLY_HOME/skills` on the next turn after it is enabled in Settings.

`$WHISPLY_HOME` is set by Whisply for the signed-in account. Do not fall back to `~/.codex` or invent another home.

## Package

```
skill-name/
├── SKILL.md
├── agents/openai.yaml   # list/chip metadata the runtime already reads
├── scripts/             # optional
├── references/          # optional
└── assets/              # optional
```

`SKILL.md` frontmatter needs only `name` and `description`. The description is the trigger: say what the skill does and when to use it.

Write the body as instructions for a later Whisply turn. Keep it under 500 lines. Put long reference material in `references/` and point to it from `SKILL.md`.

## Create

Ask where the skill should live only if the person named a project folder. Otherwise use `$WHISPLY_HOME/skills`.

From this skill folder:

```bash
python3 scripts/init_skill.py <skill-name>
python3 scripts/init_skill.py <skill-name> --path "$WHISPLY_HOME/skills" --resources scripts,references
```

`init_skill.py` writes `SKILL.md` and `agents/openai.yaml`. Fill the TODOs. Pass `--interface key=value` for `display_name`, `short_description`, and `default_prompt`.

```bash
python3 scripts/generate_openai_yaml.py <path/to/skill-folder> --interface display_name="Name" --interface short_description="Short UI label" --interface default_prompt="Use $skill-name for this request."
```

The filename `agents/openai.yaml` is the runtime metadata file. The values inside are Whisply UI copy. Do not mention Codex or OpenAI in that copy.

## Validate

```bash
python3 scripts/quick_validate.py <path/to/skill-folder>
```

The skill is created disabled. Tell the person to review it in Settings → Skills, then enable it. It is available on the next turn after they enable it.

## Naming

Lowercase letters, digits, and hyphens. Folder name matches `name`. Keep it under 64 characters. Prefer a short verb-led phrase.
