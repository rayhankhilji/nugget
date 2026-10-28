#!/usr/bin/env python3
"""Fail when the Nugget book drifts from the code.

Run from anywhere: `python3 docs/book/check-refs.py`. Exit status 1 lists every problem.
Standard library only (Python 3.11+ for tomllib). See docs/book/src/contributing/docs.md.

What it checks:
  1. Tables preceded by `<!-- check-refs: GROUP -->`: the first backticked token of each row is
     compared, both ways, with the names extracted from the source for GROUP. Config tables with a
     Default column also have plain numeric and boolean defaults compared with `impl Default`.
     IPC method tables have their Daemon column (`yes` / `not yet`) compared with nuggetd's handler.
  2. Whole-book scans: backticked IPC method names and dotted config keys, `nuggetd` commands and
     flags in code, `NUGGET_*` variables, ```toml blocks, and relative links with anchors.

The Rust is parsed with regular expressions, so this is tied to the current code layout. If a struct
moves, update SOURCES below.
"""

from __future__ import annotations

import re
import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
BOOK = Path(__file__).resolve().parent / "src"

# Rust structs whose fields a marker documents: group -> (file, struct name).
SOURCES: dict[str, tuple[str, str]] = {
    "config.sections": ("crates/nuggetd/src/config.rs", "Config"),
    "config.models": ("crates/nuggetd/src/config.rs", "ModelsConfig"),
    "config.models.selected": ("crates/nuggetd/src/config.rs", "TierSelection"),
    "config.runtime": ("crates/nuggetd/src/config.rs", "RuntimeConfig"),
    "config.ingest": ("crates/nugget-ingest/src/config.rs", "IngestConfig"),
    "config.retrieve": ("crates/nugget-retrieve/src/config.rs", "RetrieveConfig"),
    "config.retrieve.recency": ("crates/nugget-retrieve/src/config.rs", "RecencyBoost"),
    "config.providers": ("crates/nugget-infer/src/config.rs", "ProviderConfig"),
    "mcp.scopes": ("crates/nugget-core/src/ipc.rs", "McpScopes"),
    "connector.files": ("crates/nugget-connector-files/src/config.rs", "FilesSettings"),
    "connector.obsidian": ("crates/nugget-connector-files/src/config.rs", "ObsidianSettings"),
    "connector.gmail": ("crates/nugget-connector-google/src/settings.rs", "GmailSettings"),
    "connector.gcal": ("crates/nugget-connector-google/src/settings.rs", "CalendarSettings"),
    "connector.imap": ("crates/nugget-connector-imap/src/settings.rs", "ImapSettings"),
    "connector.caldav": ("crates/nugget-connector-caldav/src/lib.rs", "CalDavSettings"),
}

# Config table path (dotted) -> group, for TOML validation and dotted-key references.
CONFIG_TABLES = {
    "": "config.sections",
    "models": "config.models",
    "models.selected": "config.models.selected",
    "runtime": "config.runtime",
    "ingest": "config.ingest",
    "retrieve": "config.retrieve",
    "retrieve.recency": "config.retrieve.recency",
    "providers": "config.providers",
}

CONNECTOR_FILES = [
    "crates/nugget-connector-files/src/lib.rs",
    "crates/nugget-connector-google/src/gmail.rs",
    "crates/nugget-connector-google/src/gcal.rs",
    "crates/nugget-connector-imap/src/lib.rs",
    "crates/nugget-connector-caldav/src/lib.rs",
]

# Documented as planned; they must NOT exist yet. When one lands, remove it here and update the
# "planned" notes in the book.
PLANNED = {
    "commands": {"mcp": "MCP stdio bridge (guide/mcp.md, reference/cli.md)"},
    "flags": {"--client": "flag of the planned `nuggetd mcp`"},
}

