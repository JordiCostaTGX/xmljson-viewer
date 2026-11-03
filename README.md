# XML/JSON Viewer Rust

A standalone XML and JSON editor written in Rust, using GTK4 and
GtkSourceView 5. No browser runtime is embedded. Python is only needed for
the installer and GUI test helpers, not to run the editor.

## Linux build and installation

The tested release platform is Linux x86-64. Build from source with Rust, a
C toolchain, `pkg-config`, GTK **4.12 or newer**, GtkSourceView **5**, and
`glib-compile-resources` (GLib development tools).

On Debian or Ubuntu, install the development packages:

```sh
sudo apt update
sudo apt install build-essential pkg-config libgtk-4-dev \
  libgtksourceview-5-dev libglib2.0-bin
```

Check that your distribution provides GTK 4.12 or newer with
`pkg-config --modversion gtk4`. On Arch Linux, install the dependencies and
build:

```sh
sudo pacman -S --needed rust gtk4 gtksourceview5 pkgconf glib2
```

After installing the dependencies on either distribution, build and run:

```sh
cargo build --release --locked
./target/release/xmljson-viewer-rust
./target/release/xmljson-viewer-rust example.xml example.json
```

To install the built application in your user data directory and generate a
desktop launcher, run `sh install.sh` from the project directory. See
[Installation](#installation) for the destination and launcher details.

## Windows build

Windows builds are **experimental and untested**; no prebuilt Windows
package or installer is provided. The following uses the MSYS2 MinGW-w64
GNU toolchain. Do not mix it with the MSVC Rust toolchain.

1. Install [MSYS2](https://www.msys2.org/), then open the **MSYS2 MINGW64**
   terminal from the Start menu.
2. Update MSYS2, reopening the MINGW64 terminal and repeating the update if
   prompted, then install the build tools and GTK dependencies:

   ```sh
   pacman -Syu
   ```

   ```sh
   pacman -S --needed mingw-w64-x86_64-gcc mingw-w64-x86_64-pkgconf \
     mingw-w64-x86_64-gtk4 mingw-w64-x86_64-gtksourceview5
   ```

3. Install Rust with rustup if it is not already installed. In the MINGW64
   terminal, select the GNU toolchain:

   ```sh
   rustup toolchain install stable-gnu
   rustup default stable-gnu
   ```

4. In that same terminal, change to the project directory and build and run:

   ```sh
   cd /c/path/to/xmljson-viewer
   cargo build --release --locked
   ./target/release/xmljson-viewer-rust.exe
   ```

   The MINGW64 terminal supplies the GTK runtime DLLs through its `PATH`.
   Launch the application from that terminal; copying only the `.exe` to
   another Windows machine is not a complete installation. The Linux
   `install.sh`, `.desktop` launcher, and measurement helpers do not apply
   to Windows.

## Features

- Independent tabs, undo/redo, line numbers, syntax colors, cursor position,
  and resizable tree/editor panes.
- Open multiple files through the menu, command-line arguments, or
  drag-and-drop. Further launches open files in the existing instance using
  `GApplication` and the session D-Bus.
- Click a tree item to select its source text. `Ctrl+E` selects the element
  at the cursor and reveals its tree path. `Ctrl+L` also expands that node
  and scrolls it into view. Clicking the node's text toggles expansion,
  as does its arrow.
- Source ranges handle repeated keys, repeated XML names, array indices,
  numeric object keys, and Unicode.
- Tree labels distinguish keys, strings, numbers, literals, and XML
  attributes with a dark high-contrast palette. Hover and selection stay
  readable, and recycled rows have their old styling cleared.
- Two-space formatting and explicit XML/JSON parsing.
- Decode selected escaped content, including XML/JSON `auditData` wrappers,
  HTML entities, and escaped JSON strings.
- Literal case-sensitive search, highlighted matches, an incremental
  match count, and next/previous navigation with wraparound.
- Save/Save as, dirty indicators, and confirmation before discarding edits.
  GIO safely replaces file contents in the background. Edits made after a
  save starts remain marked as unsaved.

| Shortcut | Action |
|---|---|
| Ctrl+T / Ctrl+W | New tab / close tab |
| Ctrl+O | Open |
| Ctrl+S / Ctrl+Shift+S | Save / Save as |
| Ctrl+I | Format document |
| Ctrl+E | Select element and reveal its tree path |
| Ctrl+L | Locate, select, and expand the cursor's node |
| Ctrl+F / Escape | Show / close search |
| Enter in search, F3 / Shift+F3 | Next / previous match |
| Ctrl+A / Ctrl+C / Ctrl+X | Select all / copy / cut, including hidden text |
| Ctrl+Z / Ctrl+Shift+Z | Undo / redo |
| Ctrl+Q | Quit |

## Large documents and limitations

Documents must be UTF-8 and are held in memory in full. Keystrokes do not
copy or parse the document. After **350 ms** without edits, a snapshot is
analyzed by a single background worker. Obsolete results are cancelled or
discarded using each tab's revision. This is a **full parse after a pause**,
not an incremental parser. Inserting or replacing large buffers still has
a GUI-thread cost; multi-GB files and completely pause-free loading are
not promised.

**Extreme JSON lines:** a valid JSON file containing a line longer than
**16 KB** is automatically formatted when opened. The status bar explains
the change, and the tab is marked modified. **Nothing is written to disk
until Save.** Duplicate keys, value contents, ordering, and number tokens
are preserved.

JSON lines longer than **2,048 characters** are displayed in **512-character
chunks**. A visible bar explains the hidden text and provides Previous/Next
chunk buttons. Cursor movement and search reveal the appropriate chunk
automatically. Hidden characters remain in the editor buffer: selecting,
copying, cutting, and saving include the complete source. Strings are not
truncated or changed by the chunked presentation.

JSON uses a linear Rust lexer and materializes syntax tags only for the
visible area, refreshing them on scroll. This avoids the native regex
engine's two-second-per-line timeout and the cost of thousands of GUI tags
on one extremely long line. Incomplete JSON edits retain lexical colors
even when parsing fails. XML uses GtkSourceView's native highlighter.

The tree creates rows on demand. Branches with more than **512 children**
are grouped into expandable blocks. Selection shortcuts open the relevant
block automatically. Only the first root is expanded on initial load.

XML supports multiple-root fragments and undeclared prefixes; it does not
validate schemas or resolve custom/external DTD entities. JSON uses
`serde_json`'s depth limit (128); XML is limited to 512 levels. The safety
limit is two million nodes. Parse errors appear in the status bar; a stale
tree is disabled until it matches the current text.

XML formatting preserves comments, CDATA, declarations, and prefixes.
Documents containing mixed text and elements are left unchanged rather
than introducing whitespace that changes their values. Decode handles
common escaping layers without Python's `unicode_escape` transformation,
which can corrupt Unicode.

## Installation

```sh
sh install.sh
```

For a prebuilt Linux archive, extract it and run the same command from the
extracted directory. The installer accepts either a source build in
`target/release` or a prebuilt executable in `bin`.

The default destination is `$XDG_DATA_HOME/xmljson-viewer-rust`, or
`$HOME/.local/share/xmljson-viewer-rust` when `XDG_DATA_HOME` is unset.
An absolute destination may be supplied explicitly:

```sh
sh install.sh "$PWD/dist"
```

Spaces are supported in installation paths. Percent signs, backslashes,
double quotes, dollar signs, backticks, and control characters are rejected
explicitly because they do not work reliably in desktop launcher executable
paths. Use a different absolute destination if needed.

The installer copies the release to `bin/xmljson-viewer-rust`, installs the
README and icon, and generates a `.desktop` file with absolute `Exec` and
`Icon` paths for that destination. It refuses to overwrite an already
installed binary. It does not register MIME defaults or change existing
applications. The source `.desktop` is a path-independent template; use
the generated one for launching an installation.

Copy the generated `xmljson-viewer-rust.desktop` to your desktop or
`$XDG_DATA_HOME/applications` (default `$HOME/.local/share/applications`)
to make it discoverable. Your file manager may ask you to allow launching
the desktop shortcut.

The application ID is **`io.github.xmljsonviewer.Viewer`**. The icon is
embedded through `build.rs` and `resources/viewer.gresource.xml`, and is
also installed as a PNG for desktop launchers. See
`resources/icon-provenance.txt` for its provenance and licensing caveat.
No third-party license for the supplied image has been independently
verified; embedding it does not establish rights to redistribute it.
A project license has not been selected yet. Choose a license and confirm
the image's redistribution rights before publishing.

To copy project sources elsewhere, run this from the project root:

```sh
mkdir -p /absolute/path/to/source-copy
rsync -a --exclude=target/ --exclude=dist/ --exclude=.git/ \
  --exclude=bench-data/ --exclude=reference/ --exclude=__pycache__/ \
  --exclude='*.pyc' ./ /absolute/path/to/source-copy/
```

Source copies are independent from the installed binary. Build artifacts,
temporary documents, and repository metadata should not be published.

## Validation

```sh
cargo test --locked
cargo fmt --check
cargo clippy --locked --all-targets
desktop-file-validate xmljson-viewer-rust.desktop
GTK_A11Y=none GDK_BACKEND=x11 GSK_RENDERER=cairo \
  dbus-run-session -- xvfb-run -a \
  ./target/release/xmljson-viewer-rust --ui-smoke
GTK_A11Y=none QT_QPA_PLATFORM=xcb GDK_BACKEND=x11 GSK_RENDERER=cairo \
  dbus-run-session -- xvfb-run -a python3 tools/test_desktop.py
```

GUI test helpers require Xvfb, `xdotool`, D-Bus utilities, Python 3, and
PyQt5. Smoke tests cover tabs, Unicode source ranges, lazy groups, search,
coalescing 100 edits, Decode, undo/redo, safe saving, and the embedded icon.
The desktop test sends actual shortcuts and mouse clicks and checks the
neutral D-Bus ID and multi-file single-instance behavior.

For a read-only check with your own long-line JSON input:

```sh
GTK_A11Y=none QT_QPA_PLATFORM=xcb GDK_BACKEND=x11 GSK_RENDERER=cairo \
  dbus-run-session -- xvfb-run -a \
  python3 tools/test_real.py target/release/xmljson-viewer-rust /path/to/input.json
```

This helper checks visible colors using screenshots in memory, searching,
copying hidden text, and the input file's unchanged hash. It does not
save screenshots or distribute the input. Run it only in an isolated
session/display, not against an existing user window.

## Reproducible measurements

```sh
python3 tools/benchmark.py --generate bench-data --rows 20000
./target/release/xmljson-viewer-rust --benchmark bench-data/sample.json 5
GTK_A11Y=none GDK_BACKEND=x11 GSK_RENDERER=cairo \
  dbus-run-session -- xvfb-run -a \
  ./target/release/xmljson-viewer-rust --gui-benchmark bench-data/sample.json
```

The parse-only helper reports a mean over the requested iterations. The
GUI helper waits for source, tree rows, and visible syntax colors, then
adds **500 ms of stabilization** before checking. It is not a cold-start
benchmark or a claim of complete repaint latency. Linux peak resident
memory and the largest timer interval are also reported.

Synthetic fixtures can be generated locally; no user documents are
included. Results depend on hardware, rendering backend, document shape,
and expansion state. Re-run these commands on representative documents
rather than treating a single measurement as a universal speed guarantee.
