"""Local web UI: a Starlette app serving the SPA and a small JSON API.

Bound to 127.0.0.1 only. Future actions (posting PR review comments, applying edits to the
working tree) belong here as POST routes that take unit/group IDs from a cached report.
"""

from __future__ import annotations

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
from refactor_diff.model import Report

STATIC = Path(__file__).parent / "static"


def create_app(repo: Path, defaults: dict | None = None) -> Starlette:
    reports: dict[str, Report] = {}

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
        return JSONResponse(report.to_dict())

    async def get_report(request: Request):
        report = reports.get(request.path_params["report_id"])
        if report is None:
            return _unknown_report()
        return JSONResponse(report.to_dict())

    async def get_file(request: Request):
        """Whole-file diff of one changed file in a report (``?path=``)."""
        report = reports.get(request.path_params["report_id"])
        if report is None:
            return _unknown_report()
        data = file_diff(report, request.query_params.get("path", ""))
        if data is None:
            return JSONResponse({"error": "That file is not part of this diff."}, status_code=404)
        return JSONResponse(data)

    return Starlette(
        routes=[
            Route("/", index),
            Route("/api/config", config),
            Route("/api/sources", list_sources),
            Route("/api/analyze", run_analysis, methods=["POST"]),
            Route("/api/report/{report_id}", get_report),
            Route("/api/report/{report_id}/file", get_file),
            Mount("/static", StaticFiles(directory=STATIC), name="static"),
        ]
    )


def _unknown_report() -> JSONResponse:
    return JSONResponse({"error": "Unknown report; run the analysis again."}, status_code=404)
