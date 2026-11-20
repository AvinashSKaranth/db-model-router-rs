#!/usr/bin/env python3
"""Run every quality check and write docs/report.html (+ docs/report-history.json).

Checks: fmt, clippy, tests (+ coverage via cargo-llvm-cov), doc tests, cargo-deny,
cargo-audit, static security lints, gitleaks, traceability, rustdoc.
A missing tool is reported as "skipped", never silently passed.

Usage:  python scripts/report.py [--offline] [--no-fail]
  default     live: every docker-compose service port must answer, else exit 1
  --offline   skip #[ignore] live-service tests (local runs only, not the hook)
  --no-fail   always exit 0 (default: exit 1 when a gate fails)
Used by .githooks/pre-push.
"""

import datetime
import html
import json
import os
import re
import shutil
import socket
import subprocess
import sys
import time

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))
OUT_DIR = os.path.join(ROOT, "target", "report")
REPORT = os.path.join(ROOT, "docs", "report.html")
HISTORY = os.path.join(ROOT, "docs", "report-history.json")
HISTORY_KEEP = 30
# docker-compose services the live tests need (host ports)
LIVE_PORTS = [5432, 26257, 3308, 3309, 1433, 1521, 27017, 6379, 8000, 9092]
COV_IGNORE = r"dbmr-bench|xtask|[\\/]tests[\\/]|[\\/]examples[\\/]"

os.makedirs(OUT_DIR, exist_ok=True)
ENV = dict(os.environ, CARGO_TERM_COLOR="never", PYTHONUTF8="1")


def run(cmd, timeout=3600):
    t = time.time()
    try:
        p = subprocess.Popen(cmd, cwd=ROOT, env=ENV, stdout=subprocess.PIPE,
                             stderr=subprocess.PIPE, text=True, encoding="utf-8",
                             errors="replace")
    except FileNotFoundError:
        return None, "", f"{cmd[0]} not found", 0.0
    try:
        out, err = p.communicate(timeout=timeout)
        return p.returncode, out, err, time.time() - t
    except subprocess.TimeoutExpired:
        # Kill the whole tree: a hung test binary is a grandchild that keeps the pipes open.
        if os.name == "nt":
            subprocess.run(["taskkill", "/T", "/F", "/PID", str(p.pid)], capture_output=True)
        else:
            p.kill()
        out, err = p.communicate()
        return 124, out, err + f" (timed out after {timeout}s)", time.time() - t


def have(tool, cargo_sub=False):
    if cargo_sub:
        return run(["cargo", tool, "--version"], 60)[0] == 0
    return shutil.which(tool) is not None


def gitleaks_bin():
    found = shutil.which("gitleaks")
    if not found:  # winget installs outside PATH until a new login session
        import glob
        hits = glob.glob(os.path.expandvars(
            r"%LOCALAPPDATA%\Microsoft\WinGet\Packages\Gitleaks*\gitleaks.exe"))
        found = hits[0] if hits else None
    return found


def port_open(port):
    with socket.socket() as s:
        s.settimeout(0.5)
        return s.connect_ex(("127.0.0.1", port)) == 0


def git(*args):
    rc, out, _, _ = run(["git", *args], 30)
    return out.strip() if rc == 0 else ""


# ------------------------------------------------------------------ parsers

RUNNING = re.compile(r"^\s*(?:Running (\S+) \((.+?)\)|Doc-tests (\S+))")
TEST_LINE = re.compile(r"^test (.+?) \.\.\. (ok|FAILED|ignored)(?:, (.*))?$")


def parse_tests(text, kind):
    """libtest console output -> list of test cases."""
    cases, suite, failures = [], "?", {}
    lines = text.splitlines()
    for i, line in enumerate(lines):
        m = RUNNING.match(line)
        if m:
            if m.group(3):
                suite = f"{m.group(3)} (doc)"
            else:
                exe = os.path.basename(m.group(2).replace("\\", "/"))
                crate = re.sub(r"-[0-9a-f]{8,}(\.exe)?$", "", exe)
                src = m.group(1).replace("\\", "/")
                suite = f"{crate} :: {src}"
            continue
        m = TEST_LINE.match(line.strip())
        if m:
            cases.append({"suite": suite, "name": m.group(1), "status": m.group(2),
                          "note": m.group(3) or "", "kind": kind})
        if line.startswith("---- ") and line.endswith(" stdout ----"):
            name = line[5:-12]
            body = []
            for nxt in lines[i + 1:]:
                if nxt.startswith("---- ") or nxt.strip() == "failures:":
                    break
                body.append(nxt)
            failures[name] = "\n".join(body).strip()
    for c in cases:
        if c["status"] == "FAILED":
            c["detail"] = failures.get(c["name"], "")
    return cases


