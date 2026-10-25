"""CLI-first entry point. The browser connect form is an explicit `iss gui`."""
from __future__ import annotations

import sys


def main() -> int:
    argv = sys.argv[1:]
    if argv[:1] == ["gui"] or "--tui" in argv:
        from isharescreen.gui.connect import main as gui_main
        return gui_main()
    from isharescreen.cli import main as cli_main
    return cli_main(argv)


if __name__ == "__main__":
    raise SystemExit(main())