METHOD_PREFIXES = (
    "status", "chat", "search", "providers", "models", "egress", "sources", "ingest",
    "retrieve", "runtime", "knowledge", "mcp", "draft", "voice",
)
FILE_SUFFIXES = (".toml", ".json", ".md", ".rs", ".log", ".db", ".sock", ".lock", ".key",
                 ".plist", ".service", ".pid", ".rten")
PROSE_AFTER_NUGGETD = {"is", "was", "has", "exits", "started", "now", "running"}

problems: list[str] = []


def fail(msg: str) -> None:
    problems.append(msg)


def read(rel: str) -> str:
    return (ROOT / rel).read_text(encoding="utf-8")


# ---------------------------------------------------------------- Rust extraction


def struct_body(text: str, name: str) -> str:
    m = re.search(r"pub(?:\([a-z]+\))? struct " + name + r"\b[^{;]*\{(.*?)\n\}", text, re.S)
    if not m:
        raise SystemExit(f"check-refs: cannot find struct {name}; update SOURCES")
    return m.group(1)


def struct_fields(rel: str, name: str) -> list[str]:
    body = struct_body(read(rel), name)
    fields, rename = [], None
    for line in body.splitlines():
        line = line.strip()
        r = re.search(r'rename\s*=\s*"([^"]+)"', line)
        if line.startswith("#[") and r:
            rename = r.group(1)
            continue
        m = re.match(r"pub(?:\([a-z]+\))?\s+(?:r#)?(\w+)\s*:", line)
        if m:
            fields.append(rename or m.group(1))
            rename = None
    return fields


def literal(v: str):
    v = v.strip().rstrip(",")
    if v in ("true", "false"):
        return v == "true"
    expr = v.replace("_", "")
    if re.fullmatch(r"[0-9.]+(\s*\*\s*[0-9.]+)*", expr):
        return eval(expr)  # noqa: S307 - digits, dots and `*` only
    return None


def struct_defaults(rel: str, name: str) -> dict[str, object]:
    m = re.search(r"impl Default for " + name + r"\b.*?Self \{(.*?)\n\s*\}", read(rel), re.S)
    out: dict[str, object] = {}
    if m:
        for fm in re.finditer(r"^\s*(\w+):\s*(.+?),?\s*$", m.group(1), re.M):
            val = literal(fm.group(2))
            if val is not None:
                out[fm.group(1)] = val
    return out


def str_consts(text: str) -> dict[str, str]:
    return dict(re.findall(r'const (\w+): &str = "([^"]+)";', text))


def ipc_consts() -> tuple[dict[str, str], dict[str, str]]:
    text = read("crates/nugget-core/src/ipc.rs")
    methods = str_consts(re.search(r"pub mod method \{(.*?)\n\}", text, re.S).group(1))
    notes = str_consts(re.search(r"pub mod notification \{(.*?)\n\}", text, re.S).group(1))
    return methods, notes


def error_codes() -> dict[str, int]:
    text = read("crates/nugget-ipc/src/error.rs")
    return {k: int(v) for k, v in re.findall(r"pub const (\w+): i64 = (-?\d+);", text)}


def handled_methods(methods: dict[str, str]) -> set[str]:
    arms = re.findall(r"method::(\w+)\s*=>", read("crates/nuggetd/src/handler.rs"))
    return {methods[a] for a in arms if a in methods}


def mcp_tools() -> list[str]:
    text = read("crates/nugget-mcp/src/tools.rs")
    consts = str_consts(text)
    names = re.search(r"ALL_TOOLS: \[&str; \d+\] = \[(.*?)\];", text, re.S).group(1)
    return [consts[n] for n in re.findall(r"\w+", names)]


def connector_kinds() -> dict[str, str]:
    out = {}
    for rel in CONNECTOR_FILES:
        text = read(rel)
        consts = str_consts(text)
        for ident, display in re.findall(r'kind: (\w+),\s*display_name: "([^"]+)"', text):
            out[consts[ident]] = display
    return out


