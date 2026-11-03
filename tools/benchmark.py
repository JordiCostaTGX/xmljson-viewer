#!/usr/bin/env python3
"""Generate deterministic, non-sensitive fixtures or measure the original GUI."""
import argparse
import importlib.util
import json
from pathlib import Path
import resource
import sys
import time


def generate(directory, rows):
    directory.mkdir(parents=True, exist_ok=True)
    data = [{"id": i, "name": f"Item {i}", "active": True, "price": 123.45} for i in range(rows)]
    (directory / "sample.json").write_text(json.dumps({"items": data}, indent=2), encoding="utf-8")
    xml = ['<?xml version="1.0" encoding="UTF-8"?>', "<items>"]
    for i in range(rows):
        xml.extend([f'  <item id="{i}">', f"    <name>Item {i}</name>", "    <price>123.45</price>", "  </item>"])
    xml.append("</items>")
    (directory / "sample.xml").write_text("\n".join(xml), encoding="utf-8")


def original(script, fixture):
    print("original: importing", file=sys.stderr, flush=True)
    from PyQt5.QtCore import QTimer
    from PyQt5.QtWidgets import QApplication
    spec = importlib.util.spec_from_file_location("original_viewer", script)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    app = QApplication([])
    print("original: creating window", file=sys.stderr, flush=True)
    viewer = module.XMLJSONViewer()
    print("original: showing window", file=sys.stderr, flush=True)
    viewer.show()
    app.processEvents()
    last = time.perf_counter()
    maximum_gap = 0
    started = None
    timer = QTimer()

    def tick():
        nonlocal last, maximum_gap
        now = time.perf_counter()
        maximum_gap = max(maximum_gap, now - last)
        last = now

    timer.timeout.connect(tick)
    timer.start(10)

    def finish():
        elapsed = (time.perf_counter() - started) * 1000
        from PyQt5.QtWidgets import QTreeWidgetItemIterator
        iterator = QTreeWidgetItemIterator(viewer.tree)
        count = 0
        while iterator.value():
            count += 1
            iterator += 1
        print(f"original_gui_ms={elapsed:.3f} nodes={count} max_tick_gap_ms={maximum_gap * 1000:.3f} "
              f"peak_rss_kib={resource.getrusage(resource.RUSAGE_SELF).ru_maxrss}", flush=True)
        app.quit()

    def load():
        nonlocal started, last
        started = last = time.perf_counter()
        print("original: loading document", file=sys.stderr, flush=True)
        viewer.load_file(str(fixture))
        print("original: load returned", file=sys.stderr, flush=True)
        # Allow a paint/event-loop turn before measuring readiness.
        QTimer.singleShot(20, finish)

    QTimer.singleShot(20, load)
    app.exec_()


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--generate", type=Path)
    parser.add_argument("--rows", type=int, default=20000)
    parser.add_argument("--original", type=Path)
    parser.add_argument("--file", type=Path)
    args = parser.parse_args()
    if args.generate:
        if args.rows < 1:
            parser.error("--rows must be positive")
        generate(args.generate, args.rows)
    elif args.original and args.file:
        original(args.original, args.file)
    else:
        parser.error("Use --generate DIRECTORY or --original SCRIPT --file FILE")
