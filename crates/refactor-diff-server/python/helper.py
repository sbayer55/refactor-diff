"""Jedi helper for refactor-diff's Python code navigation.

Runs inside the *user's* interpreter (their virtualenv, or whatever python3 the server found)
so that third-party packages resolve to the installed versions. Jedi and parso are vendored
with the server and passed as ``argv[1]``, the directory that contains ``jedi/`` and
``parso/``; it is inserted at the front of ``sys.path``.

Protocol (one JSON object per line on stdin/stdout):

    -> {"ready": true, "python": "3.12", "prefix": ..., "executable": ..., "jedi": "0.20.0"}
       or {"ready": false, "error": "..."} followed by exit status 2

    <- {"id": 1, "cmd": "definitions" | "references" | "environment",
        "root": "...", "repo": "...", "file": "...", "line": 5, "col": 12, "limit": 1000}
    -> {"id": 1, "ok": true, "result": {"names": [{"module_path": ..., "module_name": ...,
        "line": ..., "col": ..., "name": ..., "type": ..., "is_definition": ...}]}}
       {"id": 1, "ok": false, "kind": "position" | "internal", "error": "..."}

Any sys.path entry of the environment that points back into the repository but not into the
environment itself (an editable install of the project) is remapped under the queried root,
so project imports resolve to the code at that revision rather than the current checkout.
"""

import json
import os
import sys
import traceback


def _emit(obj):
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()


def _fail_startup(error):
    _emit({"ready": False, "error": error})
    sys.exit(2)


if sys.version_info < (3, 10):
    _fail_startup(
        "Python %d.%d is too old; code navigation needs Python 3.10 or newer."
        % sys.version_info[:2]
    )

if len(sys.argv) > 1:
    sys.path.insert(0, sys.argv[1])

try:
    import jedi
    from jedi.api.environment import SameEnvironment
except Exception as e:  # noqa: BLE001 - report anything, the server shows the message
    _fail_startup("can't import jedi: %s: %s" % (type(e).__name__, e))


def _is_relative_to(path, base):
    try:
        return os.path.commonpath([path, base]) == base
    except ValueError:
        return False


class Helper:
    def __init__(self):
        self.env = SameEnvironment()
        self._env_sys_path = None
        self._projects = {}

    def env_sys_path(self):
        if self._env_sys_path is None:
            self._env_sys_path = list(self.env.get_sys_path())
        return self._env_sys_path

    def project(self, root, repo):
        """A Jedi project for one revision root; venv entries inside the repo are remapped."""
        key = (root, repo)
        project = self._projects.get(key)
        if project is None:
            venv = os.path.abspath(self.env.path)
            repo = os.path.abspath(repo)
            sys_path = [root]
            for entry in self.env_sys_path():
                p = os.path.abspath(entry)
                if _is_relative_to(p, repo) and not _is_relative_to(p, venv):
                    p = os.path.join(root, os.path.relpath(p, repo))
                sys_path.append(p)
            project = jedi.Project(root, sys_path=list(dict.fromkeys(sys_path)), smart_sys_path=False)
            self._projects[key] = project
        return project

    def handle(self, request):
        cmd = request.get("cmd")
        if cmd == "environment":
            return {"python": _version(), "prefix": self.env.path, "executable": self.env.executable}
        if cmd not in ("definitions", "references"):
            raise ValueError("unknown command %r" % (cmd,))
        root = os.path.abspath(request["root"])
        file = request["file"]
        line, col = int(request["line"]), int(request["col"])
        limit = int(request.get("limit") or 1000)
        script = jedi.Script(path=file, project=self.project(root, request["repo"]), environment=self.env)
        if cmd == "definitions":
            names = script.goto(line, col, follow_imports=True, follow_builtin_imports=True)
        else:
            names = script.get_references(line, col, include_builtins=False)
        return {"names": [_name(n) for n in names[:limit]]}


def _name(n):
    module = n.module_path
    return {
        "module_path": None if module is None else str(module),
        "module_name": n.module_name,
        "line": n.line,
        "col": n.column,
        "name": n.name,
        "type": n.type,
        "is_definition": bool(n.is_definition()),
    }


def _version():
    return "%d.%d" % sys.version_info[:2]


def main():
    helper = Helper()
    _emit(
        {
            "ready": True,
            "python": _version(),
            "prefix": sys.prefix,
            "executable": sys.executable,
            "jedi": jedi.__version__,
        }
    )
    for raw in sys.stdin:
        raw = raw.strip()
        if not raw:
            continue
        try:
            request = json.loads(raw)
        except ValueError as e:
            _emit({"id": None, "ok": False, "kind": "internal", "error": "bad request: %s" % e})
            continue
        rid = request.get("id")
        try:
            result = helper.handle(request)
        except ValueError as e:  # a position outside the file
            _emit({"id": rid, "ok": False, "kind": "position", "error": str(e)})
        except Exception as e:  # noqa: BLE001 - never let one query kill the helper
            traceback.print_exc()
            _emit({"id": rid, "ok": False, "kind": "internal", "error": "%s: %s" % (type(e).__name__, e)})
        else:
            _emit({"id": rid, "ok": True, "result": result})


if __name__ == "__main__":
    main()
