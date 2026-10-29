#!/usr/bin/env python3
"""Validate app/ui/radio-guides.json (run in CI).

Checks structure, unique ids, allowed status/kind/source values, https-only
links, ISO dates, and that every report says where it came from.
"""
import datetime, json, pathlib, re, sys

P = pathlib.Path(__file__).resolve().parents[2] / "app" / "ui" / "radio-guides.json"
STATUS = {"documented", "reported", "tested"}
KINDS = {"manual", "video", "post", "forum", "other"}
SOURCES = {"groups.io", "facebook", "email", "forum", "qrz", "reddit", "in person", "test", "other"}

def fail(msg):
    sys.exit(f"radio-guides.json: {msg}")

def date(s, where):
    try:
        datetime.date.fromisoformat(s)
    except (TypeError, ValueError):
        fail(f"{where}: date must be YYYY-MM-DD, got {s!r}")

def link(l, where):
    for k in ("title", "url", "kind"):
        if not l.get(k):
            fail(f"{where}: link needs {k}")
    if not re.match(r"^https://[^\s/]+\.[^\s/]+", l["url"]):
        fail(f"{where}: link must be https: {l['url']}")
    if l["kind"] not in KINDS:
        fail(f"{where}: link kind {l['kind']!r} not in {sorted(KINDS)}")

def main():
    d = json.loads(P.read_text())
    date(d.get("updated"), "updated")
    if set(d.get("statusLevels", {})) != STATUS:
        fail("statusLevels must define exactly " + ", ".join(sorted(STATUS)))
    ids = set()
    n_radios = n_reports = 0
    for b in d["brands"]:
        if not b.get("id") or not b.get("name"):
            fail("brand needs id and name")
        for r in b["radios"]:
            where = f"{b['name']} {r.get('name')}"
            for k in ("id", "name", "status", "summary", "slowData", "fastData", "connection", "steps", "links", "reports"):
                if k not in r:
                    fail(f"{where}: missing {k}")
            if r["id"] in ids:
                fail(f"duplicate radio id {r['id']}")
            ids.add(r["id"])
            if r["status"] not in STATUS:
                fail(f"{where}: status {r['status']!r}")
            for s in r["steps"]:
                if not s.get("title") or not isinstance(s.get("items"), list) or not s["items"]:
                    fail(f"{where}: each step group needs a title and items")
            for l in r["links"]:
                link(l, where)
            for i, rep in enumerate(r["reports"]):
                w = f"{where} report {i + 1}"
                for k in ("date", "source", "reporter", "summary"):
                    if not rep.get(k):
                        fail(f"{w}: missing {k}")
                date(rep["date"], w)
                if rep["source"] not in SOURCES:
                    fail(f"{w}: source {rep['source']!r} not in {sorted(SOURCES)}")
                if rep.get("url"):
                    link({"title": "report", "url": rep["url"], "kind": "post"}, w)
                for k in ("worked", "problems"):
                    if k in rep and not isinstance(rep[k], list):
                        fail(f"{w}: {k} must be a list")
            if r["status"] == "reported" and not r["reports"]:
                fail(f"{where}: status 'reported' needs at least one report")
            n_radios += 1
            n_reports += len(r["reports"])
    print(f"radio-guides.json ok: {len(d['brands'])} brands, {n_radios} radios, {n_reports} reports")

if __name__ == "__main__":
    main()
