"""CLI launches reuse geometry and credentials without a browser form."""

import io
import subprocess
from types import SimpleNamespace as S

import pytest

from isharescreen import cli, credentials
from isharescreen.tui import entry, storage


@pytest.fixture
def saved(tmp_path, monkeypatch):
    monkeypatch.setenv("XDG_STATE_HOME", str(tmp_path))
    storage.save_last(storage.ConnectFormValues(
        host="mini.local", user="viewer", password="not-persisted",
        advertise="3840x2160", hidpi="2", frontend="native",
        audio=True, curtain=True, hdr=False, share_console=True, alt_session=False,
        port=5901,
    ))
    return tmp_path


def test_saved_4k_backing_is_not_mistaken_for_4k_logical_points(saved):
    args = cli._parse_args(["--last"])
    assert args.host == "mini.local"
    assert args.port == 5901
    assert args.advertise == "1920x1080@2.0"
    assert args.frontend == "native"
    assert not args.dynamic


def test_explicit_flags_override_last_and_preset(saved):
    args = cli._parse_args([
        "--last", "--preset", "native-4k", "--host", "other.local",
        "--advertise", "1280x720@1", "--codec", "avc", "--dynamic",
    ])
    assert args.host == "other.local"
    assert args.advertise == "1280x720@1"
    assert args.codec == "avc"
    assert args.dynamic


def test_unattended_connection_uses_keyring_without_stdin_or_prompt(saved, monkeypatch):
    monkeypatch.setattr(cli.sys, "stdin", io.StringIO(""))
    lookups = []
    def lookup(*endpoint):
        lookups.append(endpoint)
        return "test-password"
    monkeypatch.setattr(credentials, "load_password", lookup)
    config = cli._build_session_config(cli._parse_args(["--last"]))
    assert config.password == "test-password"
    assert lookups == [("mini.local", "viewer", 5901)]
    assert config.advertise.width * config.advertise.hidpi_scale == 3840


def test_explicit_stdin_bypasses_keyring(saved, monkeypatch):
    monkeypatch.setattr(cli.sys, "stdin", io.StringIO("explicit\n"))
    monkeypatch.setattr(credentials, "load_password", lambda *args: pytest.fail("keyring lookup"))
    config = cli._build_session_config(cli._parse_args(["--last", "--password-stdin"]))
    assert config.password == "explicit"


def test_missing_unattended_credential_fails_without_launching_browser(saved, monkeypatch):
    monkeypatch.setattr(cli.sys, "stdin", io.StringIO(""))
    monkeypatch.setattr(credentials, "load_password", lambda *args: None)
    with pytest.raises(SystemExit, match="No browser connection is required"):
        cli._build_session_config(cli._parse_args(["--last"]))


def test_remember_persists_endpoint_and_backing_but_no_secret(tmp_path, monkeypatch):
    monkeypatch.setenv("XDG_STATE_HOME", str(tmp_path))
    monkeypatch.setattr(cli.sys, "stdin", io.StringIO("private-test-password\n"))
    saved = []
    monkeypatch.setattr(credentials, "store_password", lambda *args: saved.append(args))
    args = cli._parse_args([
        "--host", "mini.local", "-u", "viewer", "--port", "5901", "--auth", "nonsrp",
        "--preset", "native-4k", "--password-stdin", "--remember-password",
    ])
    cli._build_session_config(args)
    assert saved == [("mini.local", "viewer", 5901, "private-test-password")]
    profile = (tmp_path / "iss" / "last.json").read_text()
    assert "password" not in profile
    again = cli._parse_args(["--last"])
    assert again.advertise == "1920x1080@2.0"
    assert again.port == 5901
    assert again.codec == "hevc"
    assert again.auth == "nonsrp"


def test_console_entry_uses_cli_by_default(monkeypatch):
    calls = []
    monkeypatch.setattr(entry.sys, "argv", ["iss", "connect", "--last"])
    monkeypatch.setattr(cli, "main", lambda argv: calls.append(argv) or 0)
    assert entry.main() == 0
    assert calls == [["connect", "--last"]]
    monkeypatch.setattr(entry.sys, "argv", ["iss"])
    assert entry.main() == 0
    assert calls[-1] == []


def test_keyring_store_never_places_secret_in_argv(monkeypatch):
    monkeypatch.setattr(credentials.shutil, "which", lambda _: "/usr/bin/secret-tool")
    calls = []
    def run(argv, **kwargs):
        calls.append((argv, kwargs))
        return S(returncode=0, stdout="")
    monkeypatch.setattr(credentials.subprocess, "run", run)
    credentials.store_password("MINI.local.", "viewer", 5900, "private-test-password")
    argv, kwargs = calls[0]
    assert "private-test-password" not in argv
    assert kwargs["input"] == "private-test-password"
    assert argv[argv.index("server") + 1] == "mini.local"


def test_password_command_is_not_a_shell_and_errors_do_not_echo_secret(monkeypatch):
    calls = []
    def run(argv, **kwargs):
        calls.append((argv, kwargs))
        return S(returncode=0, stdout="password\n")
    monkeypatch.setattr(credentials.subprocess, "run", run)
    assert credentials.password_from_command("pass show 'remote/mini screen'") == "password"
    assert calls[0][0] == ["pass", "show", "remote/mini screen"]
    assert not calls[0][1].get("shell")
    def timeout(*args, **kwargs):
        raise subprocess.TimeoutExpired(["secret-tool"], 5, output="private-test-password")
    monkeypatch.setattr(credentials.subprocess, "run", timeout)
    with pytest.raises(RuntimeError) as error:
        credentials.password_from_command("pass show remote/mini")
    assert "private-test-password" not in str(error.value)
