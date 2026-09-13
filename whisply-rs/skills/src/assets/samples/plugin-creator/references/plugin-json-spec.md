# Whisply plugin.json

A plugin draft is a folder with `plugin.json` at the root. The file matches the signed-plugin manifest Whisply reviews before install. This skill writes an unsigned draft only.

Required fields:

- `manifestVersion`: `1`
- `slug`: lowercase hyphenated identifier
- `name`: visible name
- `publisherId`: lowercase underscore identifier
- `version`: semver
- `updateChannel`: `stable` or `beta`
- `description` and `releaseNotes`
- `capabilities`: at least one reviewed capability
- `sandbox`: filesystem, process, and general network access stay `false`; `typedBrokerOnly` stays `true`

Do not add `.codex-plugin`, marketplace.json, or Codex install policies.
