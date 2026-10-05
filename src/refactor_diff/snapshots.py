"""Python sources of a revision written to a temporary directory.

Code navigation (Jedi) needs real files laid out as they were at a commit; for a branch or
PR, neither side is checked out. A snapshot holds only the Python sources, which takes well
under a second even for large repos, and is reused for the lifetime of the server.
"""

from __future__ import annotations

import shutil
import subprocess
import tempfile
import threading
from pathlib import Path

from refactor_diff import sources

SNAPSHOT_SUFFIXES = (".py", ".pyi")
_BATCH = 500  # blobs per `git cat-file --batch` call


class Snapshots:
    def __init__(self, repo: Path, cache_dir: Path | None = None):
        self.repo = repo
        self.dir = cache_dir or Path(tempfile.mkdtemp(prefix="refactor-diff-"))
        self._roots: dict[str, Path] = {}
        self._lock = threading.Lock()

    def root(self, sha: str | None) -> Path:
        """Directory holding the revision's Python files; the repo itself for the working tree."""
        if sha is None:
            return self.repo
        with self._lock:
            if sha not in self._roots:
                self._roots[sha] = self._write(sha)
            return self._roots[sha]

    def close(self) -> None:
        shutil.rmtree(self.dir, ignore_errors=True)

    def _write(self, sha: str) -> Path:
        dest = self.dir / sha
        listing = subprocess.run(
            ["git", "-C", str(self.repo), "ls-tree", "-r", "-z", "--name-only", sha],
            capture_output=True,
            check=True,
        ).stdout.decode(errors="replace")
        paths = [p for p in listing.split("\0") if p.endswith(SNAPSHOT_SUFFIXES)]
        for i in range(0, len(paths), _BATCH):
            chunk = paths[i : i + _BATCH]
            blobs = sources.read_blobs(self.repo, [f"{sha}:{p}" for p in chunk])
            for spec, data in blobs.items():
                file = dest / spec.split(":", 1)[1]
                file.parent.mkdir(parents=True, exist_ok=True)
                file.write_bytes(data)
        dest.mkdir(parents=True, exist_ok=True)
        return dest
