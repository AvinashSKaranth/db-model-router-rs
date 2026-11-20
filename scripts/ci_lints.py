#!/usr/bin/env python3
"""
CI Security Lints for dbmr:
1. SQL Injection Lint (§36.5): Disallow format! / concat! building dynamic SQL query literals outside of escape_id or safe query builder files.
2. Log Injection Lint (§36.4): Disallow logging or tracing raw payload/password/secret/key values.
"""

import os
import re
import sys

ERRORS = []

SQL_CONCAT_PATTERN = re.compile(r'format!\s*\(\s*["\']\s*(SELECT|INSERT\s+INTO|UPDATE|DELETE\s+FROM|DROP\s+TABLE|ALTER\s+TABLE|CREATE\s+TABLE)\b', re.IGNORECASE)
LOG_SECRET_PATTERN = re.compile(r'\b(tracing::(trace|debug|info|warn|error)|event!|\.instrument)\s*!\s*\(.*?\b(password|secret|key_material|raw_payload|payload)\b', re.IGNORECASE)

CRATES_DIR = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", "crates"))

ALLOWLIST_SQL_FILES = {
    "escape_id",
    "compiler.rs",
    "sql_helpers.rs",
    "dbmr-adapters",
    "adapters",
}

for root, dirs, files in os.walk(CRATES_DIR):
    for f in files:
        if f.endswith(".rs"):
            file_path = os.path.join(root, f)
            rel_path = os.path.relpath(file_path, CRATES_DIR)

            with open(file_path, "r", encoding="utf-8", errors="replace") as fp:
                for line_no, line in enumerate(fp, 1):
                    # Check SQL injection lint
                    # Test fixtures build DDL from fixed table names: no user input reaches them.
                    in_tests = os.sep + "tests" + os.sep in os.sep + rel_path
                    if in_tests or any(allowed in rel_path for allowed in ALLOWLIST_SQL_FILES):
                        pass
                    else:
                        if SQL_CONCAT_PATTERN.search(line):
                            ERRORS.append(f"[SQL-INJECTION-LINT] {rel_path}:{line_no}: Potential dynamic SQL construction via format!: {line.strip()}")

                    # Check Log injection lint
                    if LOG_SECRET_PATTERN.search(line):
                        ERRORS.append(f"[LOG-INJECTION-LINT] {rel_path}:{line_no}: Potential sensitive logging of credentials/payload: {line.strip()}")

if ERRORS:
    print(f"FAILED: {len(ERRORS)} security lint violation(s) found:")
    for err in ERRORS:
        print(f"  {err}")
    sys.exit(1)
else:
    print("SUCCESS: Static security lints (SQL & Log injection) passed clean.")
    sys.exit(0)
