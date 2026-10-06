"""Go-to-definition and find-references: Jedi for Python, tsserver for TypeScript/JavaScript.

Each side of a diff is resolved in its own revision: removed lines against a snapshot of the
base commit, added and unchanged lines against the head (a snapshot, or the working tree).
The user's virtualenv supplies third-party packages; any of its sys.path entries that point
back into the repository (an editable install of the project) are remapped into the snapshot,
so project imports resolve to the code at that revision rather than the current checkout.
"""

from __future__ import annotations

import threading
from dataclasses import asdict, dataclass
from pathlib import Path

import jedi

from refactor_diff.languages import analyzer_for
from refactor_diff.languages.base import split_lines
from refactor_diff.snapshots import Snapshots

MAX_RESULTS = 1000
VENV_DIRS = (".venv", "venv", "env")

REPO = "repo"  # a file in the repository at the queried revision
LIBRARY = "library"  # an installed package, stdlib or typeshed stub
BUILTIN = "builtin"  # compiled builtin with no source to show


class NavigationError(Exception):
    pass


@dataclass
class Location:
    kind: str
    path: str  # repo-relative for REPO, absolute for LIBRARY, module name for BUILTIN
    line: int | None
    col: int | None
    name: str
    type: str  # function, class, module, param, statement, ...
    text: str  # the source line, for previews
    is_definition: bool = False


class Navigator:
    """Dispatches each query to the backend for the file's language."""

    def __init__(
        self,
        repo: Path,
        snapshots: Snapshots,
        python: str | None = None,
        tsserver: str | None = None,
    ):
        from refactor_diff.tsserver import TsServerBackend  # imports this module's types

        self.jedi = JediBackend(repo, snapshots, python)
        self.tsserver = TsServerBackend(repo, snapshots, tsserver)

    def _backend(self, path: str):
        analyzer = analyzer_for(path)
        if analyzer is None:
            raise NavigationError(f"Code navigation isn't available for {path}.")
        return self.jedi if analyzer.name == "python" else self.tsserver

    def definitions(self, sha: str | None, path: str, line: int, col: int) -> list[Location]:
        return self._backend(path).definitions(sha, path, line, col)

    def references(self, sha: str | None, path: str, line: int, col: int) -> list[Location]:
        return self._backend(path).references(sha, path, line, col)

    def describe_environment(self, path: str) -> str:
        return self._backend(path).describe_environment()

    def library_source(self, path: str) -> str:
        """Source of a library file that an earlier result pointed at."""
        if path not in self.jedi.library_files | self.tsserver.library_files:
            raise NavigationError("Only library files reached through navigation can be viewed.")
        return Path(path).read_text(errors="replace")

    def close(self) -> None:
        self.tsserver.close()


class JediBackend:
    def __init__(self, repo: Path, snapshots: Snapshots, python: str | None = None):
        self.repo = repo
        self.snapshots = snapshots
        self._python = python
        self._env = None
        self._projects: dict[Path, jedi.Project] = {}
        self._lines: dict[Path, list[str]] = {}
        self.library_files: set[str] = set()  # library files results pointed at (viewable)
        self._lock = threading.Lock()  # Jedi's caches aren't thread-safe

    # --- environment -------------------------------------------------------------------

    @property
    def environment(self):
        if self._env is None:
            python = self._python or self._find_venv()
            try:
                self._env = (
                    jedi.create_environment(python, safe=False)
                    if python
                    else jedi.get_default_environment()
                )
            except jedi.InvalidPythonEnvironment as e:
                raise NavigationError(f"Can't use Python environment {python}: {e}") from e
        return self._env

    def _find_venv(self) -> str | None:
        for name in VENV_DIRS:
            if (self.repo / name / "pyvenv.cfg").is_file():
                return str(self.repo / name)
        return None

    def describe_environment(self) -> str:
        env = self.environment
        return f"Python {'.'.join(map(str, env.version_info[:2]))} at {env.path}"

    def _project(self, root: Path) -> jedi.Project:
        if root not in self._projects:
            venv = Path(self.environment.path)  # the environment's sys.prefix
            sys_path = [str(root)]
            for entry in self.environment.get_sys_path():
                p = Path(entry)
                if p.is_relative_to(self.repo) and not p.is_relative_to(venv):
                    p = root / p.relative_to(self.repo)  # e.g. an editable install's src/
                sys_path.append(str(p))
            self._projects[root] = jedi.Project(
                str(root), sys_path=list(dict.fromkeys(sys_path)), smart_sys_path=False
            )
        return self._projects[root]

    # --- queries -------------------------------------------------------------------------

    def definitions(self, sha: str | None, path: str, line: int, col: int) -> list[Location]:
        def run(script: jedi.Script):
            return script.goto(line, col, follow_imports=True, follow_builtin_imports=True)

        return self._query(sha, path, run)

    def references(self, sha: str | None, path: str, line: int, col: int) -> list[Location]:
        def run(script: jedi.Script):
            return script.get_references(line, col, include_builtins=False)

        return self._query(sha, path, run)

    def _query(self, sha: str | None, path: str, run) -> list[Location]:
        root = self.snapshots.root(sha)
        file = root / path
        if not file.is_file():
            raise NavigationError(f"{path} doesn't exist at this revision.")
        with self._lock:
            script = jedi.Script(
                path=str(file), project=self._project(root), environment=self.environment
            )
            try:
                names = run(script)
            except ValueError as e:  # position outside the file
                raise NavigationError(str(e)) from e
            locations = [self._location(n, root) for n in names[:MAX_RESULTS]]
        locations.sort(key=lambda loc: (loc.kind != REPO, loc.path, loc.line or 0))
        return list({(loc.path, loc.line, loc.col): loc for loc in locations}.values())

    def _location(self, name, root: Path) -> Location:
        module = name.module_path
        base = {
            "line": name.line,
            "col": name.column,
            "name": name.name,
            "type": name.type,
            "is_definition": name.is_definition(),
        }
        if module is None:
            return Location(kind=BUILTIN, path=name.module_name, text="", **base)
        module = Path(module)
        text = self._line_text(module, name.line)
        if module.is_relative_to(root):
            return Location(kind=REPO, path=module.relative_to(root).as_posix(), text=text, **base)
        self.library_files.add(str(module))
        return Location(kind=LIBRARY, path=str(module), text=text, **base)

    def _line_text(self, file: Path, line: int | None) -> str:
        if line is None:
            return ""
        lines = self._lines.get(file)
        if lines is None:
            try:
                lines = split_lines(file.read_text(errors="replace"))
            except OSError:
                lines = []
            if not file.is_relative_to(self.repo):  # working-tree files can still change
                self._lines[file] = lines
        return lines[line - 1] if 0 < line <= len(lines) else ""


def to_dict(locations: list[Location]) -> list[dict]:
    return [asdict(loc) for loc in locations]
