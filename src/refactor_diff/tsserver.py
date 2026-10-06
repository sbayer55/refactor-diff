"""Go-to-definition and find-references for TypeScript and JavaScript, using tsserver.

One long-lived tsserver process (TypeScript's language server, the one editors use) answers
queries for every revision: each side of a diff is a separate project root - a snapshot of the
commit, or the working tree. Snapshots link the repository's ``node_modules`` in, so imported
packages resolve to the installed versions, like the virtualenv does for Python.

tsserver is found via ``--tsserver``, then the repository's own ``node_modules/typescript``,
then a ``tsserver`` on PATH.
"""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import threading
from pathlib import Path

from refactor_diff.languages.base import split_lines
from refactor_diff.navigation import (
    LIBRARY,
    MAX_RESULTS,
    REPO,
    Location,
    NavigationError,
)
from refactor_diff.snapshots import Snapshots

TIMEOUT = 60.0  # seconds; the first query in a large project loads the whole program
_INSTALL_HINT = (
    "TypeScript navigation needs tsserver: install TypeScript in the repository "
    "(npm install --save-dev typescript) or pass --tsserver PATH."
)


class TsServerBackend:
    def __init__(self, repo: Path, snapshots: Snapshots, tsserver: str | None = None):
        self.repo = repo
        self.snapshots = snapshots
        self._tsserver = tsserver
        self._command: list[str] | None = None
        self._proc: subprocess.Popen | None = None
        self._seq = 0
        self.library_files: set[str] = set()
        self._lock = threading.Lock()

    # --- process ---------------------------------------------------------------------------

    @property
    def command(self) -> list[str]:
        if self._command is None:
            self._command = self._find()
        return self._command

    def _find(self) -> list[str]:
        candidates = []
        if self._tsserver:
            candidates.append(Path(self._tsserver).expanduser())
        candidates.append(self.repo / "node_modules" / "typescript" / "lib" / "tsserver.js")
        for path in candidates:
            if path.is_dir():
                path = path / "lib" / "tsserver.js"  # a typescript package directory
            if path.suffix == ".js" and path.is_file():
                node = _find_node()
                if node is None:
                    raise NavigationError("TypeScript navigation needs Node.js on PATH.")
                return [node, str(path)]
            if path.is_file():
                return [str(path)]
        if self._tsserver:
            raise NavigationError(f"Can't find tsserver at {self._tsserver}.")
        found = shutil.which("tsserver")
        if found is None:
            raise NavigationError(_INSTALL_HINT)
        return [found]

    def describe_environment(self) -> str:
        script = Path(self.command[-1]).resolve()
        version = ""
        for parent in script.parents:
            manifest = parent / "package.json"
            if manifest.is_file():
                try:
                    version = " " + json.loads(manifest.read_text())["version"]
                except (OSError, ValueError, KeyError):
                    pass
                break
        return f"TypeScript{version} at {script}"

    def _ensure_started(self) -> subprocess.Popen:
        if self._proc is None or self._proc.poll() is not None:
            try:
                self._proc = subprocess.Popen(
                    [*self.command, "--disableAutomaticTypingAcquisition"],
                    stdin=subprocess.PIPE,
                    stdout=subprocess.PIPE,
                    stderr=subprocess.DEVNULL,
                    cwd=self.repo,
                )
            except OSError as e:
                raise NavigationError(f"Can't start tsserver: {e}") from e
        return self._proc

    def close(self) -> None:
        with self._lock:
            proc, self._proc = self._proc, None
        if proc is not None and proc.poll() is None:
            try:
                self._send(proc, "exit", {})
                proc.wait(timeout=2)
            except (OSError, subprocess.TimeoutExpired):
                proc.kill()

    # --- protocol --------------------------------------------------------------------------

    def _send(self, proc: subprocess.Popen, command: str, arguments: dict) -> int:
        self._seq += 1
        message = {"seq": self._seq, "type": "request", "command": command, "arguments": arguments}
        assert proc.stdin is not None
        proc.stdin.write(json.dumps(message).encode() + b"\n")
        proc.stdin.flush()
        return self._seq

    def _request(self, proc: subprocess.Popen, command: str, arguments: dict) -> dict:
        seq = self._send(proc, command, arguments)
        timer = threading.Timer(TIMEOUT, proc.kill)  # a hung server must not hang the UI
        timer.start()
        try:
            while True:
                message = _read_message(proc)
                if message.get("type") == "response" and message.get("request_seq") == seq:
                    return message
        finally:
            timer.cancel()

    # --- queries ---------------------------------------------------------------------------

    def definitions(self, sha: str | None, path: str, line: int, col: int) -> list[Location]:
        return self._query(sha, path, line, col, "definition")

    def references(self, sha: str | None, path: str, line: int, col: int) -> list[Location]:
        return self._query(sha, path, line, col, "references")

    def _query(self, sha: str | None, path: str, line: int, col: int, command: str):
        root = self.snapshots.root(sha)
        file = root / path
        if not file.is_file():
            raise NavigationError(f"{path} doesn't exist at this revision.")
        lines = split_lines(file.read_text(errors="replace"))
        if not 0 < line <= len(lines):
            raise NavigationError(f"Line {line} is outside {path}.")
        offset = _utf16_len(lines[line - 1][:col]) + 1
        with self._lock:
            proc = self._ensure_started()
            try:
                self._send(proc, "open", {"file": str(file), "projectRootPath": str(root)})
                response = self._request(
                    proc, command, {"file": str(file), "line": line, "offset": offset}
                )
                self._send(proc, "close", {"file": str(file)})  # re-read on the next query
            except (OSError, ValueError) as e:
                self._proc = None
                raise NavigationError(f"tsserver stopped responding: {e}") from e
        if not response.get("success"):
            return []  # e.g. "No content available." on whitespace or a keyword
        body = response.get("body") or []
        if command == "references":
            items = [{**ref, "name": body.get("symbolName", "")} for ref in body.get("refs", [])]
        else:
            items = [{**d, "isDefinition": True} for d in body]
        locations = [self._location(item, root) for item in items[:MAX_RESULTS]]
        locations.sort(key=lambda loc: (loc.kind != REPO, loc.path, loc.line or 0))
        return list({(loc.path, loc.line, loc.col): loc for loc in locations}.values())

    def _location(self, item: dict, root: Path) -> Location:
        file = Path(item["file"])
        line = item["start"]["line"]
        lines = _read_lines(file)
        text = item.get("lineText")
        if text is None:
            text = lines[line - 1] if 0 < line <= len(lines) else ""
        source = lines[line - 1] if 0 < line <= len(lines) else text
        col = _char_col(source, item["start"]["offset"] - 1)
        name = item.get("name") or ""
        if not name and item["end"]["line"] == line:
            name = source[col : _char_col(source, item["end"]["offset"] - 1)]
        base = {
            "line": line,
            "col": col,
            "name": name,
            "type": "definition" if item.get("isDefinition") else "reference",
            "text": text,
            "is_definition": bool(item.get("isDefinition")),
        }
        relative = _relative_to(file, root)
        if relative is not None and "node_modules" not in relative.parts:
            return Location(kind=REPO, path=relative.as_posix(), **base)
        self.library_files.add(str(file))
        return Location(kind=LIBRARY, path=str(file), **base)


