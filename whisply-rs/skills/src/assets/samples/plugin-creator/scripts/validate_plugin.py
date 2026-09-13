#!/usr/bin/env python3
"""Validate a Whisply plugin draft against the product manifest shape."""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path
from typing import Any


SLUG_RE = re.compile(r"^[a-z][a-z0-9-]{1,62}[a-z0-9]$")
PUBLISHER_RE = re.compile(r"^[a-z][a-z0-9_]{1,63}$")
SEMVER_RE = re.compile(r"^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-[0-9A-Za-z.-]+)?$")
IDENTIFIER_RE = re.compile(r"^[a-z][a-z0-9_]{1,63}$")
ACTION_RE = re.compile(r"^[a-z][a-z0-9_.-]{2,95}$")
REQUIRED = (
    "manifestVersion",
    "slug",
    "name",
    "publisherId",
    "version",
    "updateChannel",
    "description",
    "releaseNotes",
    "capabilities",
    "includedBuiltInSkillSlugs",
    "dependencies",
    "storage",
    "retention",
    "usage",
    "support",
    "sandbox",
    "compatibility",
)


def error(message: str) -> None:
    print(f"[ERROR] {message}", file=sys.stderr)


def load_manifest(plugin_root: Path) -> dict[str, Any] | None:
    path = plugin_root / "plugin.json"
    if not path.is_file():
        error(f"Missing {path}. Whisply plugin drafts use plugin.json at the plugin root.")
        return None
    try:
        payload = json.loads(path.read_text())
    except json.JSONDecodeError as exc:
        error(f"plugin.json is not valid JSON: {exc}")
        return None
    if not isinstance(payload, dict):
        error("plugin.json must contain an object.")
        return None
    return payload


def validate(plugin_root: Path) -> int:
    if (plugin_root / ".codex-plugin").exists():
        error("This folder still has .codex-plugin. Use a Whisply plugin.json at the root.")
        return 1
    payload = load_manifest(plugin_root)
    if payload is None:
        return 1

    failed = False
    for key in REQUIRED:
        if key not in payload:
            error(f"Missing required field '{key}'.")
            failed = True
    if failed:
        return 1

    if payload.get("manifestVersion") != 1:
        error("manifestVersion must be 1.")
        failed = True
    if not isinstance(payload.get("slug"), str) or not SLUG_RE.fullmatch(payload["slug"]):
        error("slug must be a lowercase hyphenated identifier.")
        failed = True
    if not isinstance(payload.get("publisherId"), str) or not PUBLISHER_RE.fullmatch(
        payload["publisherId"]
    ):
        error("publisherId must match [a-z][a-z0-9_]{1,63}.")
        failed = True
    if not isinstance(payload.get("version"), str) or not SEMVER_RE.fullmatch(payload["version"]):
        error("version must be semver.")
        failed = True
    if payload.get("updateChannel") not in {"stable", "beta"}:
        error("updateChannel must be stable or beta.")
        failed = True

    capabilities = payload.get("capabilities")
    if not isinstance(capabilities, list) or not capabilities:
        error("capabilities must contain at least one capability.")
        failed = True
    else:
        for capability in capabilities:
            if not isinstance(capability, dict):
                error("Each capability must be an object.")
                failed = True
                continue
            if not isinstance(capability.get("id"), str) or not IDENTIFIER_RE.fullmatch(
                capability["id"]
            ):
                error("capability.id must be a lowercase identifier.")
                failed = True
            actions = capability.get("actionTypes")
            if not isinstance(actions, list) or not actions:
                error("capability.actionTypes must contain at least one action.")
                failed = True
            elif any(
                not isinstance(action, str) or not ACTION_RE.fullmatch(action)
                for action in actions
            ):
                error("capability.actionTypes contains an invalid action type.")
                failed = True

    sandbox = payload.get("sandbox")
    if not isinstance(sandbox, dict):
        error("sandbox must be an object.")
        failed = True
    else:
        if sandbox.get("generalFilesystemAccess") is not False:
            error("sandbox.generalFilesystemAccess must be false.")
            failed = True
        if sandbox.get("processExecution") is not False:
            error("sandbox.processExecution must be false.")
            failed = True
        if sandbox.get("generalNetworkAccess") is not False:
            error("sandbox.generalNetworkAccess must be false.")
            failed = True
        if sandbox.get("typedBrokerOnly") is not True:
            error("sandbox.typedBrokerOnly must be true.")
            failed = True

    support = payload.get("support")
    if not isinstance(support, dict) or not support.get("url"):
        error("support.url is required.")
        failed = True

    skills_root = plugin_root / "skills"
    if skills_root.is_dir():
        for child in sorted(skills_root.iterdir()):
            if child.is_dir() and not (child / "SKILL.md").is_file():
                error(f"Bundled skill {child.name} is missing SKILL.md.")
                failed = True

    if failed:
        return 1
    print(f"[OK] Whisply plugin draft is valid: {plugin_root}")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description="Validate a Whisply plugin draft.")
    parser.add_argument("plugin_path")
    args = parser.parse_args()
    return validate(Path(args.plugin_path).expanduser().resolve())


if __name__ == "__main__":
    raise SystemExit(main())
