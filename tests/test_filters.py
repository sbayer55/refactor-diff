"""Tags behind the import, file-rename and moved-function filters."""

from pathlib import Path

from conftest import commit, git

from refactor_diff.engine import analyze
from refactor_diff.model import MOVE, TAG_FILE_MOVE, TAG_IMPORTS, TAG_MOVED

HELPER = '''def helper(x, y):
    """Add things up."""
    total = x + y
    if total > 10:
        return total * 2
    return total
'''

OTHER = """def other(a):
    b = a - 1
    return b * b
"""

TS_HELPER = (
    "export function helper(x: number, y: number): number {\n  const total = x + y;\n"
    "  if (total > 10) {\n    return total * 2;\n  }\n  return total;\n}\n"
)


def report_for(tmp_path: Path, before: dict, after: dict):
    repo = tmp_path / "repo"
    commit(repo, before, "before")
    git(repo, "checkout", "-qb", "feature")
    commit(repo, after, "after")
    return analyze(repo, "main", "feature")


def tagged(report, tag):
    return [u for u in report.units.values() if tag in u.tags]


def move_units(report):
    [g] = [g for g in report.groups if g.kind == MOVE]
    return [report.units[u] for u in g.unit_ids]


# --- imports ---------------------------------------------------------------------------------


def test_import_only_edits_are_tagged(tmp_path):
    report = report_for(
        tmp_path,
        {"a.py": "import os\nimport sys\n\nprint(os, sys)\n"},
        {"a.py": "import sys\nimport os\nimport re\n\nprint(os, sys)\n"},
    )
    assert report.units and all(TAG_IMPORTS in u.tags for u in report.units.values())


def test_import_mixed_with_code_is_not_tagged(tmp_path):
    report = report_for(
        tmp_path,
        {"a.py": "import os; x = 1\n"},
        {"a.py": "import re; x = 2\n"},
    )
    assert not tagged(report, TAG_IMPORTS)


def test_code_change_is_not_tagged_as_import(tmp_path):
    report = report_for(
        tmp_path,
        {"a.py": "import os\n\nx = 1\n"},
        {"a.py": "import os\n\nx = 2\n"},
    )
    assert not tagged(report, TAG_IMPORTS)


def test_typescript_import_edits_are_tagged(tmp_path):
    report = report_for(
        tmp_path,
        {"a.ts": 'import { a } from "./m";\n\nexport const x = a;\n'},
        {"a.ts": 'import { a, b } from "./m";\n\nexport const x = a;\n'},
    )
    [u] = report.units.values()
    assert TAG_IMPORTS in u.tags
    [g] = report.groups
    assert g.kind == "import" and g.label == 'insert import { b } from "./m"'


# --- file renames ----------------------------------------------------------------------------


def test_pure_rename_and_its_import_updates(tmp_path):
    models = "class User:\n    pass\n"
    report = report_for(
        tmp_path,
        {"pkg/models.py": models, "pkg/api.py": "from pkg.models import User\n\nprint(User)\n"},
        {
            "pkg/models.py": None,
            "pkg/entities.py": models,
            "pkg/api.py": "from pkg.entities import User\n\nprint(User)\n",
        },
    )
    files = {f.path: f for f in report.files}
    assert files["pkg/entities.py"].pure_rename
    assert not files["pkg/api.py"].pure_rename
    [u] = tagged(report, TAG_FILE_MOVE)
    assert u.path == "pkg/api.py"


def test_relative_import_after_rename(tmp_path):
    models = "class User:\n    pass\n"
    report = report_for(
        tmp_path,
        {"pkg/models.py": models, "pkg/api.py": "from .models import User\n"},
        {
            "pkg/models.py": None,
            "pkg/entities.py": models,
            "pkg/api.py": "from .entities import User\n",
        },
    )
    assert [u.path for u in tagged(report, TAG_FILE_MOVE)] == ["pkg/api.py"]


def test_moved_importer_keeps_its_relative_target(tmp_path):
    body = "from .models import User\n\n\ndef make():\n    return User()\n"
    report = report_for(
        tmp_path,
        {"pkg/models.py": "class User:\n    pass\n", "pkg/api.py": body},
        {
            "pkg/models.py": "class User:\n    pass\n",
            "pkg/api.py": None,
            "pkg/sub/api.py": body.replace("from .models", "from ..models"),
        },
    )
    [f] = [f for f in report.files if f.path == "pkg/sub/api.py"]
    assert f.status == "R" and not f.pure_rename
    [u] = tagged(report, TAG_FILE_MOVE)
    assert u.path == "pkg/sub/api.py"


def test_import_of_a_different_module_is_not_a_file_move(tmp_path):
    report = report_for(
        tmp_path,
        {"a.py": "from pkg.models import User\n", "pkg/models.py": "class User:\n    pass\n"},
        {"a.py": "from pkg.other import User\n", "pkg/models.py": "class User:\n    pass\n"},
    )
    assert tagged(report, TAG_IMPORTS) and not tagged(report, TAG_FILE_MOVE)


