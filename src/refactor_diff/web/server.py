"""Local web UI: a Starlette app serving the SPA and a small JSON API.

Bound to 127.0.0.1 only. Future actions (posting PR review comments, applying edits to the
working tree) belong here as POST routes that take unit/group IDs from a cached report.
"""

from __future__ import annotations

import contextlib
from pathlib import Path

from starlette.applications import Starlette
from starlette.concurrency import run_in_threadpool
from starlette.requests import Request
from starlette.responses import FileResponse, JSONResponse
from starlette.routing import Mount, Route
from starlette.staticfiles import StaticFiles

from refactor_diff import sources
from refactor_diff.engine import analyze
from refactor_diff.fileview import file_diff
from refactor_diff.languages.base import split_lines
from refactor_diff.model import Report
from refactor_diff.navigation import NavigationError, Navigator, to_dict
from refactor_diff.snapshots import Snapshots
from refactor_diff.state import ReviewStore

STATIC = Path(__file__).parent / "static"


SIDES = ("old", "new")


def create_app(
    repo: Path,
    defaults: dict | None = None,
    python: str | None = None,
    state_dir: Path | None = None,
) -> Starlette:
    reports: dict[str, Report] = {}
    snapshots = Snapshots(repo)
    navigator = Navigator(repo, snapshots, python)
    store = ReviewStore(repo, state_dir)

    def hunks_of(report: Report) -> dict[str, str]:
        return {h.fingerprint: h.path for h in report.hunks.values()}

    def with_review(report: Report) -> dict:
        return report.to_dict() | {
            "review": store.review(report.source["identity"], hunks_of(report))
        }

    async def index(request: Request):
        return FileResponse(STATIC / "index.html")

    async def config(request: Request):
        return JSONResponse({"repo": str(repo), "defaults": defaults or {}})

    async def list_sources(request: Request):
        try:
            return JSONResponse(await run_in_threadpool(sources.list_sources, repo))
        except sources.SourceError as e:
            return JSONResponse({"error": str(e)}, status_code=400)

    async def run_analysis(request: Request):
        body = await request.json()
        try:
            pr = int(body["pr"]) if body.get("pr") not in (None, "") else None
            min_count = max(1, int(body.get("min_count") or 2))
        except (TypeError, ValueError):
            return JSONResponse(
                {"error": "PR number and min count must be integers."}, status_code=400
            )
        try:
            report = await run_in_threadpool(
                analyze, repo, body.get("base"), body.get("head"), pr, min_count
            )
        except sources.SourceError as e:
            return JSONResponse({"error": str(e)}, status_code=400)
        reports[report.id] = report
        await run_in_threadpool(
            store.record_analysis,
            report.source["identity"],
            report.source["head_sha"],
            hunks_of(report),
        )
        return JSONResponse(with_review(report))

    async def get_report(request: Request):
        report = reports.get(request.path_params["report_id"])
        if report is None:
            return _unknown_report()
        return JSONResponse(with_review(report))

    async def get_review(request: Request):
        report = reports.get(request.path_params["report_id"])
        if report is None:
            return _unknown_report()
        return JSONResponse(store.review(report.source["identity"], hunks_of(report)))

    async def mark_review(request: Request):
        """Add/remove reviewed marks; body: ``{"groups": {"add", "remove"}, "hunks": {...}}``."""
        report = reports.get(request.path_params["report_id"])
        if report is None:
            return _unknown_report()
        body = await request.json()
        changes = {}
        for field in ("groups", "hunks"):
            change = body.get(field)
            if change is None:
                continue
            if not isinstance(change, dict) or not all(
                isinstance(change.get(k, []), list) for k in ("add", "remove")
            ):
                return JSONResponse({"error": f"{field} must be {{add: [], remove: []}}."}, 400)
            changes[field] = change
        await run_in_threadpool(store.mark, report.source["identity"], **changes)
        return JSONResponse(store.review(report.source["identity"], hunks_of(report)))

    async def get_file(request: Request):
        """Whole-file diff of one changed file in a report (``?path=``)."""
        report = reports.get(request.path_params["report_id"])
        if report is None:
            return _unknown_report()
        data = file_diff(report, request.query_params.get("path", ""))
        if data is None:
            return JSONResponse({"error": "That file is not part of this diff."}, status_code=404)
        return JSONResponse(data)

    async def navigate(request: Request):
        """Go to definition / find references for the name at (line, col) on one side."""
        report = reports.get(request.path_params["report_id"])
        if report is None:
            return _unknown_report()
        body = await request.json()
        action, side = body.get("action"), body.get("side")
        try:
            line, col = int(body["line"]), int(body["col"])
        except (KeyError, TypeError, ValueError):
            return JSONResponse({"error": "line and col must be integers."}, status_code=400)
        if action not in ("definition", "references") or side not in SIDES:
            return JSONResponse({"error": "Unknown action or side."}, status_code=400)
        path = _side_path(report, side, body.get("path", ""))
        query = navigator.definitions if action == "definition" else navigator.references
        try:
            locations = await run_in_threadpool(query, _side_sha(report, side), path, line, col)
            env = navigator.describe_environment()
        except NavigationError as e:
            return JSONResponse({"error": str(e)}, status_code=400)
        return JSONResponse(
            {"action": action, "side": side, "environment": env, "locations": to_dict(locations)}
        )

    async def get_source(request: Request):
        """Any repository file at one side's revision (``?side=old|new&path=``)."""
        report = reports.get(request.path_params["report_id"])
        if report is None:
            return _unknown_report()
        side = request.query_params.get("side")
        if side not in SIDES:
            return JSONResponse({"error": "side must be old or new."}, status_code=400)
        path = request.query_params.get("path", "")
        texts = await run_in_threadpool(sources.read_files, repo, _side_sha(report, side), [path])
        if path not in texts:
            return JSONResponse(
                {"error": f"{path} doesn't exist in the {_side_name(side)} version."},
                status_code=404,
            )
        return JSONResponse({"path": path, "side": side, "lines": split_lines(texts[path])})

    async def get_library(request: Request):
        """A library file (installed package or stub) that a navigation result pointed at."""
        path = request.query_params.get("path", "")
        try:
            text = await run_in_threadpool(navigator.library_source, path)
        except (NavigationError, OSError) as e:
            return JSONResponse({"error": str(e)}, status_code=404)
        return JSONResponse({"path": path, "lines": split_lines(text)})

    @contextlib.asynccontextmanager
    async def lifespan(app):
        yield
        snapshots.close()

    return Starlette(
        lifespan=lifespan,
        routes=[
            Route("/", index),
            Route("/api/config", config),
            Route("/api/sources", list_sources),
            Route("/api/analyze", run_analysis, methods=["POST"]),
            Route("/api/report/{report_id}", get_report),
            Route("/api/report/{report_id}/file", get_file),
            Route("/api/report/{report_id}/review", get_review),
            Route("/api/report/{report_id}/review", mark_review, methods=["POST"]),
            Route("/api/report/{report_id}/navigate", navigate, methods=["POST"]),
            Route("/api/report/{report_id}/source", get_source),
            Route("/api/library", get_library),
            Mount("/static", StaticFiles(directory=STATIC), name="static"),
        ],
    )


def _unknown_report() -> JSONResponse:
    return JSONResponse({"error": "Unknown report; run the analysis again."}, status_code=404)


def _side_sha(report: Report, side: str) -> str | None:
    """Commit for a side; None means the working tree."""
    return report.source["base_sha"] if side == "old" else report.source["head_sha"]


def _side_path(report: Report, side: str, path: str) -> str:
    """The UI names files by their new path; a renamed file's old side lives at old_path."""
    if side == "old":
        for f in report.files:
            if f.path == path and f.old_path:
                return f.old_path
    return path


def _side_name(side: str) -> str:
    return "original" if side == "old" else "new"