def _read_message(proc: subprocess.Popen) -> dict:
    """One ``Content-Length``-framed JSON message from tsserver's stdout."""
    assert proc.stdout is not None
    length = None
    while True:
        header = proc.stdout.readline()
        if not header:
            raise OSError("tsserver exited")
        header = header.strip()
        if not header:
            if length is not None:
                break
            continue
        name, _, value = header.decode(errors="replace").partition(":")
        if name.lower() == "content-length":
            length = int(value)
    return json.loads(proc.stdout.read(length))


def _find_node() -> str | None:
    found = shutil.which("node")
    if found:
        return found
    # nvm only puts node on PATH in interactive shells; fall back to its newest install.
    nvm = Path(os.environ.get("NVM_DIR", Path.home() / ".nvm")) / "versions" / "node"
    installs = sorted(nvm.glob("v*/bin/node"), key=lambda p: _version(p.parent.parent.name))
    return str(installs[-1]) if installs else None


def _version(name: str) -> tuple[int, ...]:
    try:
        return tuple(int(part) for part in name.lstrip("v").split("."))
    except ValueError:
        return ()


def _relative_to(file: Path, root: Path) -> Path | None:
    for candidate, base in ((file, root), (file.resolve(), root.resolve())):
        if candidate.is_relative_to(base):
            return candidate.relative_to(base)
    return None


def _read_lines(file: Path) -> list[str]:
    try:
        return split_lines(file.read_text(errors="replace"))
    except OSError:
        return []


def _utf16_len(text: str) -> int:
    return len(text.encode("utf-16-le")) // 2


def _char_col(line: str, utf16_col: int) -> int:
    """tsserver counts UTF-16 code units; tokens and the UI count characters."""
    units = 0
    for i, ch in enumerate(line):
        if units >= utf16_col:
            return i
        units += 2 if ord(ch) > 0xFFFF else 1
    return len(line)