def parse_clippy(stdout):
    msgs = []
    for line in stdout.splitlines():
        try:
            j = json.loads(line)
        except ValueError:
            continue
        if j.get("reason") != "compiler-message":
            continue
        m = j["message"]
        if m.get("level") not in ("warning", "error") or not m.get("spans"):
            continue
        sp = next((s for s in m["spans"] if s.get("is_primary")), m["spans"][0])
        key = (sp["file_name"], sp["line_start"], m["message"])
        if key in {(x["file"], x["line"], x["message"]) for x in msgs}:
            continue
        msgs.append({"level": m["level"], "file": sp["file_name"], "line": sp["line_start"],
                     "message": m["message"], "code": (m.get("code") or {}).get("code", "")})
    return msgs


def parse_cov(path):
    with open(path, encoding="utf-8") as f:
        data = json.load(f)["data"][0]
    files = []
    for fl in data["files"]:
        s = fl["summary"]
        name = os.path.relpath(fl["filename"], ROOT).replace("\\", "/")
        files.append({"file": name, "lines": s["lines"], "functions": s["functions"],
                      "regions": s["regions"]})
    files.sort(key=lambda x: x["lines"]["percent"])
    return data["totals"], files


# ------------------------------------------------------------------ steps

steps = []  # {key,title,status(pass|fail|skip|warn),summary,duration,log,gate}


def step(key, title, status, summary, duration=0.0, log="", gate=True, **extra):
    steps.append(dict(key=key, title=title, status=status, summary=summary,
                      duration=duration, log=log[-60000:], gate=gate, **extra))
    print(f"[{status.upper():4}] {title}: {summary}", flush=True)


