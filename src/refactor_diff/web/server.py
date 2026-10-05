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
from starlette.responses import FileResponse, JSONResponse, PlainTextResponse
from starlette.routing import Mount, Route
from starlette.staticfiles import StaticFiles

from refactor_diff import sources
from refactor_diff.engine import analyze
from refactor_diff.export import anchor_line, markdown_summary
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

    async def get_summary(request: Request):
        """The review summary as Markdown (what "Copy as Markdown" copies)."""
        report = reports.get(request.path_params["report_id"])
        if report is None:
            return _unknown_report()
        review = store.review(report.source["identity"], hunks_of(report))
        return PlainTextResponse(markdown_summary(report, review), media_type="text/markdown")

    async def get_commits(request: Request):
        """The commits between base and head, oldest first (empty for the working tree)."""
        report = reports.get(request.path_params["report_id"])
        if report is None:
            return _unknown_report()
        try:
            commits = await run_in_threadpool(
                sources.list_commits, repo, report.source["base_sha"], report.source["head_sha"]
            )
        except sources.SourceError as e:
            return JSONResponse({"error": str(e)}, status_code=400)
        return JSONResponse({"commits": commits})

    def _pr_of(report: Report) -> int | None:
        return report.source["pr"]["number"] if report.source.get("pr") else None

    async def post_pr_comment(request: Request):
        """Post ``{"body"}`` as a comment on the report's pull request."""
        report = reports.get(request.path_params["report_id"])
        if report is None:
            return _unknown_report()
        number = _pr_of(report)
        if number is None:
            return JSONResponse({"error": "This report isn't a pull request."}, status_code=400)
        body = (await request.json()).get("body", "")
        if not isinstance(body, str) or not body.strip():
            return JSONResponse({"error": "The comment is empty."}, status_code=400)
        try:
            url = await run_in_threadpool(sources.post_pr_comment, repo, number, body)
        except sources.SourceError as e:
            return JSONResponse({"error": str(e)}, status_code=400)
        return JSONResponse({"url": url})

    async def post_review_comment(request: Request):
        """Post ``{"body", "hunk_id"}`` as an inline review comment on the hunk's first
        changed line (``"line"``/``"side"`` override it)."""
        report = reports.get(request.path_params["report_id"])
        if report is None:
            return _unknown_report()
        number = _pr_of(report)
        if number is None:
            return JSONResponse({"error": "This report isn't a pull request."}, status_code=400)
        body = await request.json()
        text = body.get("body", "")
        hunk = report.hunks.get(body.get("hunk_id", ""))
        if not isinstance(text, str) or not text.strip() or hunk is None:
            return JSONResponse({"error": "A comment and a hunk are required."}, status_code=400)
        first = anchor_line(report, hunk)
        side = body.get("side") or ("RIGHT" if first.new_no else "LEFT")
        try:
            line = int(body.get("line") or (first.new_no if side == "RIGHT" else first.old_no))
        except (TypeError, ValueError):
            return JSONResponse({"error": "line must be an integer."}, status_code=400)
        path = hunk.path if side == "RIGHT" else _side_path(report, "old", hunk.path)
        try:
            url = await run_in_threadpool(
                sources.post_review_comment,
                repo,
                number,
                text,
                report.source["head_sha"],
                path,
                line,
                side,
            )
        except sources.SourceError as e:
            return JSONResponse({"error": str(e)}, status_code=400)
        return JSONResponse({"url": url, "path": path, "line": line, "side": side})

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
            Route("/api/report/{report_id}/summary.md", get_summary),
            Route("/api/report/{report_id}/commits", get_commits),
            Route("/api/report/{report_id}/pr/comment", post_pr_comment, methods=["POST"]),
            Route(
                "/api/report/{report_id}/pr/review-comment",
                post_review_comment,
                methods=["POST"],
            ),
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
