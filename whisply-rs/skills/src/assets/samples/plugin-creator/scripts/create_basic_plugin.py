#!/usr/bin/env python3
"""Scaffold a Whisply plugin draft under $WHISPLY_HOME/plugins."""

from __future__ import annotations

import argparse
import json
import os
import re
from pathlib import Path
from typing import Any


MAX_PLUGIN_NAME_LENGTH = 64
SLUG_RE = re.compile(r"^[a-z][a-z0-9-]{1,62}[a-z0-9]$")
PUBLISHER_RE = re.compile(r"^[a-z][a-z0-9_]{1,63}$")


def whisply_home() -> Path:
    value = os.environ.get("WHISPLY_HOME", "").strip()
    if not value:
        raise SystemExit("WHISPLY_HOME is not set. Whisply sets this for the signed-in account.")
    return Path(value)


def normalize_plugin_name(plugin_name: str) -> str:
    normalized = plugin_name.strip().lower()
    normalized = re.sub(r"[^a-z0-9]+", "-", normalized)
    normalized = normalized.strip("-")
    return re.sub(r"-{2,}", "-", normalized)


def display_name_from_plugin_name(plugin_name: str) -> str:
    return " ".join(part.capitalize() for part in re.split(r"[-_]+", plugin_name))


def build_plugin_json(plugin_name: str, publisher_id: str) -> dict[str, Any]:
    display_name = display_name_from_plugin_name(plugin_name)
    return {
        "manifestVersion": 1,
        "slug": plugin_name,
        "name": display_name,
        "publisherId": publisher_id,
        "version": "0.1.0",
        "updateChannel": "stable",
        "description": f"{display_name} adds a reviewed Whisply plugin capability.",
        "releaseNotes": "Initial draft. Review capabilities in Whisply before install.",
        "capabilities": [
            {
                "id": "primary_action",
                "label": f"{display_name} action",
                "description": f"The primary reviewed action provided by {display_name}.",
                "actionTypes": ["plugin.assist"],
                "destinations": [],
                "dataClasses": [],
                "localApplications": [],
                "websites": [],
                "connectorTypes": [],
            }
        ],
        "includedBuiltInSkillSlugs": [],
        "dependencies": [],
        "storage": {
            "accountBytesMaximum": 0,
            "categories": [],
        },
        "retention": [],
        "usage": {
            "componentKinds": [],
            "maximumPerTaskUnits": 1,
        },
        "support": {
            "owner": "Local draft",
            "url": "https://whisply.app/support",
        },
        "sandbox": {
            "generalFilesystemAccess": False,
            "processExecution": False,
            "generalNetworkAccess": False,
            "typedBrokerOnly": True,
        },
        "compatibility": {
            "minimumWhisplyVersion": "10.1.0",
            "minimumMacOSVersion": "14.0",
        },
    }


def write_json(path: Path, data: dict[str, Any], force: bool) -> None:
    if path.exists() and not force:
        raise FileExistsError(f"{path} already exists. Use --force to overwrite.")
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(data, indent=2) + "\n")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Create a Whisply plugin draft.")
    parser.add_argument("plugin_name")
    parser.add_argument(
        "--path",
        help="Parent directory. Defaults to $WHISPLY_HOME/plugins.",
    )
    parser.add_argument(
        "--publisher-id",
        default="local_draft",
        help="Publisher identifier (lowercase_underscore).",
    )
    parser.add_argument("--with-skills", action="store_true", help="Create skills/")
    parser.add_argument("--force", action="store_true", help="Overwrite plugin.json")
    return parser.parse_args()


def main() -> None:
    args = parse_args()
    plugin_name = normalize_plugin_name(args.plugin_name)
    if plugin_name != args.plugin_name.strip():
        print(f"Note: Normalized plugin name from '{args.plugin_name}' to '{plugin_name}'.")
    if not SLUG_RE.fullmatch(plugin_name):
        raise SystemExit(
            f"Plugin name '{plugin_name}' must be a lowercase hyphenated slug of 3-64 characters."
        )
    if not PUBLISHER_RE.fullmatch(args.publisher_id):
        raise SystemExit("publisher-id must match [a-z][a-z0-9_]{1,63}.")

    parent = Path(args.path).expanduser() if args.path else whisply_home() / "plugins"
    plugin_root = parent.resolve() / plugin_name
    plugin_root.mkdir(parents=True, exist_ok=True)
    write_json(
        plugin_root / "plugin.json",
        build_plugin_json(plugin_name, args.publisher_id),
        args.force,
    )
    if args.with_skills:
        (plugin_root / "skills").mkdir(parents=True, exist_ok=True)

    print(f"Created Whisply plugin draft: {plugin_root}")
    print(f"plugin manifest: {plugin_root / 'plugin.json'}")
    print("This draft is unsigned. Review it in Settings → Plugins before install.")


if __name__ == "__main__":
    main()