def cli() -> tuple[set[str], set[str]]:
    text = read("crates/nuggetd/src/main.rs")
    body = re.search(r"enum Command \{(.*?)\n\}", text, re.S).group(1)
    variants = re.findall(r"^    ([A-Z]\w*)", body, re.M)
    commands = {re.sub(r"(?<!^)([A-Z])", r"-\1", v).lower() for v in variants}
    flags = {"--help", "--version"}
    for m in re.finditer(r"#\[arg\(([^)]*)\)\]\s*(?:///.*\n\s*)*(\w+):", text):
        if "long" in m.group(1):
            flags.add("--" + m.group(2).replace("_", "-"))
    return commands, flags


def workspace_crates() -> set[str]:
    members = re.search(r"^members = \[(.*?)\]", read("Cargo.toml"), re.S | re.M).group(1)
    names = set()
    for path in re.findall(r'"([^"]+)"', members):
        m = re.search(r'^name = "([^"]+)"', read(f"{path}/Cargo.toml"), re.M)
        names.add(m.group(1))
    return names


def source_env_vars() -> set[str]:
    found = set()
    for base in ("crates", "apps/desktop/src-tauri", "bench"):
        for p in (ROOT / base).rglob("*.rs"):
            found.update(re.findall(r'"(NUGGET_[A-Z0-9_]+)"', p.read_text(encoding="utf-8")))
    return found


# ---------------------------------------------------------------- Markdown parsing


def code_spans(md: str) -> list[str]:
    return re.findall(r"`([^`\n]+)`", strip_fences(md))


def strip_fences(md: str) -> str:
    return re.sub(r"^```.*?^```", "", md, flags=re.S | re.M)


def fences(md: str) -> list[tuple[str, str]]:
    return re.findall(r"^```([\w,-]*)\n(.*?)^```", md, re.S | re.M)


def marked_tables(md: str) -> list[tuple[str, list[list[str]]]]:
    out = []
    lines = md.splitlines()
    for i, line in enumerate(lines):
        m = re.match(r"<!-- check-refs: ([\w.]+) -->", line.strip())
        if not m:
            continue
        rows, j = [], i + 1
        while j < len(lines) and not lines[j].startswith("|"):
            j += 1
        header = True
        while j < len(lines) and lines[j].startswith("|"):
            cells = [c.strip() for c in re.split(r"(?<!\\)\|", lines[j].strip())[1:-1]]
            if header or set(lines[j]) <= set("|-: "):
                header = False
            else:
                rows.append(cells)
            j += 1
        out.append((m.group(1), rows))
    return out


def first_code(cell: str) -> str | None:
    m = re.search(r"`([^`]+)`", cell)
    return m.group(1) if m else None


def slug(text: str) -> str:
    text = text.replace("`", "")
    text = re.sub(r"\[([^\]]*)\]\([^)]*\)", r"\1", text)
    out = []
    for ch in text.strip().lower():
        if ch.isalnum() or ch in "-_":
            out.append(ch)
        elif ch.isspace():
            out.append("-")
    return "".join(out)


def anchors(md: str) -> set[str]:
    seen: dict[str, int] = {}
    result = set()
    for h in re.findall(r"^#{1,6}\s+(.+?)\s*$", strip_fences(md), re.M):
        s = slug(h)
        n = seen.get(s, 0)
        result.add(s if n == 0 else f"{s}-{n}")
        seen[s] = n + 1
    return result


# ---------------------------------------------------------------- checks


