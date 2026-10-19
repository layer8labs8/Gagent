#!/usr/bin/env python3
"""Lists the libraries that are built into Gagent, with their licenses.

Used by the release workflow:

    cargo metadata --format-version 1 --locked | python3 scripts/third_party.py > THIRD-PARTY-LICENSES.txt
"""
import json
import sys


def main() -> None:
    data = json.load(sys.stdin)
    members = set(data.get("workspace_members", []))
    rows = []
    for pkg in data["packages"]:
        if pkg["id"] in members:
            continue
        license_ = pkg.get("license") or (
            "see " + pkg["license_file"] if pkg.get("license_file") else "unknown"
        )
        rows.append((pkg["name"], pkg["version"], license_, pkg.get("repository") or ""))
    rows.sort(key=lambda r: (r[0].lower(), r[1]))

    print("Gagent includes the following open-source libraries.")
    print("Each one is used under its own license; the full texts are in the repositories named below")
    print("and in the packages on https://crates.io.")
    print()
    for name, version, license_, repo in rows:
        line = f"{name} {version} - {license_}"
        if repo:
            line += f" - {repo}"
        print(line)


if __name__ == "__main__":
    main()
