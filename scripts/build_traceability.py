#!/usr/bin/env python3
"""Rebuild tests/traceability.json from the Node reference suite.

Mapping is FILE-LEVEL: each Node `it()` case inherits the status of its test file.
`covered` means a Rust suite exercises the same behaviour area on SQLite/in-memory;
it does NOT claim a 1:1 port of every case. Statuses:

  covered       Rust test file(s) listed in `rust` exercise this area and pass in CI.
  unverified    Rust live test exists but needs a database service (#[ignore]).
  out_of_scope  CLI / db-manager / codegen / migrations: moved to a separate project.
  waived        Not applicable to Rust (Express internals, dev scratch). See `note`.
  open          No Rust counterpart yet.

Usage: PYTHONUTF8=1 python scripts/build_traceability.py [path-to-node-repo]
"""
import json
import os
import re
import sys

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))
NODE = sys.argv[1] if len(sys.argv) > 1 else os.path.join(ROOT, "..", "db-model-router")
OUT = os.path.join(ROOT, "tests", "traceability.json")

ROUTE = "crates/dbmr/tests/route_tests.rs"
MODEL = "crates/dbmr/tests/model_contract.rs"
FILTER = "crates/dbmr/tests/filter_tests.rs"
SCHEMA = "crates/dbmr/tests/schema_tests.rs"
SQLITE = "crates/dbmr/tests/sqlite_tests.rs"
DBS = "postgres|mysql|mariadb|cockroachdb|mssql|oracle|mongodb|redis|dynamodb"

# (regex on path relative to Node repo, status, rust refs, note). First match wins.
RULES = [
    (r"test/(commands/|integration/|cli|flags|saas-structure|schema-printer|diff-engine)", "out_of_scope", [], "CLI extracted to separate project (ADR-0013)"),
    (r"test/(unit/(cli-|migrat|types-generator|dashboard-api)|properties/(codegen|db-manager|saas-structure))", "out_of_scope", [], "CLI / db-manager / codegen extracted (ADR-0013)"),
    (r"test/features\.test\.js", "open", [], "Mixed: OpenAPI generator (out of scope) + model features; not ported case-by-case"),
    (r"test/unit/route\.query-ultimate-express", "waived", [], "Express monkeypatch; Axum handles QUERY natively"),
    (r"test/dev\.js", "waived", [], "Upstream dev scratch file"),
    (rf"test/adapters/({DBS})\.", "unverified", ["crates/dbmr/tests/{db}_tests.rs"], "Needs live service (docker compose)"),
    (rf"test/properties/({DBS})\.filter", "unverified", ["crates/dbmr/tests/{db}_tests.rs"], "Needs live service"),
    (r"test/adapters/sqlite3\.route", "covered", [ROUTE], ""),
    (r"test/adapters/sqlite3\.", "covered", [SQLITE, MODEL], ""),
    (r"test/(route\.test|filter-operators\.route|unit/(route\.query-method|api-query|csv-export|vulnerability-security)|properties/(route|response-shape))", "covered", [ROUTE], ""),
    (r"test/(filter-operators\.test|unit/(parse-filters|sort-state)|properties/(filter|search|sqlite3\.filter))", "covered", [FILTER, "crates/dbmr/src/adapters/compiler.rs"], ""),
    (r"test/(model\.|unit/(model\.hooks|transaction|null-impact)|encryption\.model|json-consistency|properties/model-roundtrip)", "covered", [MODEL], ""),
    (r"test/encryption\.unit", "covered", ["crates/dbmr/src/core/encryption.rs"], "unit tests in module"),
    (r"test/(schema-(validator|parser|to-meta)|properties/schema)", "covered", [SCHEMA, "crates/dbmr/tests/proptest_schema.rs"], ""),
    (r"test/kafka\.test", "covered", ["crates/dbmr/tests/kafka_tests.rs"], ""),
    (r"test/kafka\.integration", "unverified", ["crates/dbmr/tests/kafka_tests.rs"], "Needs live Kafka"),
    (r"test/db\.(individual|multiple)", "covered", ["crates/dbmr/tests/facade_tests.rs"], "global db shim (ADR-0008)"),
]

IT = re.compile(r"\bit\s*\(\s*['\"`](.*?)['\"`]")


def classify(rel):
    for pat, status, rust, note in RULES:
        m = re.search(pat, rel)
        if m:
            db = m.group(1) if status == "unverified" and m.groups() else ""
            return status, [r.replace("{db}", db) for r in rust], note
    return "open", [], ""


def main():
    cases, missing = [], set()
    for root, _, files in os.walk(os.path.join(NODE, "test")):
        for f in sorted(files):
            if not f.endswith(".js"):
                continue
            path = os.path.join(root, f)
            rel = os.path.relpath(path, NODE).replace("\\", "/")
            status, rust, note = classify(rel)
            missing.update(r for r in rust if not os.path.exists(os.path.join(ROOT, r)))
            with open(path, encoding="utf-8", errors="replace") as fp:
                for n, line in enumerate(fp, 1):
                    m = IT.search(line)
                    if m:
                        cases.append({"file": rel, "line": n, "name": m.group(1), "status": status, "rust": rust, "note": note})
    assert not missing, f"rules reference missing Rust files: {sorted(missing)}"
    summary = {}
    for c in cases:
        summary[c["status"]] = summary.get(c["status"], 0) + 1
    with open(OUT, "w", encoding="utf-8") as fp:
        json.dump({"source": "db-model-router/test", "granularity": "file", "total_cases": len(cases), "summary": summary, "cases": cases}, fp, indent=1)
    print(json.dumps(summary, indent=1))


if __name__ == "__main__":
    main()
