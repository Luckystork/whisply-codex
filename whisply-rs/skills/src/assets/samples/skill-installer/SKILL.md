---
name: skill-installer
description: Install a Whisply skill from a GitHub repo path into $WHISPLY_HOME/skills. Use when the person asks to list skills in a repo, install a skill from GitHub, or add a skill they already have a URL for.
metadata:
  short-description: Install a skill from GitHub into Whisply
---

# Whisply Skill Installer

Install skill folders into `$WHISPLY_HOME/skills`. `$WHISPLY_HOME` is set by Whisply for the signed-in account. Do not write to `~/.codex` or `~/.agents`.

Do not offer an OpenAI curated catalog. List or install only the repo and path the person named.

## Scripts

These scripts use the network. Request escalation in the sandbox before running them.

- `scripts/list-skills.py --repo <owner>/<repo> --path <path>`
- `scripts/list-skills.py --repo <owner>/<repo> --path <path> --format json`
- `scripts/install-skill-from-github.py --repo <owner>/<repo> --path <path/to/skill>`
- `scripts/install-skill-from-github.py --url https://github.com/<owner>/<repo>/tree/<ref>/<path>`

## Behavior

- Destination is `$WHISPLY_HOME/skills/<skill-name>` unless `--dest` is set.
- Abort if that folder already exists.
- Public GitHub repos download directly. Private repos use existing git credentials or `GITHUB_TOKEN` / `GH_TOKEN`.
- After install, tell the person the skill is present and left disabled. They review and enable it in Settings → Skills.

When listing, mark skills that already exist under `$WHISPLY_HOME/skills` as already installed.
