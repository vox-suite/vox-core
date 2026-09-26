#!/usr/bin/env python3
"""Validate or compare a Vox declarative skill package without contacting Core."""
import argparse
import json
import re
import sys
from pathlib import Path

SECRET_MARKERS = (
    "-----begin private key", "-----begin rsa private key", "bearer ",
    "api_key=", "client_secret=", "sk-proj-", "sk_live_", "ghp_",
)


def load(path: Path) -> dict:
    data = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(data, dict):
        raise ValueError("Package must be a JSON object")
    required = ("external_key", "title", "summary", "instructions")
    for key in required:
        if not isinstance(data.get(key), str) or not data[key].strip():
            raise ValueError(f"{key} must be non-empty text")
    if not re.fullmatch(r"[a-z0-9-]{1,128}", data["external_key"]):
        raise ValueError("external_key must contain only lowercase letters, digits, and hyphens (max 128)")
    for key, limit in (("title", 120), ("summary", 500), ("instructions", 16384)):
        if len(data[key].encode("utf-8")) > limit:
            raise ValueError(f"{key} exceeds the Core byte limit of {limit}")
    capabilities = data.get("requested_capabilities", [])
    if not isinstance(capabilities, list) or len(capabilities) > 64 or any(
        not isinstance(item, str) or not item or len(item.encode("utf-8")) > 255 for item in capabilities
    ):
        raise ValueError("requested_capabilities must be up to 64 non-empty capability keys")
    resources = data.get("resources", {})
    if not isinstance(resources, dict) or len(json.dumps(resources).encode("utf-8")) > 32768:
        raise ValueError("resources must be an object of at most 32 KiB")
    joined = (data["instructions"] + json.dumps(resources)).lower()
    if any(marker in joined for marker in SECRET_MARKERS):
        raise ValueError("Package appears to contain credentials; store credentials in Core connections")
    return data


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("package", type=Path)
    parser.add_argument("--compare", type=Path, help="Review changes from an earlier package")
    args = parser.parse_args()
    try:
        current = load(args.package)
        previous = load(args.compare) if args.compare else None
    except (OSError, ValueError, json.JSONDecodeError) as error:
        print(f"Skill validation failed: {error}", file=sys.stderr)
        return 1
    print(f"Valid skill: {current['title']} ({current['external_key']})")
    requested = set(current.get("requested_capabilities", []))
    print("Requested capabilities: " + (", ".join(sorted(requested)) or "none"))
    print("Capability requests never grant access; the user must enable each agent separately.")
    if previous:
        old = set(previous.get("requested_capabilities", []))
        print("Added capabilities: " + (", ".join(sorted(requested - old)) or "none"))
        print("Removed capabilities: " + (", ".join(sorted(old - requested)) or "none"))
        for field in ("instructions", "resources", "title", "summary"):
            if current.get(field, {} if field == "resources" else "") != previous.get(field, {} if field == "resources" else ""):
                print(f"Changed: {field}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
