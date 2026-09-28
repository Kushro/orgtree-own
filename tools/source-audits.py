"""Run the four cheap SOURCE AUDITS before landing: about a minute, no database.

Every new or edited test module can break these without touching product code,
and they were the bulk of the 21 python regressions found at v3 34c8649
(fix-the-21-new-python-backend-test-failures-befo):

  tests/test_import_provenance.py        every test module carries the ONE guard line
  tests/test_hub_isolation.py            a file that names the launcher isolates the hub
                                         (or is EXEMPT with a reason)
  tests/test_child_spawn_gate.py         every sys.executable mention is allowlisted
  tests/test_no_duplicate_definitions.py every Python file parses (no U+FEFF BOM) and
                                         defines each name once

Usage (from the checkout you are about to land, after your final rebase):

    python tools/source-audits.py [--json-output PATH]

It runs the four modules through tools/run-python-verification.py (one fresh
interpreter each, this checkout's provenance) one at a time, prints each
module's result line and first failure, and exits 0 only when all four pass.
It is not a test-baseline run and takes no lock; still check the machine's
test gate first, as for any module run."""
import argparse
import json
from pathlib import Path
import subprocess
import sys
import tempfile

REPO = Path(__file__).resolve().parents[1]
AUDITS = ("tests/test_import_provenance.py", "tests/test_hub_isolation.py",
          "tests/test_child_spawn_gate.py", "tests/test_no_duplicate_definitions.py")


def first_failure(stderr: str) -> str:
    """The first FAIL/ERROR block's assertion text, a few lines, for the summary."""
    blocks = stderr.split("=" * 70)[1:]
    if not blocks:
        return ""
    lines = [ln for ln in blocks[0].splitlines() if ln.strip()]
    tail = [ln for ln in lines if ln.startswith(("AssertionError", "SyntaxError", "- ", "+ "))]
    return "\n      ".join([lines[0]] + tail[:6])


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--json-output", type=Path, help="also write every module's receipt here")
    args = parser.parse_args()
    results, ok = [], True
    with tempfile.TemporaryDirectory(prefix="source-audits-") as tmp:
        for module in AUDITS:
            receipt = Path(tmp) / (Path(module).stem + ".json")
            proc = subprocess.run([sys.executable, "tools/run-python-verification.py", module,
                                   "--timeout", "300", "--json-output", str(receipt)],
                                  cwd=REPO, capture_output=True, text=True, timeout=420)
            try:
                data = json.loads(receipt.read_text(encoding="utf-8"))
                row = data["modules"][0]
            except (OSError, ValueError, KeyError, IndexError):
                print(f"CANNOT RUN {module}: runner exit {proc.returncode}\n{proc.stderr[-800:]}")
                return 2
            stderr = row.get("stderr", "")
            summary = [ln for ln in stderr.splitlines() if ln.startswith(("Ran ", "OK", "FAILED"))]
            passed = proc.returncode == 0 and any(ln.startswith("OK") for ln in summary)
            ok = ok and passed
            print(f"{'PASS' if passed else 'FAIL'} {module}: {' / '.join(summary) or '(no result line)'}")
            if not passed:
                print("      " + first_failure(stderr))
            results.append({"module": module, "passed": passed, "summary": summary,
                            "runner_exit": proc.returncode, "import_provenance": row.get("import_provenance"),
                            "stderr": stderr})
    if args.json_output:
        args.json_output.write_text(json.dumps({"repo": str(REPO), "modules": results}, indent=2) + "\n",
                                    encoding="utf-8")
    print("SOURCE AUDITS: " + ("PASS" if ok else "FAIL (fix these before landing)"))
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