def main() -> int:
    pages = {p: p.read_text(encoding="utf-8") for p in sorted(BOOK.rglob("*.md"))}
    methods, notes = ipc_consts()
    handled = handled_methods(methods)
    commands, flags = cli()

    expected: dict[str, set[str]] = {g: set(struct_fields(*s)) for g, s in SOURCES.items()}
    expected["ipc.methods"] = set(methods.values())
    expected["ipc.notifications"] = set(notes.values())
    codes = error_codes()
    expected["ipc.errors"] = set(codes)
    expected["mcp.tools"] = set(mcp_tools())
    kinds = connector_kinds()
    expected["connector.kinds"] = set(kinds)
    expected["cli.commands"] = commands
    expected["cli.flags"] = flags
    expected["crates"] = workspace_crates()
    env_in_source = source_env_vars()

    documented: dict[str, set[str]] = {}
    for page, md in pages.items():
        rel = page.relative_to(BOOK)
        for group, rows in marked_tables(md):
            if group.startswith("env."):
                expected.setdefault(group, set())
            elif group not in expected:
                fail(f"{rel}: unknown check-refs group `{group}`")
                continue
            names = documented.setdefault(group, set())
            for cells in rows:
                name = first_code(cells[0])
                if name is None:
                    fail(f"{rel} [{group}]: row without a backticked name: {cells[0]!r}")
                    continue
                if group == "cli.flags":
                    name = name.split()[0]
                names.add(name)
                check_row(rel, group, name, cells, codes, kinds, handled)

    for group, want in expected.items():
        have = documented.get(group)
        if have is None:
            fail(f"no table is marked `check-refs: {group}`")
            continue
        if group.startswith("env."):
            for v in sorted(have - env_in_source):
                fail(f"[{group}] `{v}` is documented but not used in the source")
            continue
        for name in sorted(have - want):
            fail(f"[{group}] `{name}` is documented but does not exist in the code")
        for name in sorted(want - have):
            fail(f"[{group}] `{name}` exists in the code but is not documented")

    check_defaults(pages)
    config_paths = {
        (f"{t}.{f}" if t else f)
        for t, g in CONFIG_TABLES.items()
        for f in expected[g]
    }
    for page, md in pages.items():
        rel = page.relative_to(BOOK)
        scan_tokens(rel, md, set(methods.values()) | set(notes.values()) | set(expected), config_paths)
        scan_cli(rel, md, commands, flags)
        for v in sorted(set(re.findall(r"\bNUGGET_[A-Z0-9_]*[A-Z0-9]\b", md)) - env_in_source):
            fail(f"{rel}: `{v}` does not appear in the source")
        for lang, body in fences(md):
            if lang == "toml":
                check_toml(rel, body, expected)
        check_links(page, md, pages)

    for kind, items in PLANNED.items():
        real = commands if kind == "commands" else flags
        for name, what in items.items():
            if name in real:
                fail(f"`{name}` ({what}) now exists: remove it from PLANNED and update the book")

    if problems:
        print(f"check-refs: {len(problems)} problem(s)", file=sys.stderr)
        for p in problems:
            print(f"  - {p}", file=sys.stderr)
        return 1
    print(f"check-refs: OK ({len(pages)} pages, {sum(len(v) for v in documented.values())} names checked)")
    return 0


def check_row(rel, group, name, cells, codes, kinds, handled) -> None:
    if group == "ipc.errors" and name in codes:
        code = first_code(cells[1]) if len(cells) > 1 else None
        if code is None or int(code) != codes[name]:
            fail(f"{rel}: error `{name}` is documented as {code}, the code says {codes[name]}")
    if group == "connector.kinds" and name in kinds:
        if len(cells) < 2 or cells[1] != kinds[name]:
            fail(f"{rel}: kind `{name}` display name {cells[1:2]} != {kinds[name]!r}")
    if group == "ipc.methods":
        daemon = cells[-1].strip().lower()
        if daemon not in ("yes", "not yet"):
            fail(f"{rel}: method `{name}` Daemon column must be `yes` or `not yet`, not {daemon!r}")
        elif (daemon == "yes") != (name in handled):
            actual = "implements" if name in handled else "does not implement"
            fail(f"{rel}: method `{name}` is marked {daemon!r} but nuggetd {actual} it")


