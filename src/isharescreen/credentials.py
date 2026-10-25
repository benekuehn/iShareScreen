"""Linux login-keyring access. Secrets travel through pipes, never argv/files."""

import shutil
import subprocess


def _attributes(host, user, port):
    return [
        "application", "isharescreen", "protocol", "rfb",
        "server", host.strip().lower().rstrip("."), "user", user,
        "port", str(port),
    ]


def load_password(host, user, port=5900):
    tool = shutil.which("secret-tool")
    if tool is None:
        return None
    try:
        result = subprocess.run(
            [tool, "lookup", *_attributes(host, user, port)],
            stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
            text=True, timeout=5, check=False,
        )
    except subprocess.TimeoutExpired:
        raise RuntimeError("Login keyring is locked or unavailable (lookup timed out)") from None
    if result.returncode == 0:
        return result.stdout.rstrip("\r\n") or None
    return None


def store_password(host, user, port, password):
    tool = shutil.which("secret-tool")
    if tool is None:
        raise RuntimeError(
            "Saving a password requires secret-tool and an unlocked Secret Service keyring; "
            "use --password-command or --password-stdin instead"
        )
    try:
        result = subprocess.run(
            [tool, "store", "--label=iShareScreen", *_attributes(host, user, port)],
            input=password, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
            text=True, timeout=30, check=False,
        )
    except subprocess.TimeoutExpired:
        raise RuntimeError("Login keyring is locked or unavailable (store timed out)") from None
    if result.returncode:
        raise RuntimeError("Could not save password in the login keyring")


def password_from_command(command):
    """Invoke a password manager directly, without interpreting shell syntax."""
    import shlex

    argv = shlex.split(command)
    if not argv:
        raise RuntimeError("--password-command requires a command")
    try:
        result = subprocess.run(
            argv, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL, text=True, timeout=15, check=False,
        )
    except (OSError, subprocess.TimeoutExpired):
        raise RuntimeError("Password command failed or timed out") from None
    if result.returncode or not result.stdout.rstrip("\r\n"):
        raise RuntimeError("Password command failed or returned an empty password")
    password = result.stdout.rstrip("\r\n")
    if "\n" in password or "\r" in password:
        raise RuntimeError("Password command must return exactly one password line")
    return password
