---
name: plugin-creator
description: Scaffold a Whisply plugin draft under $WHISPLY_HOME/plugins with a reviewable plugin.json and optional bundled skills. Use when the person wants a new Whisply plugin, a plugin manifest, or help packaging skills as a plugin.
metadata:
  short-description: Scaffold a Whisply plugin draft
---

# Whisply Plugin Creator

Create a plugin draft the person can review in Whisply. This does not sign or publish the plugin. It writes a `plugin.json` that matches Whisply's signed-plugin manifest shape, plus optional skill folders.

`$WHISPLY_HOME` is set by Whisply for the signed-in account. Default destination is `$WHISPLY_HOME/plugins/<plugin-slug>`. Do not write Codex marketplace files, `.codex-plugin`, or `~/.agents/plugins`.

## Create

From this skill folder:

```bash
python3 scripts/create_basic_plugin.py <plugin-name>
python3 scripts/create_basic_plugin.py <plugin-name> --with-skills
```

Then:

```bash
python3 scripts/validate_plugin.py <plugin-path>
```

The manifest is an unsigned draft. Tell the person to open Settings → Plugins, review the declared capabilities, and install only after that review. Do not claim the plugin is live until they install it in Whisply.

## Manifest

`plugin.json` at the plugin root must include:

- `manifestVersion`: 1
- `slug` and `name`
- `publisherId`
- `version` (semver)
- at least one capability
- locked sandbox (`generalFilesystemAccess`, `processExecution`, and `generalNetworkAccess` false; `typedBrokerOnly` true)
- support, storage, retention, usage, and compatibility blocks

Do not grant general filesystem, process, or network access in the draft.

## Bundled skills

`--with-skills` creates `skills/`. Add each bundled skill with `$skill-creator` so those packages are valid Whisply skills. List their slugs in `includedBuiltInSkillSlugs` only when they exist.
