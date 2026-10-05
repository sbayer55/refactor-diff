"""Load the files changed between two git revisions, a GitHub PR, or the working tree."""

from __future__ import annotations

import json
import shutil
import subprocess
from dataclasses import dataclass
from pathlib import Path

from refactor_diff.model import FileChange

WORKTREE = ":worktree:"


class SourceError(Exception):
    pass


@dataclass
class ResolvedSource:
    label: str
    base: str  # what the user asked for
    head: str
    base_sha: str  # merge-base actually diffed against
    head_sha: str | None  # None for the working tree
    pr: dict | None = None

    @property
    def identity(self) -> str:
        """What the comparison *is*, independent of the commits it currently resolves to, so
        review state keyed on it survives new commits: the PR, or the ref names."""
        if self.pr is not None:
            return f"pr:{self.pr['number']}"
        if self.head == WORKTREE:
            return f"worktree:{self.base}"
        return f"refs:{self.base}:{self.head}"

    def to_dict(self) -> dict:
        return {
            "label": self.label,
            "base": self.base,
            "head": self.head,
            "base_sha": self.base_sha,
            "head_sha": self.head_sha,
            "pr": self.pr,
            "identity": self.identity,
        }


def git(repo: Path, *args: str, input: bytes | None = None) -> bytes:
    proc = subprocess.run(["git", "-C", str(repo), *args], input=input, capture_output=True)
    if proc.returncode != 0:
        msg = proc.stderr.decode(errors="replace").strip()
        raise SourceError(f"git {' '.join(args)} failed: {msg}")
    return proc.stdout


def git_text(repo: Path, *args: str) -> str:
    return git(repo, *args).decode(errors="replace").strip()


def repo_root(path: Path) -> Path:
    return Path(git_text(path, "rev-parse", "--show-toplevel"))


def rev(repo: Path, ref: str) -> str:
    return git_text(repo, "rev-parse", "--verify", f"{ref}^{{commit}}")


def has_commit(repo: Path, sha: str) -> bool:
    try:
        git(repo, "cat-file", "-e", f"{sha}^{{commit}}")
        return True
    except SourceError:
        return False


def gh_available() -> bool:
    return shutil.which("gh") is not None


def gh_json(repo: Path, *args: str):
    if not gh_available():
        raise SourceError("The GitHub CLI (gh) is not installed or not on PATH.")
    proc = subprocess.run(["gh", *args], cwd=repo, capture_output=True)
    if proc.returncode != 0:
        raise SourceError(f"gh {' '.join(args)} failed: {proc.stderr.decode().strip()}")
    return json.loads(proc.stdout)


# --- listing what can be compared -------------------------------------------------------


def list_sources(repo: Path) -> dict:
    branches = git_text(
        repo,
        "for-each-ref",
        "--sort=-committerdate",
        "--format=%(refname:short)",
        "refs/heads",
        "refs/remotes",
    ).splitlines()
    branches = [b for b in branches if b and not b.endswith("/HEAD")]
    try:
        current = git_text(repo, "rev-parse", "--abbrev-ref", "HEAD")
    except SourceError:
        current = "HEAD"
    default_base = next(
        (b for b in ("main", "master", "origin/main", "origin/master") if b in branches),
        branches[0] if branches else "HEAD",
    )
    prs: list[dict] = []
    pr_error = None
    if gh_available():
        try:
            prs = gh_json(
                repo,
                "pr",
                "list",
                "--limit",
                "50",
                "--json",
                "number,title,headRefName,baseRefName,author",
            )
        except (SourceError, json.JSONDecodeError) as e:
            pr_error = str(e)
    else:
        pr_error = "gh CLI not found"
    return {
        "repo": str(repo),
        "branches": branches,
        "current": current,
        "default_base": default_base,
        "prs": prs,
        "pr_error": pr_error,
    }


# --- resolving a request into concrete revisions -----------------------------------------


def resolve(repo: Path, base: str | None, head: str | None, pr: int | None) -> ResolvedSource:
    if pr is not None:
        return _resolve_pr(repo, pr)
    if not base:
        raise SourceError("Choose a base ref to compare against.")
    head = head or "HEAD"
    base_sha = resolve_ref(repo, base)
    if head == WORKTREE:
        return ResolvedSource(f"{base} → working tree", base, head, base_sha, None)
    head_sha = resolve_ref(repo, head)
    merge_base = git_text(repo, "merge-base", base_sha, head_sha)
    return ResolvedSource(f"{base}...{head}", base, head, merge_base, head_sha)


def _resolve_pr(repo: Path, number: int) -> ResolvedSource:
    info = gh_json(
        repo,
        "pr",
        "view",
        str(number),
        "--json",
        "number,title,url,baseRefName,headRefName,baseRefOid,headRefOid",
    )
    base_sha, head_sha = info["baseRefOid"], info["headRefOid"]
    if not has_commit(repo, head_sha):
        _fetch(repo, f"pull/{number}/head")
    if not has_commit(repo, base_sha):
        _fetch(repo, info["baseRefName"])
    for sha in (base_sha, head_sha):
        if not has_commit(repo, sha):
            raise SourceError(f"Could not fetch commit {sha[:10]} for PR #{number}.")
    merge_base = git_text(repo, "merge-base", base_sha, head_sha)
    return ResolvedSource(
        f"#{number} {info['title']}",
        info["baseRefName"],
        info["headRefName"],
        merge_base,
        head_sha,
        pr=info,
    )


