# -*- mode: python ; coding: utf-8 -*-
"""PyInstaller spec for the desktop app's backend (a onedir build of the refactor-diff CLI).

onedir rather than onefile: the app restarts the sidecar on every repository switch, and a
onefile binary re-extracts thousands of files (typeshed alone) on every start. It is also a
single process, so the app can SIGTERM the real Python process and let uvicorn shut down
gracefully.
"""

from pathlib import Path

from PyInstaller.utils.hooks import collect_data_files, collect_submodules

SRC = Path(SPECPATH).parent / "src"

datas = [
    # Served by server.py relative to its own __file__, so keep the package layout.
    (str(SRC / "refactor_diff" / "web" / "static"), "refactor_diff/web/static"),
    # Jedi runs `python jedi/.../subprocess/__main__.py` in the target repo's interpreter and
    # imports jedi and parso from the directory next to the bundled package, so both need
    # their .py sources on disk (plus typeshed stubs and parso's grammar files).
    *collect_data_files("jedi", include_py_files=True),
    *collect_data_files("parso", include_py_files=True),
]

hiddenimports = [
    # uvicorn and anyio pick implementations with importlib at run time.
    *collect_submodules("uvicorn"),
    "anyio._backends._asyncio",
    # C extensions imported by the TypeScript analyzer.
    "tree_sitter",
    "tree_sitter_javascript",
    "tree_sitter_typescript",
    "refactor_diff.languages.python",
    "refactor_diff.languages.typescript",
]

a = Analysis(
    [str(SRC / "refactor_diff" / "__main__.py")],
    pathex=[str(SRC)],
    binaries=[],
    datas=datas,
    hiddenimports=hiddenimports,
    hookspath=[],
    runtime_hooks=[],
    excludes=["tkinter", "pytest", "_pytest", "httpx2", "ruff"],
    noarchive=False,
)
pyz = PYZ(a.pure)

exe = EXE(
    pyz,
    a.scripts,
    # Run Python unbuffered: the app reads our stdout/stderr through pipes, and PyInstaller's
    # isolated interpreter ignores PYTHONUNBUFFERED.
    [("u", None, "OPTION")],
    exclude_binaries=True,
    name="refactor-diff-sidecar",
    debug=False,
    bootloader_ignore_signals=False,
    strip=False,
    upx=False,
    console=True,
    disable_windowed_traceback=False,
    argv_emulation=False,
    target_arch=None,
    codesign_identity=None,  # ad-hoc; Apple Silicon refuses to run unsigned native code
    entitlements_file=None,
)
coll = COLLECT(
    exe,
    a.binaries,
    a.datas,
    strip=False,
    upx=False,
    name="refactor-diff-sidecar",
)
