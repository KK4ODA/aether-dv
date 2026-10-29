#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Assemble mockup pages: shared frame (brand, transport strip, rail, drawer)
+ per-page <main> and CSS from design/mockups/src/."""
import pathlib

SRC = pathlib.Path(__file__).resolve().parent.parent / "mockups" / "src"
OUT = SRC.parent
PAGES = {  # page -> (title, rail link href that is current)
    "chat": ("Chat", "chat.html"),
    "message": ("Message", "message.html"),
    "connections": ("Connections", "connections.html"),
    "transfers": ("Transfers", "transfers.html"),
}
top = (SRC / "_frame_top.html").read_text()
bottom = (SRC / "_frame_bottom.html").read_text()
for page, (title, href) in PAGES.items():
    main_f, css_f = SRC / f"{page}.main.html", SRC / f"{page}.css"
    if not main_f.exists():
        continue
    frame = top.replace(f'<a href="{href}">', f'<a href="{href}" aria-current="page">', 1)
    html = f"""<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Aether DV — {title} (mockup)</title>
<link rel="stylesheet" href="app.css">
<style>
{css_f.read_text() if css_f.exists() else ''}</style>
</head>
<body>
{frame}{main_f.read_text()}{bottom}</body>
</html>
"""
    (OUT / f"{page}.html").write_text(html)
    print("built", page)
