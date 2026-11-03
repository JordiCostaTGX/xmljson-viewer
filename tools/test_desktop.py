#!/usr/bin/env python3
"""Test real keyboard shortcuts and D-Bus single-instance in an isolated display."""
from pathlib import Path
import subprocess
import sys
import tempfile
import time


def wait_until(check, message, seconds=10):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        if check():
            return
        time.sleep(0.05)
    raise AssertionError(message)


def xdo(*args):
    return subprocess.run(["xdotool", *args], capture_output=True, text=True, check=True).stdout.strip()


def windows():
    result = subprocess.run(["xdotool", "search", "--name", "^XML/JSON Viewer Rust"],
                            capture_output=True, text=True)
    if result.returncode not in (0, 1):
        raise RuntimeError(result.stderr)
    return result.stdout.split()


def main(binary):
    from PyQt5.QtWidgets import QApplication
    qt = QApplication([])

    def tree_rows(window):
        qt.processEvents()
        image = qt.primaryScreen().grabWindow(int(window)).toImage()
        rows = []
        for y in range(60, min(image.height() - 80, 400)):
            ink = any(
                all(component > 140 for component in (
                    image.pixelColor(x, y).red(),
                    image.pixelColor(x, y).green(),
                    image.pixelColor(x, y).blue(),
                ))
                for x in range(35, 200)
            )
            if ink and (not rows or y > rows[-1][-1] + 1):
                rows.append([y])
            elif ink:
                rows[-1].append(y)
        return rows

    def tree_has_colors(window, colors):
        qt.processEvents()
        image = qt.primaryScreen().grabWindow(int(window)).toImage()
        found = set()
        for y in range(60, image.height() - 80):
            for x in range(20, min(300, image.width())):
                color = image.pixelColor(x, y).name()
                if color in colors:
                    found.add(color)
        return found == colors

    with tempfile.TemporaryDirectory(prefix="xmljson-desktop-test-") as folder:
        paths = [Path(folder) / f"{name}.json" for name in ("one", "two", "three")]
        for path in paths:
            path.write_text('{"original":true}', encoding="utf-8")
        with tempfile.TemporaryFile(mode="w+") as log:
            process = subprocess.Popen([str(binary), str(paths[0])], stdout=log, stderr=log)
            try:
                wait_until(lambda: len(windows()) == 1, "Window did not appear")
                window = windows()[0]
                wait_until(lambda: "one.json" in xdo("getwindowname", window), "First file did not open")
                secondary = subprocess.run([str(binary), str(paths[1]), str(paths[2])],
                                           capture_output=True, text=True, timeout=10)
                assert secondary.returncode == 0, secondary.stderr
                wait_until(lambda: "three.json" in xdo("getwindowname", window), "D-Bus did not open the files")
                assert windows() == [window], "More than one instance/window exists"
                owner = subprocess.run(
                    ["gdbus", "call", "--session", "--dest", "org.freedesktop.DBus",
                     "--object-path", "/org/freedesktop/DBus", "--method",
                     "org.freedesktop.DBus.GetNameOwner", "io.github.xmljsonviewer.Viewer"],
                    capture_output=True, text=True, check=True,
                )
                assert owner.stdout.strip(), "Neutral application ID was not registered"
                wait_until(lambda: tree_has_colors(window, {"#9cdcfe", "#82b8ef"}),
                           "Visible tree key/literal colors are missing")
                xdo("windowfocus", "--sync", window)
                xdo("key", "ctrl+a")
                xdo("type", "--clearmodifiers", '--delay', '1', '{"keyboard":[1,2]}')
                xdo("key", "ctrl+i")
                time.sleep(0.5)
                xdo("key", "ctrl+s")
                wait_until(lambda: paths[2].read_text() == '{\n  "keyboard": [\n    1,\n    2\n  ]\n}',
                           "Ctrl+I or Ctrl+S failed with the editor focused")
                time.sleep(0.5)
                rows = tree_rows(window)
                assert len(rows) >= 2, "Tree rows are missing"
                root_y = sum(rows[0]) // len(rows[0])
                xdo("mousemove", "--window", window, "80", str(root_y))
                xdo("click", "1")
                wait_until(lambda: len(tree_rows(window)) < len(rows), "Clicking text did not collapse the tree")
                xdo("click", "1")
                wait_until(lambda: len(tree_rows(window)) >= len(rows), "Clicking text did not expand the tree")
                # Put the cursor on the array key, then use the actual shortcut.
                xdo("mousemove", "--window", window, "600", "150")
                xdo("click", "1")
                xdo("key", "ctrl+Home", "Down", "Home", "Right", "Right", "ctrl+l")
                wait_until(lambda: len(tree_rows(window)) > len(rows), "Ctrl+L did not expand the key's node")
                wait_until(lambda: tree_has_colors(window, {"#9cdcfe", "#b5cea8", "#253247"}),
                           "Expanded tree number colors or selection background are missing")
                xdo("key", "ctrl+t")
                wait_until(lambda: "Untitled" in xdo("getwindowname", window), "Ctrl+T did not create a tab")
                xdo("key", "ctrl+w")
                wait_until(lambda: "three.json" in xdo("getwindowname", window), "Ctrl+W did not close the tab")
                xdo("key", "ctrl+q")
                assert process.wait(timeout=5) == 0
                print("DESKTOP_OK keyboard_ctrl_i_s_t_w_q_l text_click_expands_collapses single_instance multi_file tree_colors_selection")
            finally:
                if process.poll() is None:
                    process.terminate()
                    try:
                        process.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        process.kill()
                        process.wait()
                if process.returncode not in (0, -15):
                    log.seek(0)
                    print(log.read(), file=sys.stderr)


if __name__ == "__main__":
    main(Path(sys.argv[1] if len(sys.argv) > 1 else "target/release/xmljson-viewer-rust").resolve())
