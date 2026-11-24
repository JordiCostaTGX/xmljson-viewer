#!/bin/sh
set -eu

cd "$(dirname "$0")"
destination="${1:-${XDG_DATA_HOME:-${HOME:?HOME must be set}/.local/share}/xmljson-viewer}"
case "$destination" in
    /*) ;;
    *) printf '%s\n' "The installation directory must be absolute" >&2; exit 1 ;;
esac
case "$destination" in
    *%*|*\\*|*\"*|*\$*|*\`*|*[[:cntrl:]]*)
        printf '%s\n' "Use an installation directory without %, backslashes, quotes, dollar signs, backticks or control characters (desktop launcher limitation)" >&2
        exit 1 ;;
esac
binary=target/release/xmljson-viewer
if [ ! -x "$binary" ] && [ -x bin/xmljson-viewer ]; then
    binary=bin/xmljson-viewer
fi
if [ ! -x "$binary" ]; then
    printf '%s\n' "Run cargo build --release --locked first" >&2
    exit 1
fi
if [ -e "$destination/bin/xmljson-viewer" ]; then
    printf '%s\n' "$destination/bin/xmljson-viewer already exists; it will not be overwritten automatically" >&2
    exit 1
fi
install -d "$destination/bin"
install -m 755 "$binary" "$destination/bin/xmljson-viewer"
install -m 644 README.md "$destination/README.md"
install -m 644 resources/visor-mascot.png "$destination/visor-mascot.png"
install -m 644 resources/icon-provenance.txt "$destination/icon-provenance.txt"
# Quote spaces; reject paths incompatible with desktop Exec processing above.
python3 - "$destination" <<'PY'
from pathlib import Path
import sys

destination = Path(sys.argv[1])
binary = str(destination / "bin" / "xmljson-viewer")
launcher = Path("xmljson-viewer.desktop").read_text()
launcher = "\n".join(
    'Exec="' + binary + '" %F' if line.startswith("Exec=") else
    "Icon=" + str(destination / "visor-mascot.png") if line.startswith("Icon=") else
    line for line in launcher.splitlines()
) + "\n"
(destination / "xmljson-viewer.desktop").write_text(launcher)
PY
printf '%s\n' "Installed: $destination/bin/xmljson-viewer" \
    "Launcher: $destination/xmljson-viewer.desktop" \
    "No file associations or existing application files were changed."