def resolve_ref(repo: Path, ref: str) -> str:
    """Resolve ``ref`` to a commit, falling back to a remote branch of the same name.

    A branch that was never checked out locally only exists as ``origin/<ref>``; if even that
    is missing, the branch is fetched from the remote.
    """
    try:
        return rev(repo, ref)
    except SourceError:
        pass
    remotes = _remotes(repo)
    for remote in remotes:
        try:
            return rev(repo, f"{remote}/{ref}")
        except SourceError:
            continue
    for remote in remotes:
        try:
            git(repo, "fetch", "--quiet", remote, f"+refs/heads/{ref}:refs/remotes/{remote}/{ref}")
            return rev(repo, f"{remote}/{ref}")
        except SourceError:
            continue
    searched = ", ".join(f"{r}/{ref}" for r in remotes)
    raise SourceError(
        f"Unknown ref {ref!r}: not a local ref"
        + (f", and not found as {searched} (also tried fetching it)." if remotes else ".")
    )


def _remotes(repo: Path) -> list[str]:
    """Remotes to try for fallbacks, most likely first."""
    remotes = git_text(repo, "remote").split()
    return [r for r in ("origin", "upstream") if r in remotes] or remotes[:1]


def _fetch(repo: Path, refspec: str) -> None:
    for remote in _remotes(repo):
        try:
            git(repo, "fetch", "--quiet", remote, refspec)
            return
        except SourceError:
            continue


# --- loading file contents ----------------------------------------------------------------


def load_changes(repo: Path, src: ResolvedSource) -> list[FileChange]:
    args = ["diff", "--name-status", "-M", "-z", src.base_sha]
    if src.head_sha:
        args.append(src.head_sha)
    fields = git(repo, *args).decode(errors="replace").split("\0")

    entries: list[tuple[str, str | None, str]] = []  # (status, old_path, new_path)
    i = 0
    while i < len(fields) and fields[i]:
        status = fields[i][0]
        if status in "RC":
            old, new = fields[i + 1], fields[i + 2]
            entries.append(("R" if status == "R" else "A", old if status == "R" else None, new))
            i += 3
        else:
            path = fields[i + 1]
            entries.append((status if status in "ADM" else "M", path, path))
            i += 2

    wanted: list[str] = []
    for status, old, new in entries:
        if status != "A" and old:
            wanted.append(f"{src.base_sha}:{old}")
        if status != "D" and src.head_sha:
            wanted.append(f"{src.head_sha}:{new}")
    blobs = read_blobs(repo, wanted)

    changes: list[FileChange] = []
    for status, old, new in entries:
        old_bytes = blobs.get(f"{src.base_sha}:{old}", b"") if status != "A" else b""
        if status == "D":
            new_bytes = b""
        elif src.head_sha:
            new_bytes = blobs.get(f"{src.head_sha}:{new}", b"")
        else:
            file = repo / new
            new_bytes = file.read_bytes() if file.is_file() else b""
        if b"\0" in old_bytes or b"\0" in new_bytes:
            continue  # binary
        changes.append(
            FileChange(
                path=new,
                old_path=old if status == "R" else None,
                status=status,
                old_text=old_bytes.decode("utf-8", errors="replace"),
                new_text=new_bytes.decode("utf-8", errors="replace"),
            )
        )
    return changes


def read_file_at(repo: Path, src: ResolvedSource, paths: list[str]) -> dict[str, str]:
    """Head-side contents of arbitrary files (used for repo-wide leftover checks)."""
    return read_files(repo, src.head_sha, paths)


def read_files(repo: Path, sha: str | None, paths: list[str]) -> dict[str, str]:
    """Contents of ``paths`` at commit ``sha``, or in the working tree when ``sha`` is None.
    Missing files are left out."""
    if sha is None:
        out = {}
        for p in paths:
            f = repo / p
            if f.is_file() and f.resolve().is_relative_to(repo.resolve()):
                out[p] = f.read_text(errors="replace")
        return out
    blobs = read_blobs(repo, [f"{sha}:{p}" for p in paths])
    return {k.split(":", 1)[1]: v.decode("utf-8", errors="replace") for k, v in blobs.items()}


def grep_files(repo: Path, src: ResolvedSource, word: str, pathspec: list[str]) -> list[str]:
    args = ["grep", "-l", "-w", "-F", "-e", word]
    if src.head_sha:
        args.append(src.head_sha)
    args += ["--", *pathspec]
    proc = subprocess.run(["git", "-C", str(repo), *args], capture_output=True)
    if proc.returncode not in (0, 1):
        return []
    files = proc.stdout.decode(errors="replace").splitlines()
    if src.head_sha:
        files = [f.split(":", 1)[1] for f in files]
    return files


def read_blobs(repo: Path, specs: list[str]) -> dict[str, bytes]:
    if not specs:
        return {}
    out = git(repo, "cat-file", "--batch", input=("\n".join(specs) + "\n").encode())
    blobs: dict[str, bytes] = {}
    pos = 0
    for spec in specs:
        nl = out.index(b"\n", pos)
        line = out[pos:nl]
        pos = nl + 1
        header = line.split()
        if line.endswith(b" missing") or len(header) != 3:
            continue
        size = int(header[2])
        blobs[spec] = out[pos : pos + size]
        pos += size + 1
    return blobs
