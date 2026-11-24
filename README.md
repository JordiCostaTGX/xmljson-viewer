# XML/JSON Viewer

A standalone XML and JSON editor written in Rust with
[egui](https://github.com/emilk/egui). It runs on **Windows, Linux and
macOS** from a single code base and needs no GTK, Qt or browser runtime:
the executable is self-contained. Python is only needed for the Linux
installer and the fixture generator, not to run the editor.

## Download (Windows, Linux, macOS)

Every tagged release (`v*`) is built by GitHub Actions and published on the
repository's **Releases** page:

- **Windows**: download `xmljson-viewer-windows-x86_64.zip`, extract it
  and run `xmljson-viewer.exe`. Nothing else has to be installed.
  Files can be opened from the menu, by drag-and-drop, or with
  _Open with…_ in Explorer.
- **Linux**: download `xmljson-viewer-linux-x86_64.tar.gz`, extract it
  and run `sh install.sh` from the extracted directory (see
  [Linux installation](#linux-installation)).
- **macOS** (Apple silicon): download
  `xmljson-viewer-macos-arm64.tar.gz` and run `bin/xmljson-viewer`.

Builds for every push and pull request are available as workflow artifacts.

## Building from source

Install Rust with [rustup](https://rustup.rs/), then:

```sh
cargo build --release --locked
./target/release/xmljson-viewer
./target/release/xmljson-viewer example.xml example.json
```

- **Windows**: use the default MSVC toolchain that rustup installs (it
  asks for the Visual Studio C++ Build Tools). No MSYS2 or GTK is needed.
  If you use the GNU toolchain instead, `dlltool.exe` from a MinGW-w64
  distribution must be on `PATH`.
- **Linux**: a C toolchain plus fontconfig headers are needed to build
  (`sudo apt install build-essential pkg-config libfontconfig1-dev` on
  Debian/Ubuntu, `sudo pacman -S --needed base-devel fontconfig` on Arch).
  At runtime the usual desktop libraries (X11 or Wayland, xkbcommon and a
  Vulkan or OpenGL driver) are used.
- **macOS**: Xcode command line tools.

Release builds on Windows do not open a console window.

## Startup diagnostics

Each GUI launch overwrites the single `startup.log` file in
`%LOCALAPPDATA%\xmljson-viewer\logs` on Windows. If that directory is
unavailable, the application uses `xmljson-viewer/logs` inside the system
temporary directory (also the default on Linux and macOS).

The log records the application version, OS, architecture, process ID and
startup stages, including single-instance communication and graphics/window
initialization. Startup errors and Rust panics automatically open the log
in Notepad on Windows (the default file viewer on Linux and macOS) and
display an error dialog with the error and log path. If opening the log
fails, the dialog reports that too. Panic logs also include a backtrace.
When reporting a launch failure, attach `startup.log` and the dialog text
before launching the application again, since the next launch clears the
log. Older `startup-*.log` files from previous versions can be deleted
manually. File contents and
command-line paths are not logged routinely, but error or panic messages
can contain paths or other diagnostic details; review logs before sharing.

If Windows blocks execution before the application starts, or a required
DLL cannot be loaded, no application log or dialog may be produced. Check
Windows Security protection history and Reliability Monitor (`perfmon /rel`)
in that case. These diagnostics do not sign the executable or remove
SmartScreen warnings. Benchmark mode does not create startup logs or dialogs.

## Features

- Independent tabs, undo/redo, line numbers, syntax colors (JSON and XML),
  cursor position, and a resizable tree panel.
- Open multiple files through the menu, command-line arguments, or
  drag-and-drop.
- Click a tree item to select its source text. `Ctrl+E` selects the element
  at the cursor and reveals its tree path. `Ctrl+L` also expands that node
  and scrolls it into view. Clicking the node's text toggles expansion,
  as does its arrow. The tree also supports arrow keys and Enter.
- Source ranges handle repeated keys, repeated XML names, array indices,
  numeric object keys, Unicode, CRLF line endings and a UTF-8 byte order
  mark.
- Tree labels distinguish keys, strings, numbers, literals, and XML
  attributes with a dark high-contrast palette.
- Two-space formatting and explicit XML/JSON parsing.
- Decode selected escaped content, including XML/JSON `auditData` wrappers,
  HTML entities, and escaped JSON strings.
- Right-click a JSON string and choose **Open as XML**, or right-click an XML
  text or CDATA node and choose **Open as JSON** for embedded objects and arrays.
  The decoded payload opens in a new tab without changing the source document.
- Literal case-sensitive search, highlighted matches, a match count, and
  next/previous navigation with wraparound.
- Save/Save as, dirty indicators, and confirmation before discarding edits.
  Saving writes a temporary file next to the target and renames it over
  the original in the background. Edits made after a save starts remain
  marked as unsaved.

| Shortcut                          | Action                                       |
| --------------------------------- | -------------------------------------------- |
| Ctrl+T / Ctrl+W                   | New tab / close tab                          |
| Ctrl+O                            | Open                                         |
| Ctrl+S / Ctrl+Shift+S             | Save / Save as                               |
| Ctrl+I                            | Format document                              |
| Ctrl+E                            | Select element and reveal its tree path      |
| Ctrl+L                            | Locate, select, and expand the cursor's node |
| Ctrl+F / Escape                   | Show / close search                          |
| Enter in search, F3 / Shift+F3    | Next / previous match                        |
| Ctrl+A / Ctrl+C / Ctrl+X / Ctrl+V | Select all / copy / cut / paste              |
| Ctrl+Z / Ctrl+Shift+Z (or Ctrl+Y) | Undo / redo                                  |
| Ctrl+Q                            | Quit                                         |

On macOS, Cmd replaces Ctrl.

## Large documents and limitations

Documents must be UTF-8 and are held in memory in full, in a rope so that
edits do not copy the document. Keystrokes do not parse the document.
After **350 ms** without edits, a snapshot is analyzed by a single
background worker. Obsolete results are cancelled or discarded using each
tab's revision. This is a **full parse after a pause**, not an incremental
parser.

The editor only lays out and colors the lines that are visible. Lines
longer than **4,096 characters** are additionally laid out in horizontal
windows around the viewport, so extremely long single-line JSON can be
opened, scrolled and edited without reformatting it or hiding any text.
XML highlighting carries its lexer state across lines (comments, CDATA,
multi-line tags) and recomputes it only from the edited line onwards.

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

Each launch opens its own window; files are not forwarded to an already
running instance.

## Linux installation

```sh
sh install.sh
```

For a prebuilt Linux archive, extract it and run the same command from the
extracted directory. The installer accepts either a source build in
`target/release` or a prebuilt executable in `bin`.

The default destination is `$XDG_DATA_HOME/xmljson-viewer`, or
`$HOME/.local/share/xmljson-viewer` when `XDG_DATA_HOME` is unset.
An absolute destination may be supplied explicitly:

```sh
sh install.sh "$PWD/dist"
```

Spaces are supported in installation paths. Percent signs, backslashes,
double quotes, dollar signs, backticks, and control characters are rejected
explicitly because they do not work reliably in desktop launcher executable
paths. Use a different absolute destination if needed.

The installer copies the release to `bin/xmljson-viewer`, installs the
README and icon, and generates a `.desktop` file with absolute `Exec` and
`Icon` paths for that destination. It refuses to overwrite an already
installed binary. It does not register MIME defaults or change existing
applications. The source `.desktop` is a path-independent template; use
the generated one for launching an installation.

Copy the generated `xmljson-viewer.desktop` to your desktop or
`$XDG_DATA_HOME/applications` (default `$HOME/.local/share/applications`)
to make it discoverable.

The application ID is **`io.github.xmljsonviewer.Viewer`**. The icon is
embedded in the Windows executable and also installed as a PNG for desktop
launchers. See `resources/icon-provenance.txt` for its provenance and
licensing caveat. No third-party license for the supplied image has been
independently verified; embedding it does not establish rights to
redistribute it. A project license has not been selected yet. Choose a
license and confirm the image's redistribution rights before publishing.

## Validation

```sh
cargo test --locked
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
```

The tests need no display. Besides the parser, lexer and editor unit
tests, they drive the application logic headlessly: tabs, Unicode source
ranges, lazy groups, search, coalescing 100 edits into one parse, Decode,
undo/redo, safe saving, close confirmation, and rendering complete frames
of a large XML document with a long line.

## Reproducible measurements

```sh
python3 tools/benchmark.py --generate bench-data --rows 20000
./target/release/xmljson-viewer --benchmark bench-data/sample.json 5
```

The parse-only helper reports a mean over the requested iterations and,
on Linux, the peak resident memory. On Windows, run it from a debug build
(`cargo run -- --benchmark …`) because release builds have no console.
Synthetic fixtures can be generated locally; no user documents are
included. Results depend on hardware, document shape and expansion state.