def test_edits_inside_a_renamed_file_are_not_tagged(tmp_path):
    before = "import os\n\n\ndef f():\n    return os.sep\n\n\ndef g():\n    return 1\n" * 3
    after = before.replace("return 1", "return 2", 1)
    report = report_for(tmp_path, {"a.py": before}, {"a.py": None, "b.py": after})
    [f] = report.files
    assert f.status == "R" and not f.pure_rename
    assert report.units and not any(u.tags for u in report.units.values())


def test_typescript_specifier_after_rename(tmp_path):
    models = "export class User {}\n"
    report = report_for(
        tmp_path,
        {"src/models.ts": models, "src/api.ts": 'import { User } from "./models";\n'},
        {
            "src/models.ts": None,
            "src/entities/index.ts": models,
            "src/api.ts": 'import { User } from "./entities";\n',
        },
    )
    assert [u.path for u in tagged(report, TAG_FILE_MOVE)] == ["src/api.ts"]


# --- moved functions -------------------------------------------------------------------------


def test_exact_move_is_tagged_with_its_import(tmp_path):
    report = report_for(
        tmp_path,
        {
            "pkg/a.py": HELPER + "\n\n" + OTHER,
            "pkg/b.py": "X = 1\n",
            "pkg/c.py": "from pkg.a import helper\n\nprint(helper(1, 2))\n",
        },
        {
            "pkg/a.py": OTHER,
            "pkg/b.py": "X = 1\n\n\n" + HELPER,
            "pkg/c.py": "from pkg.b import helper\n\nprint(helper(1, 2))\n",
        },
    )
    units = move_units(report)
    assert len(units) == 3
    assert all(TAG_MOVED in u.tags for u in units)


def test_move_with_an_edit_is_not_certain(tmp_path):
    report = report_for(
        tmp_path,
        {"a.py": HELPER + "\n\n" + OTHER, "b.py": "X = 1\n"},
        {"a.py": OTHER, "b.py": "X = 1\n\n\n" + HELPER.replace("total * 2", "total * 3")},
    )
    assert move_units(report) and not tagged(report, TAG_MOVED)


def test_move_with_an_edited_comment_is_not_certain(tmp_path):
    commented = HELPER.replace("    return total\n", "    return total  # done\n")
    report = report_for(
        tmp_path,
        {"a.py": HELPER + "\n\n" + OTHER, "b.py": "X = 1\n"},
        {"a.py": OTHER, "b.py": "X = 1\n\n\n" + commented},
    )
    assert move_units(report) and not tagged(report, TAG_MOVED)


def test_duplicated_code_is_not_certain(tmp_path):
    # Deleted from two files, inserted once: which copy moved is a guess.
    report = report_for(
        tmp_path,
        {"a.py": HELPER + "\n\n" + OTHER, "b.py": HELPER + "\n\n" + OTHER, "c.py": "X = 1\n"},
        {"a.py": OTHER, "b.py": OTHER, "c.py": "X = 1\n\n\n" + HELPER},
    )
    assert move_units(report) and not tagged(report, TAG_MOVED)


def test_method_moved_to_another_class_is_not_certain(tmp_path):
    method = (
        "    def area(self):\n        w = self.width\n        h = self.height\n"
        "        return w * h\n"
    )
    report = report_for(
        tmp_path,
        {"a.py": "class A:\n    x = 1\n\n" + method, "b.py": "class B:\n    y = 2\n"},
        {"a.py": "class A:\n    x = 1\n", "b.py": "class B:\n    y = 2\n\n" + method},
    )
    assert move_units(report) and not tagged(report, TAG_MOVED)


def test_loose_statements_are_not_certain(tmp_path):
    block = "config = load()\nconfig.update(extra)\nconfig.validate(strict=True)\nrun(config)\n"
    report = report_for(
        tmp_path,
        {"a.py": block + "\n\n" + OTHER, "b.py": "X = 1\n"},
        {"a.py": OTHER, "b.py": "X = 1\n\n\n" + block},
    )
    assert move_units(report) and not tagged(report, TAG_MOVED)


def test_typescript_function_move_is_certain(tmp_path):
    report = report_for(
        tmp_path,
        {
            "a.ts": TS_HELPER + "\nexport const X = 1;\n",
            "b.ts": "export const Y = 2;\n",
            "c.ts": 'import { helper } from "./a";\n\nhelper(1, 2);\n',
        },
        {
            "a.ts": "export const X = 1;\n",
            "b.ts": "export const Y = 2;\n\n" + TS_HELPER,
            "c.ts": 'import { helper } from "./b";\n\nhelper(1, 2);\n',
        },
    )
    units = move_units(report)
    assert len(units) == 3  # block out, block in, import in c.ts
    assert all(TAG_MOVED in u.tags for u in units)
