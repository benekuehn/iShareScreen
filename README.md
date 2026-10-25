# iShareScreen

Cross-platform Python client for Apple's macOS Screen Sharing **High
Performance** mode (HEVC RExt 4:4:4 over UDP/SRTP). Renders the host
Mac's screen in a native wgpu window with hardware decode where
available.

This fork includes an experimental Linux GPU decode/composition path. See
[native/README.md](native/README.md) for build instructions, validation and
current limitations. Run `--frontend native` to try it.
The standalone Rust migration is tracked in [NATIVE_PORT.md](NATIVE_PORT.md).
Its current `iss-native --auth-check` executable authenticates without Python;
the standalone streaming viewer is still under construction.

## Setup

### On the host Mac (the target you want to view)

1. Any modern macOS that supports the Screen Sharing app's High
   Performance mode (Apple Silicon, recent macOS).
2. *System Settings → General → Sharing → **Screen Sharing*** → toggle on.

### On the viewing machine

#### macOS

1. Install Python 3.10 or later (from [python.org](https://www.python.org/downloads/),
   `brew install python`, or `uv python install 3.13`).
2. Install iShareScreen:
   ```sh
   pip install git+https://github.com/renegadelink/iShareScreen.git
   ```

#### Windows

1. Install Python 3.10 or later from [python.org](https://www.python.org/downloads/)
   (the installer includes pip; check "Add python.exe to PATH" during install).
2. Open PowerShell or cmd and run:
   ```sh
   pip install git+https://github.com/renegadelink/iShareScreen.git
   ```

3. (Optional, only if you want audio) install **libfdk-aac** — Apple's
   PT=101 audio uses AAC-ELD-SBR, which Windows Media Foundation can't
   decode. The cleanest source is [MSYS2](https://www.msys2.org):
   ```sh
   pacman -Sy --noconfirm mingw-w64-x86_64-fdk-aac
   ```
   This drops `libfdk-aac-2.dll` at `C:\msys64\mingw64\bin\`, which iss
   searches automatically. If you already have [scoop](https://scoop.sh),
   `scoop install msys2` followed by the same `pacman` command also
   works — iss looks under `%USERPROFILE%\scoop\apps\msys2\current\mingw64\bin`
   too. Without libfdk-aac, video works as normal and audio is silently
   skipped.

#### Linux (Debian / Ubuntu)

1. Install Python and the system libraries that the GPU + window stack
   need (Vulkan loader, OpenGL, X11 / Wayland surfaces, PortAudio,
   AAC-ELD-SBR audio decoder):
   ```sh
   sudo apt install python3 python3-pip python3-venv \
       libvulkan1 libgl1 libegl1 \
       libxrandr2 libxinerama1 libxcursor1 libxi6 \
       libportaudio2 libfdk-aac2 \
       xclip
   ```
   `libfdk-aac2` is only used for audio; if you skip it, video still
   works and audio is silently disabled.

   `xclip` enables bidirectional clipboard sync with the macOS host. On
   Wayland desktops use `wl-clipboard` instead. If neither is installed
   iss logs a one-line warning at startup and runs without clipboard
   sync — everything else still works.
2. (Optional, for hardware HEVC decode on Intel GPUs) install vaapi:
   ```sh
   sudo apt install vainfo intel-media-va-driver-non-free
   ```
   For AMD, swap the driver: `sudo apt install mesa-va-drivers`.
3. Install iShareScreen (in a venv recommended):
   ```sh
   python3 -m venv ~/.venvs/iss
   ~/.venvs/iss/bin/pip install git+https://github.com/renegadelink/iShareScreen.git
   ~/.venvs/iss/bin/iss     # or symlink to ~/.local/bin
   ```

For Fedora / Arch / openSUSE, translate the apt package names with your
distro's package manager (most are named the same or very close).

### Firewall

iss connects to the host Mac over **TCP 5900** (control) plus two UDP
flows: **5900** (audio + RTCP) and **5901** (video). It sends from both
UDP ports during connect. If a firewall still blocks the stream, allow
UDP 5900–5901 inbound.

## Usage

```sh
# One-time setup: prompts for the Mac password without echoing it.
iss connect --host mac.local -u me --preset native-4k --remember-password

# Subsequent launches, including unattended launches by a development agent:
iss connect --last
# Bare `iss` also reconnects using the saved settings and login keyring.
```

The CLI connects directly; the optional browser connection form is `iss gui`.
On Linux, `--remember-password` uses `secret-tool` and an unlocked Secret
Service login keyring (`libsecret` on Arch, `libsecret-tools` on Debian/Ubuntu).
It also saves the connection settings, without the password. The password
travels to the keyring through stdin, never a command argument or settings file.
Supplying this option stores the entered credential before connection; it does
not verify that the Mac accepts it. Keyring entries are scoped to host, user
and port. Other platforms can use stdin or a password-manager command.

Explicit flags override saved settings and the preset. `native-4k` selects
the Linux VAAPI/EGL HEVC viewer at 3840×2160 backing with a 1920×1080 logical
desktop; it does not guarantee sustained 60 fps. For this checkout, replace
`iss` with `./launch-performance.sh` to use its venv and saved settings.

For an existing password manager, use a command that prints one password line:

```sh
iss connect --last --password-command 'pass show remote/mini'
```

The command is executed directly, without a shell. Explicit stdin or password
command takes precedence over the login keyring. Interactive terminals prompt
if no credential exists; unattended launches fail with an actionable error.
After one-time keyring setup, an agent can run, stop and rerun the same CLI
without a browser form or further password entry while that keyring is unlocked.

The browser form accepts host / username / password
(masked) and resolution, then a live session view with per-tile fps and
loss, throughput, UDP queue health, and a log tail. The actual screen
streams in a separate window. Last-session values pre-fill the form on
the next launch; this form does not save passwords.

The existing streaming flags also work directly, for example:

```sh
iss --host mac.local -u me --advertise 1920x1080 --no-curtain
```

Useful keys in the live session: **f** force IDR refresh (fixes the
rare gray patch the auto-recovery doesn't catch), **r** reconnect,
**d** disconnect, **Ctrl-B** save a bug-report snapshot, **q** quit.

For CI / scripted use:

```sh
echo "$PASSWORD" | iss --headless --host mac.local -u me --password-stdin --auto-quit-secs 30
```

## License

AGPL-3.0-or-later. See [LICENSE](LICENSE).

This is an independent reverse-engineering of a publicly-documented
network protocol. No Apple source code, headers, or symbols are
included.
