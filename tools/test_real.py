#!/usr/bin/env python3
"""Inspect visible colors and chunk navigation using a real, read-only document."""
import hashlib
from pathlib import Path
import subprocess
import sys
import tempfile
import time
from PyQt5.QtWidgets import QApplication

from test_desktop import wait_until, windows, xdo


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main(binary, document):
    before = digest(document)
    qt = QApplication([])
    with tempfile.TemporaryFile(mode="w+") as log:
        process = subprocess.Popen([str(binary), str(document)], stdout=log, stderr=log)
        try:
            wait_until(lambda: len(windows()) == 1, "Window did not appear")
            window = windows()[0]
            palette = {(156, 220, 254), (206, 145, 120), (181, 206, 168), (86, 156, 214)}

            def pixels():
                qt.processEvents()
                image = qt.primaryScreen().grabWindow(int(window)).toImage()
                count = 0
                for y in range(80, min(image.height() - 70, 720)):
                    for x in range(330, min(image.width(), 1200)):
                        color = image.pixelColor(x, y)
                        if (color.red(), color.green(), color.blue()) in palette:
                            count += 1
                return count

            started = time.monotonic()
            wait_until(lambda: pixels() > 20, "No visible syntax colors in the input JSON", seconds=15)
            print(f"REAL_VISIBLE_OK colored_pixels={pixels()} ready_ms={(time.monotonic()-started)*1000:.1f}")
            xdo("windowfocus", "--sync", window)
            # The extreme string is selected through search, without printing it.
            import json
            data = json.loads(document.read_text())
            strings = []

            def visit(value):
                if isinstance(value, str):
                    strings.append(value)
                elif isinstance(value, dict):
                    for item in value.values():
                        visit(item)
                elif isinstance(value, list):
                    for item in value:
                        visit(item)

            visit(data)
            longest = max(strings, key=len)
            query = json.dumps(longest, ensure_ascii=False)[1:33]
            xdo("key", "ctrl+f")
            xdo("type", "--clearmodifiers", "--delay", "1", query)
            time.sleep(0.3)
            xdo("key", "Return", "Escape")
            time.sleep(0.3)
            # A searched long value must still have visible colored glyphs.
            assert pixels() > 20
            print("REAL_CHUNK_SEARCH_OK visible_colors_after_jump")
            xdo("key", "ctrl+a", "ctrl+c")
            wait_until(lambda: bool(qt.clipboard().text()), "Could not copy the document")
            copied = qt.clipboard().text()
            assert json.loads(copied) == data, "Copy must include hidden chunks"
            print("REAL_COPY_OK full_hidden_text_preserved")
        finally:
            # This is our isolated test process, not a user's live window.
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()
    assert digest(document) == before, "The input file was modified"
    print("REAL_SOURCE_UNCHANGED")


if __name__ == "__main__":
    main(Path(sys.argv[1]).resolve(), Path(sys.argv[2]).resolve())
