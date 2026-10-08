import json
import os
import stat

import pytest

from refactor_diff import settings


def test_load_without_a_file_gives_defaults(tmp_path):
    data = settings.load(tmp_path)
    assert data["ai"]["provider"] == "claude"
    assert data["ai"]["providers"]["claude"]["model"] == "claude-opus-5-5"
    assert data["ai"]["providers"]["ollama"]["host"] == "http://127.0.0.1:11434"
    assert data["ai"]["context"] == {"function": True, "pr": True, "references": False}


def test_load_tolerates_a_broken_file(tmp_path):
    (tmp_path / "settings.json").write_text("{not json")
    assert settings.load(tmp_path)["ai"]["provider"] == "claude"
    (tmp_path / "settings.json").write_text(json.dumps({"ai": {"provider": "nope"}}))
    assert settings.load(tmp_path)["ai"]["provider"] == "claude"


def test_save_round_trips_and_is_private(tmp_path):
    data = settings.load(tmp_path)
    data["ai"]["providers"]["claude"]["api_key"] = "sk-ant-secret-1234"
    data["ai"]["provider"] = "ollama"
    settings.save(data, tmp_path)
    path = settings.settings_path(tmp_path)
    assert stat.S_IMODE(os.stat(path).st_mode) == 0o600
    again = settings.load(tmp_path)
    assert again["ai"]["provider"] == "ollama"
    assert again["ai"]["providers"]["claude"]["api_key"] == "sk-ant-secret-1234"
    # Defaults are filled in for keys the file doesn't have.
    assert again["ai"]["providers"]["openai"]["base_url"] == ""


def test_public_view_masks_secrets_and_flags_configured(tmp_path):
    data = settings.load(tmp_path)
    data["ai"]["providers"]["claude"]["api_key"] = "sk-ant-secret-1234"
    view = settings.public_view(data)
    claude = view["ai"]["providers"]["claude"]
    assert claude["api_key"] == "••••1234" and claude["configured"] is True
    assert view["ai"]["providers"]["ollama"]["configured"] is False  # no model yet
    assert view["ai"]["active"] == {
        "provider": "claude",
        "model": "claude-opus-5-5",
        "configured": True,
    }
    assert "sk-ant" not in json.dumps(view)


def test_uses_default_mask_when_no_key(tmp_path):
    view = settings.public_view(settings.load(tmp_path))
    assert view["ai"]["providers"]["claude"]["api_key"] == ""
    assert view["ai"]["active"]["configured"] is False


def test_apply_update_keeps_a_masked_key_and_validates(tmp_path):
    current = settings.load(tmp_path)
    current["ai"]["providers"]["claude"]["api_key"] = "sk-ant-secret-1234"
    update = {
        "ai": {
            "provider": "ollama",
            "providers": {
                "claude": {
                    "api_key": "••••1234",
                    "model": " claude-sonnet-5-5 ",
                    "configured": True,
                },
                "ollama": {"model": "qwen3-coder:30b", "num_ctx": "65536"},
            },
            "context": {"references": True, "bogus": True},
        }
    }
    merged = settings.apply_update(current, update)
    assert merged["ai"]["provider"] == "ollama"
    assert merged["ai"]["providers"]["claude"]["api_key"] == "sk-ant-secret-1234"
    assert merged["ai"]["providers"]["claude"]["model"] == "claude-sonnet-5-5"
    assert merged["ai"]["providers"]["ollama"]["num_ctx"] == 65536
    assert merged["ai"]["context"]["references"] is True
    assert "bogus" not in merged["ai"]["context"]
    # A real new key replaces the stored one; an empty string clears it.
    merged = settings.apply_update(current, {"ai": {"providers": {"claude": {"api_key": ""}}}})
    assert merged["ai"]["providers"]["claude"]["api_key"] == ""

    with pytest.raises(ValueError):
        settings.apply_update(current, {"ai": {"provider": "cursor"}})
    with pytest.raises(ValueError):
        settings.apply_update(current, {"ai": {"providers": {"ollama": {"num_ctx": "lots"}}}})
    with pytest.raises(ValueError):
        settings.apply_update(current, {"nothing": 1})


def test_resolve_test_config_uses_stored_secret_for_masked_value(tmp_path):
    current = settings.load(tmp_path)
    current["ai"]["providers"]["claude"]["api_key"] = "sk-ant-secret-1234"
    resolved = settings.resolve_test_config(
        current, "claude", {"api_key": "••••1234", "model": "x"}
    )
    assert resolved["ai"]["provider"] == "claude"
    assert resolved["ai"]["providers"]["claude"] == {
        "api_key": "sk-ant-secret-1234",
        "model": "x",
        "base_url": "",
    }
    with pytest.raises(ValueError):
        settings.resolve_test_config(current, "cursor", {})
