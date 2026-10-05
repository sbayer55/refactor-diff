"""``refactor-diff`` launcher: starts the local web UI for a repository."""

from __future__ import annotations

import argparse
import socket
import sys
import threading
import webbrowser
from pathlib import Path

import uvicorn

from refactor_diff import sources
from refactor_diff.categories import CATEGORIES
from refactor_diff.web.server import create_app

HOST = "127.0.0.1"


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    p = argparse.ArgumentParser(
        prog="refactor-diff",
        description="Open a local web UI that collapses repetitive refactor edits in a diff.",
    )
    p.add_argument("range", nargs="?", help="pre-select BASE..HEAD (or BASE...HEAD) in the UI")
    p.add_argument("--pr", type=int, help="pre-select a GitHub pull request number")
    p.add_argument(
        "--worktree", metavar="BASE", help="pre-select uncommitted changes compared against BASE"
    )
    p.add_argument(
        "--hide",
        metavar="KINDS",
        help="pre-set filters: comma-separated file kinds to hide (source, tests, docs, config, "
        "other) and/or 'comments' to hide comment- and docstring-only edits",
    )
    p.add_argument(
        "--exclude",
        metavar="GLOB",
        action="append",
        default=[],
        help="pre-set filters: hide files matching GLOB (repeatable), e.g. 'migrations'",
    )
    p.add_argument(
        "--python",
        metavar="PATH",
        help="Python interpreter or virtualenv used to resolve imports for code navigation "
        "(default: .venv, venv or env in the repository)",
    )
    p.add_argument(
        "--tsserver",
        metavar="PATH",
        help="tsserver (or TypeScript's tsserver.js) used for TypeScript/JavaScript code "
        "navigation (default: node_modules/typescript in the repository, then PATH)",
    )
    p.add_argument("--repo", type=Path, default=Path.cwd(), help="git repository (default: cwd)")
    p.add_argument("--port", type=int, default=0, help="port to listen on (default: random)")
    p.add_argument("--no-browser", action="store_true", help="don't open a browser window")
    return p.parse_args(argv)


def defaults_from(args: argparse.Namespace) -> dict:
    defaults = _source_defaults(args)
    filters = _filter_defaults(args)
    if filters:
        defaults["filters"] = filters
    return defaults


def _source_defaults(args: argparse.Namespace) -> dict:
    if args.pr is not None:
        return {"mode": "pr", "pr": args.pr}
    if args.worktree:
        return {"mode": "worktree", "base": args.worktree}
    if args.range:
        sep = "..." if "..." in args.range else ".."
        base, _, head = args.range.partition(sep)
        return {"mode": "refs", "base": base, "head": head or "HEAD"}
    return {}


def _filter_defaults(args: argparse.Namespace) -> dict:
    filters: dict = {}
    if args.hide:
        kinds = {k.strip().lower() for k in args.hide.split(",") if k.strip()}
        unknown = kinds - set(CATEGORIES) - {"comments"}
        if unknown:
            sys.exit(f"refactor-diff: unknown --hide value(s): {', '.join(sorted(unknown))}")
        filters["hidden"] = sorted(kinds & set(CATEGORIES))
        filters["hideDocs"] = "comments" in kinds
    if args.exclude:
        filters["exclude"] = args.exclude
    return filters


def free_port() -> int:
    with socket.socket() as s:
        s.bind((HOST, 0))
        return s.getsockname()[1]


def main(argv: list[str] | None = None) -> None:
    args = parse_args(argv)
    try:
        repo = sources.repo_root(args.repo)
    except sources.SourceError:
        sys.exit(f"refactor-diff: {args.repo} is not inside a git repository")

    port = args.port or free_port()
    url = f"http://{HOST}:{port}/"
    print(f"refactor-diff: serving {repo} at {url} (Ctrl+C to stop)")
    if not args.no_browser:
        threading.Timer(0.8, webbrowser.open, [url]).start()
    uvicorn.run(
        create_app(repo, defaults_from(args), args.python, args.tsserver),
        host=HOST,
        port=port,
        log_level="warning",
    )
