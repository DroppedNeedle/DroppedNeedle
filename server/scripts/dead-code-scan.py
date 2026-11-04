"""Fail on a public Rust item nothing references.

usage: python3 -I server/scripts/dead-code-scan.py server

Reads every workspace member's `src/` and `tests/`, and the server crate's
`tests/`. Comments and string contents are blanked first, so a name that
appears only in prose or a string is not a reference. Each item declared
`pub`, `pub(crate)`, `pub(super)` or `pub(in ..)` in a member's `src/` (fn,
const, static, struct, enum, trait, type) is dead when its name occurs
nowhere but in definitions of that name and on `use` lines. A format string's inline capture, `{name}`,
counts as a reference, as do a function named in an attribute string
(serde's `default = "f"`) and a name imported under an alias.

The scan works on names, not paths: an item that shares its name with a
live one is never reported. An entry in `dead-code-allowlist.txt` keeps an
item that is meant to stay (one read only by serde or a format capture, a
plugin protocol constant `PLUGINS.md` documents); each line is
`<path from server/> <name> <reason>`. The scan fails on a dead item not
listed, on a listed item that is no longer dead, and on a line without a
reason.
"""
import re
import sys
import tomllib
from pathlib import Path

KINDS = "fn|const|static|struct|enum|trait|type"
VIS = r"pub(?:\((?:crate|super|self|in\s+[\w:]+)\))?"
PUB_DEF = re.compile(
    r"^\s*%s\s+(?:default\s+)?(?:const\s+|async\s+|unsafe\s+|extern\s+\"C\"\s+)*(%s)\s+([A-Za-z_][A-Za-z0-9_]*)"
    % (VIS, KINDS)
)
ANY_DEF = re.compile(
    r"^\s*(?:%s\s+)?(?:default\s+)?(?:const\s+|async\s+|unsafe\s+)*(?:%s)\s+([A-Za-z_][A-Za-z0-9_]*)"
    % (VIS, KINDS)
)
USE = re.compile(r"^\s*(?:%s\s+)?use\b" % VIS)
WORD = re.compile(r"[A-Za-z_][A-Za-z0-9_]*")
RAW = re.compile(r'r(#*)"')
# A format string's inline capture, `{name}` or `{name:?}`, reads `name`.
CAPTURE = re.compile(r"\{([A-Za-z_][A-Za-z0-9_]*)(?::[^{}]*)?\}")
# An attribute that names a function in a string: serde's `default = "f"`,
# `with = "m"`, `deserialize_with = "m::f"` and the like.
ATTRIBUTE_PATH = re.compile(r'\b(?:default|with|deserialize_with|serialize_with|skip_serializing_if|bound)\s*=\s*"([^"]+)"')
# `use a::Name as Alias;` reads `Name` through its alias.
RENAMED = re.compile(r"\b([A-Za-z_][A-Za-z0-9_]*)\s+as\s+[A-Za-z_]")


def blank(text):
    """The text with comments and string contents replaced by spaces."""
    out = list(text)
    i, n = 0, len(text)

    def clear(start, end):
        for k in range(start, end):
            if text[k] != "\n":
                out[k] = " "

    while i < n:
        c = text[i]
        if text.startswith("//", i):
            j = text.find("\n", i)
            j = n if j == -1 else j
            clear(i, j)
            i = j
        elif text.startswith("/*", i):
            depth, j = 1, i + 2
            while j < n and depth:
                if text.startswith("/*", j):
                    depth, j = depth + 1, j + 2
                elif text.startswith("*/", j):
                    depth, j = depth - 1, j + 2
                else:
                    j += 1
            clear(i, j)
            i = j
        elif c in "rb" and (i == 0 or not (text[i - 1].isalnum() or text[i - 1] == "_")) and (
            raw := RAW.match(text, i + (1 if c == "b" and text.startswith("br", i) else 0))
        ):
            start = raw.end()
            close = '"' + raw.group(1)
            j = text.find(close, start)
            j = n if j == -1 else j
            clear(start, j)
            i = j + len(close)
        elif c == '"':
            j = i + 1
            while j < n and text[j] != '"':
                j += 2 if text[j] == "\\" else 1
            clear(i + 1, j)
            i = j + 1
        elif c == "'" and i + 2 < n and text[i + 2] == "'":
            i += 3
        elif c == "'" and text.startswith("'\\", i):
            j = text.find("'", i + 2)
            i = j + 1 if j != -1 and j - i < 12 else i + 1
        else:
            i += 1
    return "".join(out)


def sources(root):
    """(path from server/, is shipping source, text) for every file read."""
    manifest = tomllib.loads((root / "Cargo.toml").read_text())
    folders = []
    for member in manifest["workspace"]["members"]:
        base = root / member
        folders.append((base / "src", True))
        folders.append((base / "tests", False))
    seen = set()
    for folder, shipping in folders:
        if not folder.is_dir():
            continue
        for path in sorted(folder.rglob("*.rs")):
            rel = path.relative_to(root).as_posix()
            if rel in seen:
                continue
            seen.add(rel)
            yield rel, shipping, path.read_text()


def scan(root):
    """Every dead item as (path, line, kind, name)."""
    definitions = []
    counts = {}
    for rel, shipping, text in sources(root):
        named = [capture.group(1) for capture in CAPTURE.finditer(text)]
        for attribute in ATTRIBUTE_PATH.finditer(text):
            named.extend(WORD.findall(attribute.group(1)))
        for name in named:
            counts[name] = counts.get(name, 0) + 1
        in_use = False
        for no, line in enumerate(blank(text).split("\n"), 1):
            if USE.match(line):
                in_use = True
            public = PUB_DEF.match(line) if shipping else None
            defined = ANY_DEF.match(line)
            for word in WORD.finditer(line):
                name = word.group(0)
                if defined and defined.group(1) == name and word.start() == defined.start(1):
                    if public and public.group(2) == name:
                        definitions.append((rel, no, public.group(1), name))
                    continue
                if not in_use:
                    counts[name] = counts.get(name, 0) + 1
            if in_use:
                for renamed in RENAMED.finditer(line):
                    counts[renamed.group(1)] = counts.get(renamed.group(1), 0) + 1
            if in_use and ";" in line:
                in_use = False
    return [item for item in definitions if not counts.get(item[3])]


def allowlist(path):
    """{(path, name): reason}, and the lines that give no reason."""
    listed, wrong = {}, []
    for no, line in enumerate(path.read_text().splitlines(), 1):
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        parts = line.split(None, 2)
        if len(parts) < 3:
            wrong.append(f"{path.name}:{no}: no reason given: {line}")
            continue
        listed[(parts[0], parts[1])] = parts[2]
    return listed, wrong


def main():
    root = Path(sys.argv[1] if len(sys.argv) > 1 else "server").resolve()
    listed, wrong = allowlist(Path(__file__).with_name("dead-code-allowlist.txt"))
    dead = scan(root)
    found = set()
    for rel, no, kind, name in dead:
        found.add((rel, name))
        if (rel, name) not in listed:
            wrong.append(f"{rel}:{no}: {kind} {name} is referenced nowhere")
    for (rel, name), reason in listed.items():
        if (rel, name) not in found:
            wrong.append(f"{rel}: {name} is listed ({reason}) but no longer dead; remove the line")
    allowed = len(listed)
    if wrong:
        print("\n".join(wrong))
        print(f"{len(wrong)} problem(s); {allowed} item(s) allowlisted", file=sys.stderr)
        return 1
    print(f"no unreferenced public item; {allowed} allowlisted")
    return 0


if __name__ == "__main__":
    sys.exit(main())