def main():
    args = sys.argv[1:]
    live = "--offline" not in args
    down = [p for p in LIVE_PORTS if not port_open(p)] if live else []
    if down:  # live tests are mandatory unless --offline is given explicitly
        print(f"Live services not reachable on ports {down}. Start them with "
              "`docker compose up -d`, or pass --offline for a local-only run.")
        sys.exit(1)
    started = datetime.datetime.now().astimezone()

    # fmt
    rc, out, err, d = run(["cargo", "fmt", "--all", "--check"])
    files = sorted(set(re.findall(r"^Diff in (.+?)(?: at line| :)", out, re.M)))
    step("fmt", "Format (rustfmt)", "pass" if rc == 0 else "fail",
         "clean" if rc == 0 else f"{len(files) or 'some'} file(s) need formatting", d, out + err)

    # clippy
    rc, out, err, d = run(["cargo", "clippy", "--workspace", "--all-features", "--all-targets",
                           "--message-format=json", "--", "-D", "warnings"])
    msgs = parse_clippy(out)
    step("clippy", "Lint (clippy -D warnings)", "pass" if rc == 0 else "fail",
         "no warnings" if rc == 0 else f"{len(msgs)} finding(s)", d, err, items=msgs)

    # tests + coverage
    test_args = ["--", "--test-threads=1"] + (["--include-ignored"] if live else [])
    cov_json = os.path.join(OUT_DIR, "coverage.json")
    cov = None
    cov_why = "cargo-llvm-cov not installed (cargo install cargo-llvm-cov)"
    if have("llvm-cov", cargo_sub=True):
        if os.path.exists(cov_json):
            os.remove(cov_json)  # never report a previous run's numbers
        # --ignore-run-fail: still measure coverage when tests fail; the tests gate below
        # counts failures from the output.
        rc, out, err, d = run(["cargo", "llvm-cov", "--workspace", "--all-features", "--json",
                               "--summary-only", "--output-path", cov_json,
                               "--ignore-run-fail", "--ignore-filename-regex", COV_IGNORE,
                               *test_args])
        if os.path.exists(cov_json):
            cov = parse_cov(cov_json)
        else:
            cov_why = f"cargo llvm-cov produced no report (exit {rc}); see the tests log"
    else:
        rc, out, err, d = run(["cargo", "test", "--workspace", "--all-features", *test_args])
    cases = parse_tests(out + "\n" + err, "unit/integration")
    rc_doc, out_doc, err_doc, d_doc = run(["cargo", "test", "--workspace", "--all-features",
                                           "--doc"])
    cases += parse_tests(out_doc + "\n" + err_doc, "doc")
    n = {s: sum(c["status"] == s for c in cases) for s in ("ok", "FAILED", "ignored")}
    ok = rc == 0 and rc_doc == 0 and n["FAILED"] == 0 and cases
    mode = "live + offline" if live else "offline (live-service tests ignored)"
    step("tests", f"Tests ({mode})", "pass" if ok else "fail",
         f"{n['ok']} passed, {n['FAILED']} failed, {n['ignored']} ignored", d + d_doc,
         (out + err)[-30000:] + "\n" + (out_doc + err_doc)[-10000:], cases=cases)

    target = 95.0
    if cov:
        pct = cov[0]["lines"]["percent"]
        step("coverage", "Coverage (cargo-llvm-cov, lines)",
             "pass" if pct >= target else "warn",
             f"{pct:.1f}% lines (target {target:.0f}%)", 0, gate=False,
             totals=cov[0], files=cov[1])
    else:
        step("coverage", "Coverage (cargo-llvm-cov)", "skip", cov_why, gate=False)

    # dependency policy + advisories
    if have("deny", cargo_sub=True):
        rc, out, err, d = run(["cargo", "deny", "--all-features", "check"])
        errs = re.findall(r"^error\[(.+?)\]: (.+)$", err, re.M)
        warns = re.findall(r"^warning\[(.+?)\]: (.+)$", err, re.M)
        step("deny", "Dependency policy (cargo-deny: advisories, licenses, bans, sources)",
             "pass" if rc == 0 else "fail",
             f"{len(errs)} error(s), {len(warns)} warning(s)", d, err,
             items=[{"level": "error", "code": c, "message": m} for c, m in errs]
             + [{"level": "warning", "code": c, "message": m} for c, m in warns])
    else:
        step("deny", "Dependency policy (cargo-deny)", "skip", "cargo-deny not installed")

    if have("audit", cargo_sub=True):
        rc, out, err, d = run(["cargo", "audit", "--json"])
        try:
            j = json.loads(out)
            vulns = [{"id": v["advisory"]["id"], "package": v["package"]["name"],
                      "version": v["package"]["version"], "title": v["advisory"]["title"],
                      "url": v["advisory"].get("url") or ""}
                     for v in j["vulnerabilities"]["list"]]
            warns = [{"id": (w.get("advisory") or {}).get("id", kind), "package": w["package"]["name"],
                      "version": w["package"]["version"],
                      "title": (w.get("advisory") or {}).get("title", kind), "url": ""}
                     for kind, ws in (j.get("warnings") or {}).items() for w in ws]
        except (ValueError, KeyError):
            vulns, warns = [], []
        step("audit", "Vulnerability audit (cargo-audit, RustSec)",
             "pass" if rc == 0 and not vulns else "fail",
             f"{len(vulns)} vulnerability(ies), {len(warns)} warning(s)", d, err,
             vulns=vulns, warns=warns)
    else:
        step("audit", "Vulnerability audit (cargo-audit)", "skip", "cargo-audit not installed")

    # static scans
    rc, out, err, d = run([sys.executable, "scripts/ci_lints.py"])
    found = [l.strip() for l in (out + err).splitlines() if l.strip().startswith("[")]
    step("lints", "Static security lints (SQL / log injection)", "pass" if rc == 0 else "fail",
         "clean" if rc == 0 else f"{len(found)} finding(s)", d, out + err, lines=found)

    if gitleaks_bin():
        gl = os.path.join(OUT_DIR, "gitleaks.json")
        rc, out, err, d = run([gitleaks_bin(), "detect", "--no-git", "--source", ".", "--config", ".gitleaks.toml", "--redact",
                               "--report-format", "json", "--report-path", gl, "--no-banner"])
        try:
            leaks = [{"rule": x["RuleID"], "file": x["File"], "line": x["StartLine"]}
                     for x in json.load(open(gl, encoding="utf-8"))]
        except (OSError, ValueError):
            leaks = []
        step("secrets", "Secret scan (gitleaks)", "pass" if rc == 0 else "fail",
             f"{len(leaks)} finding(s)", d, err, leaks=leaks)
    else:
        step("secrets", "Secret scan (gitleaks)", "skip", "gitleaks not installed")

    rc, out, err, d = run(["cargo", "run", "-q", "-p", "xtask", "--", "traceability-check"])
    tail = (out + err).strip().splitlines()
    step("trace", "Requirement traceability (xtask)", "pass" if rc == 0 else "fail",
         tail[-1] if tail else "", d, out + err)

    doc_env = dict(ENV, RUSTDOCFLAGS="-D warnings")
    t = time.time()
    p = subprocess.run(["cargo", "doc", "--workspace", "--all-features", "--no-deps"], cwd=ROOT,
                       env=doc_env, capture_output=True, text=True, encoding="utf-8",
                       errors="replace")
    step("doc", "API docs (rustdoc -D warnings)", "pass" if p.returncode == 0 else "fail",
         "clean" if p.returncode == 0 else "rustdoc warnings", time.time() - t, p.stderr)

    finished = datetime.datetime.now().astimezone()
    failed = [s for s in steps if s["gate"] and s["status"] == "fail"]
    meta = {
        "started": started.isoformat(timespec="seconds"),
        "duration": (finished - started).total_seconds(),
        "commit": git("rev-parse", "--short", "HEAD") or "none",
        "branch": git("rev-parse", "--abbrev-ref", "HEAD"),
        "dirty": bool(git("status", "--porcelain")),
        "rustc": run(["rustc", "--version"], 30)[1].strip(),
        "live": live,
        "result": "FAIL" if failed else "PASS",
    }

    history = []
    if os.path.exists(HISTORY):
        try:
            history = json.load(open(HISTORY, encoding="utf-8"))
        except ValueError:
            history = []
    tests = next(s for s in steps if s["key"] == "tests")
    covs = next(s for s in steps if s["key"] == "coverage")
    history.append({**{k: meta[k] for k in ("started", "commit", "result", "live")},
                    "passed": n["ok"], "failed": n["FAILED"], "ignored": n["ignored"],
                    "coverage": round(covs["totals"]["lines"]["percent"], 1) if "totals" in covs else None,
                    "steps": {s["key"]: s["status"] for s in steps}})
    history = history[-HISTORY_KEEP:]
    with open(HISTORY, "w", encoding="utf-8") as f:
        json.dump(history, f, indent=1)

    with open(REPORT, "w", encoding="utf-8") as f:
        f.write(render(meta, steps, history))
    print(f"\n{meta['result']}: wrote {os.path.relpath(REPORT, ROOT)}"
          + (f" ({len(failed)} gate(s) failed: {', '.join(s['key'] for s in failed)})" if failed else ""))
    return 1 if failed and "--no-fail" not in args else 0