def check_defaults(pages) -> None:
    for page, md in pages.items():
        for group, rows in marked_tables(md):
            if group not in SOURCES or not group.startswith("config."):
                continue
            defaults = struct_defaults(*SOURCES[group])
            for cells in rows:
                name = first_code(cells[0])
                if name not in defaults or len(cells) < 3:
                    continue
                doc = first_code(cells[2])
                doc_val = literal(doc) if doc else None
                if doc_val is None:
                    continue
                src = defaults[name]
                if (isinstance(src, bool) != isinstance(doc_val, bool)) or abs(float(src) - float(doc_val)) > 1e-9:
                    fail(f"{page.relative_to(BOOK)}: `{name}` default is documented as {doc}, the code says {src}")


def scan_tokens(rel, md, ipc_names, config_paths) -> None:
    for tok in set(code_spans(md)):
        tok = tok.strip()
        if not re.fullmatch(r"[a-z_]+(\.[a-z_]+)+", tok) or tok.endswith(FILE_SUFFIXES):
            continue
        if tok.split(".")[0] not in METHOD_PREFIXES:
            continue
        if tok not in ipc_names and tok not in config_paths:
            fail(f"{rel}: `{tok}` is not an IPC method, notification, config key or check-refs group")


def nuggetd_invocations(md):
    for lang, body in fences(md):
        if lang in ("", "sh", "bash", "console", "powershell", "pwsh", "text"):
            yield from body.splitlines()
    for span in code_spans(md):
        if span.startswith("nuggetd "):
            yield span


def scan_cli(rel, md, commands, flags) -> None:
    for line in nuggetd_invocations(md):
        for m in re.finditer(r"\bnuggetd((?:\s+[^\s|;&]+)*)", line):
            tokens = m.group(1).split()
            if not tokens:
                continue
            if re.fullmatch(r"[a-z][a-z-]*", tokens[0]):
                cmd = tokens[0]
                if cmd in PROSE_AFTER_NUGGETD:
                    continue
                if cmd not in commands and cmd not in PLANNED["commands"]:
                    fail(f"{rel}: `nuggetd {cmd}` is not a nuggetd command")
            for t in tokens:
                f = t.split("=")[0]
                if re.fullmatch(r"--[a-z][a-z-]*", f) and f not in flags and f not in PLANNED["flags"]:
                    fail(f"{rel}: `{f}` (after nuggetd) is not a nuggetd flag")


def check_toml(rel, body, expected) -> None:
    try:
        doc = tomllib.loads(body)
    except tomllib.TOMLDecodeError as e:
        fail(f"{rel}: a toml block does not parse: {e}")
        return

    def walk(table: dict, path: str) -> None:
        group = CONFIG_TABLES.get(path)
        if group is None:
            fail(f"{rel}: toml block has unknown table `[{path}]`")
            return
        for key, val in table.items():
            if key not in expected[group]:
                fail(f"{rel}: toml block has unknown key `{key}` in `[{path or 'top level'}]`")
                continue
            sub = f"{path}.{key}" if path else key
            if isinstance(val, dict) and sub in CONFIG_TABLES:
                walk(val, sub)
            elif isinstance(val, list) and sub == "providers":
                for p in val:
                    walk(p, sub)

    walk(doc, "")


def check_links(page, md, pages) -> None:
    rel = page.relative_to(BOOK)
    for target in re.findall(r"\]\(([^)\s]+)\)", strip_fences(md)):
        if re.match(r"[a-z]+:", target):
            continue
        path, _, anchor = target.partition("#")
        dest = (page.parent / path).resolve() if path else page
        if dest not in pages:
            fail(f"{rel}: link to missing page `{target}`")
            continue
        if anchor and anchor not in anchors(pages[dest]):
            fail(f"{rel}: link `{target}` names a missing heading")


if __name__ == "__main__":
    sys.exit(main())