# ------------------------------------------------------------------ html

E = html.escape
BADGE = {"pass": "Pass", "fail": "Fail", "skip": "Skipped", "warn": "Below target",
         "ok": "Pass", "FAILED": "Fail", "ignored": "Ignored", "PASS": "Pass", "FAIL": "Fail"}


def badge(status):
    cls = {"ok": "pass", "FAILED": "fail", "ignored": "skip", "PASS": "pass",
           "FAIL": "fail"}.get(status, status)
    return f'<span class="b {cls}">{BADGE.get(status, status)}</span>'


def pct_cell(p):
    cls = "hi" if p >= 90 else "mid" if p >= 70 else "lo"
    return (f'<td class="num"><div class="bar"><i class="{cls}" style="width:{p:.1f}%"></i></div>'
            f'{p:.1f}%</td>')


def render(meta, steps, history):
    by = {s["key"]: s for s in steps}
    t = by["tests"]
    cases = t.get("cases", [])
    n = {s: sum(c["status"] == s for c in cases) for s in ("ok", "FAILED", "ignored")}
    cov = by["coverage"]
    cov_txt = f'{cov["totals"]["lines"]["percent"]:.1f}%' if "totals" in cov else "n/a"
    vulns = len(by["audit"].get("vulns", [])) if by["audit"]["status"] != "skip" else "n/a"
    lint_n = len(by["clippy"].get("items", []))

    def card(label, value, status, sub=""):
        return (f'<div class="card {status}"><div class="lab">{E(label)}</div>'
                f'<div class="val">{E(str(value))}</div><div class="sub">{E(sub)}</div></div>')

    cards = "".join([
        card("Result", meta["result"], "pass" if meta["result"] == "PASS" else "fail",
             f'{sum(s["status"] == "fail" for s in steps)} failing check(s)'),
        card("Tests", f'{n["ok"]}/{len(cases)}', t["status"],
             f'{n["FAILED"]} failed · {n["ignored"]} ignored'),
        card("Line coverage", cov_txt, cov["status"], "target 95%"),
        card("Vulnerabilities", vulns, by["audit"]["status"], "RustSec advisories"),
        card("Lint findings", lint_n, by["clippy"]["status"], "clippy -D warnings"),
        card("Secret findings", len(by["secrets"].get("leaks", [])) if by["secrets"]["status"] != "skip" else "n/a",
             by["secrets"]["status"], "gitleaks"),
    ])

    rows = "".join(
        f'<tr><td>{badge(s["status"])}</td><td><a href="#{s["key"]}">{E(s["title"])}</a>'
        f'{"" if s["gate"] else " <span class=muted>(informational)</span>"}</td>'
        f'<td>{E(s["summary"])}</td><td class="num">{s["duration"]:.1f}s</td></tr>'
        for s in steps)

    # test cases grouped by suite
    suites = {}
    for c in cases:
        suites.setdefault(c["suite"], []).append(c)
    groups = []
    for suite, cs in sorted(suites.items()):
        f = sum(c["status"] == "FAILED" for c in cs)
        trs = "".join(
            f'<tr class="tc {c["status"]}"><td>{badge(c["status"])}</td><td><code>{E(c["name"])}</code>'
            + (f'<div class="muted">{E(c["note"])}</div>' if c["note"] else "")
            + (f'<pre>{E(c.get("detail", ""))}</pre>' if c.get("detail") else "")
            + "</td></tr>" for c in cs)
        groups.append(f'<details class="suite"{" open" if f else ""}><summary><code>{E(suite)}</code>'
                      f' <span class="muted">{len(cs)} test(s)</span>{" " + badge("FAILED") if f else ""}'
                      f'</summary><table>{trs}</table></details>')
    tests_html = "".join(groups) or '<p class="muted">No test output parsed.</p>'

    if "files" in cov:
        tot = cov["totals"]
        cov_html = (
            f'<p>Lines {tot["lines"]["covered"]}/{tot["lines"]["count"]} · functions '
            f'{tot["functions"]["percent"]:.1f}% · regions {tot["regions"]["percent"]:.1f}%. '
            f'Excludes bench, xtask, tests and examples. Sorted lowest first.</p>'
            '<table><thead><tr><th>File</th><th class="num">Lines</th><th class="num">Functions</th>'
            '<th class="num">Regions</th><th class="num">Uncovered lines</th></tr></thead><tbody>'
            + "".join(f'<tr><td><code>{E(fl["file"])}</code></td>{pct_cell(fl["lines"]["percent"])}'
                      f'{pct_cell(fl["functions"]["percent"])}{pct_cell(fl["regions"]["percent"])}'
                      f'<td class="num">{fl["lines"]["count"] - fl["lines"]["covered"]}</td></tr>'
                      for fl in cov["files"])
            + "</tbody></table>")
    else:
        cov_html = f'<p class="muted">{E(cov["summary"])}</p>'

    def items_table(items, cols):
        if not items:
            return '<p class="muted">No findings.</p>'
        head = "".join(f"<th>{E(c)}</th>" for c, _ in cols)
        body = "".join("<tr>" + "".join(f"<td>{E(str(it.get(k, '')))}</td>" for _, k in cols) + "</tr>"
                       for it in items)
        return f"<table><thead><tr>{head}</tr></thead><tbody>{body}</tbody></table>"

    def section(key, inner):
        s = by[key]
        log = (f'<details><summary>Raw output</summary><pre>{E(s["log"])}</pre></details>'
               if s["log"].strip() else "")
        return (f'<section id="{key}"><h2>{E(s["title"])} {badge(s["status"])}</h2>'
                f'<p>{E(s["summary"])}</p>{inner}{log}</section>')

    audit = by["audit"]
    hist_rows = "".join(
        f'<tr><td>{E(h["started"].replace("T", " ")[:16])}</td><td><code>{E(h["commit"])}</code></td>'
        f'<td>{badge(h["result"])}</td><td class="num">{h["passed"]}</td><td class="num">{h["failed"]}</td>'
        f'<td class="num">{h["ignored"]}</td><td class="num">{"" if h["coverage"] is None else str(h["coverage"]) + "%"}</td>'
        f'<td>{"live" if h["live"] else "offline"}</td></tr>'
        for h in reversed(history))

    return f"""<!doctype html>
<html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>dbmr Quality Report</title>
<style>
:root{{--bg:#f7f8fa;--fg:#1c2330;--muted:#677185;--card:#fff;--line:#e2e6ec;--pass:#1a7f4b;--passbg:#e3f5ea;
--fail:#b42318;--failbg:#fde7e5;--skip:#5d6778;--skipbg:#eceff3;--warn:#9a5b00;--warnbg:#fdf1dc;--code:#eef1f5}}
@media (prefers-color-scheme:dark){{:root{{--bg:#11151c;--fg:#e4e8ee;--muted:#98a2b3;--card:#19202a;--line:#2a3340;
--pass:#5fd394;--passbg:#163626;--fail:#ff8a7e;--failbg:#3d1b18;--skip:#aab3c2;--skipbg:#252d38;--warn:#f3b65c;--warnbg:#3a2c12;--code:#222a35}}}}
*{{box-sizing:border-box}}body{{margin:0;background:var(--bg);color:var(--fg);font:14px/1.5 system-ui,-apple-system,Segoe UI,sans-serif}}
main{{max-width:1180px;margin:0 auto;padding:24px 16px 64px}}h1{{margin:0 0 4px;font-size:24px}}
h2{{font-size:18px;margin:0 0 8px;display:flex;gap:10px;align-items:center}}
section{{background:var(--card);border:1px solid var(--line);border-radius:10px;padding:16px;margin:16px 0;overflow-x:auto}}
.muted{{color:var(--muted)}}code{{background:var(--code);padding:1px 5px;border-radius:4px;font-size:12.5px}}
pre{{background:var(--code);padding:10px;border-radius:6px;overflow:auto;max-height:420px;font-size:12px;white-space:pre-wrap}}
table{{width:100%;border-collapse:collapse}}th,td{{text-align:left;padding:6px 8px;border-bottom:1px solid var(--line);vertical-align:top}}
th{{font-size:12px;color:var(--muted);font-weight:600}}.num{{text-align:right;white-space:nowrap}}
.cards{{display:grid;grid-template-columns:repeat(auto-fit,minmax(160px,1fr));gap:12px;margin:16px 0}}
.card{{background:var(--card);border:1px solid var(--line);border-left:4px solid var(--skip);border-radius:10px;padding:12px}}
.card.pass{{border-left-color:var(--pass)}}.card.fail{{border-left-color:var(--fail)}}.card.warn{{border-left-color:var(--warn)}}
.lab{{font-size:12px;color:var(--muted)}}.val{{font-size:26px;font-weight:700}}.sub{{font-size:12px;color:var(--muted)}}
.b{{font-size:11px;font-weight:600;padding:2px 8px;border-radius:999px;white-space:nowrap}}
.b.pass{{color:var(--pass);background:var(--passbg)}}.b.fail{{color:var(--fail);background:var(--failbg)}}
.b.skip{{color:var(--skip);background:var(--skipbg)}}.b.warn{{color:var(--warn);background:var(--warnbg)}}
.bar{{display:inline-block;width:70px;height:6px;background:var(--line);border-radius:3px;margin-right:6px;vertical-align:middle;overflow:hidden}}
.bar i{{display:block;height:100%}}.bar .hi{{background:var(--pass)}}.bar .mid{{background:var(--warn)}}.bar .lo{{background:var(--fail)}}
details.suite{{border-top:1px solid var(--line);padding:6px 0}}summary{{cursor:pointer}}
.tools{{display:flex;gap:8px;flex-wrap:wrap;margin-bottom:8px}}input,select{{font:inherit;padding:5px 8px;border:1px solid var(--line);border-radius:6px;background:var(--bg);color:var(--fg)}}
nav a{{margin-right:12px}}a{{color:inherit}}
</style></head><body><main>
<h1>dbmr quality report</h1>
<div class="muted">Run {E(meta["started"].replace("T", " "))} · {meta["duration"]:.0f}s · commit <code>{E(meta["commit"])}</code>{" (uncommitted changes)" if meta["dirty"] else ""} · branch {E(meta["branch"])} · {E(meta["rustc"])} · {"live services" if meta["live"] else "offline"}</div>
<div class="cards">{cards}</div>
<nav class="muted"><a href="#checks">Checks</a><a href="#tests">Test cases</a><a href="#coverage">Coverage</a><a href="#deny">Dependencies</a><a href="#audit">Audit</a><a href="#lints">Static scan</a><a href="#clippy">Lint</a><a href="#history">History</a></nav>
<section id="checks"><h2>Checks</h2><table><thead><tr><th>Status</th><th>Check</th><th>Summary</th><th class="num">Time</th></tr></thead><tbody>{rows}</tbody></table></section>
<section id="tests"><h2>Test cases {badge(t["status"])}</h2><p>{E(t["summary"])} · {len(suites)} suite(s)</p>
<div class="tools"><input id="q" placeholder="Filter tests…" oninput="ft()"><select id="st" onchange="ft()"><option value="">All</option><option value="ok">Passed</option><option value="FAILED">Failed</option><option value="ignored">Ignored</option></select></div>
{tests_html}
<details><summary>Raw output</summary><pre>{E(t["log"])}</pre></details></section>
<section id="coverage"><h2>{E(cov["title"])} {badge(cov["status"])}</h2>{cov_html}</section>
{section("deny", items_table(by["deny"].get("items", []), [("Level", "level"), ("Check", "code"), ("Message", "message")]))}
{section("audit", items_table(audit.get("vulns", []), [("Advisory", "id"), ("Crate", "package"), ("Version", "version"), ("Title", "title")]) + ("<h3>Warnings (unmaintained / yanked)</h3>" + items_table(audit.get("warns", []), [("Advisory", "id"), ("Crate", "package"), ("Version", "version"), ("Title", "title")]) if audit.get("warns") else ""))}
{section("lints", items_table([{"f": l} for l in by["lints"].get("lines", [])], [("Finding", "f")]))}
{section("secrets", items_table(by["secrets"].get("leaks", []), [("Rule", "rule"), ("File", "file"), ("Line", "line")]))}
{section("clippy", items_table(by["clippy"].get("items", []), [("Level", "level"), ("Lint", "code"), ("File", "file"), ("Line", "line"), ("Message", "message")]))}
{section("fmt", "")}
{section("trace", "")}
{section("doc", "")}
<section id="history"><h2>History (last {HISTORY_KEEP} runs)</h2><table><thead><tr><th>Run</th><th>Commit</th><th>Result</th><th class="num">Passed</th><th class="num">Failed</th><th class="num">Ignored</th><th class="num">Coverage</th><th>Mode</th></tr></thead><tbody>{hist_rows}</tbody></table></section>
<p class="muted">Generated by <code>scripts/report.py</code> (pre-push hook). Re-run: <code>python scripts/report.py</code>.</p>
</main>
<script>
function ft(){{var q=document.getElementById('q').value.toLowerCase(),s=document.getElementById('st').value;
document.querySelectorAll('details.suite').forEach(function(d){{var any=0;d.querySelectorAll('tr.tc').forEach(function(r){{
var show=(!q||r.textContent.toLowerCase().indexOf(q)>=0)&&(!s||r.classList.contains(s));r.style.display=show?'':'none';if(show)any=1;}});
d.style.display=any?'':'none';if(q||s)d.open=!!any;}});}}
</script></body></html>
"""


if __name__ == "__main__":
    sys.exit(main())
