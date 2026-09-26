"""S9 step 5 (plan decision 42): the transition fence's DEFAULT follows the
store backend. It is OFF on PostgreSQL (every writer converted, the fence-off
gate passed) and ON for SQLite/JSON (the legacy door-off DOC_LOCK fallbacks
still run there). ORGTREE_ORGTX_FENCE=0/1 overrides it both ways.

What these prove:
  * `orgtx.default_transition_fence` gives all six backend x env answers;
  * a fresh interpreter importing orgtree with only the env set gets the same
    answer in `orgtx.TRANSITION_FENCE` and in halt's fence, so the pin is on
    the real module-level value and not only on the helper.

Run:  python tools/run-python-verification.py tests/test_fence_default.py
"""

import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

_temp = tempfile.TemporaryDirectory(prefix='v3-fence-default-', ignore_cleanup_errors=True)
data = Path(_temp.name) / 'data'
data.mkdir()
os.environ.update(ORGTREE_DATA=str(data), ORGTREE_STORE='sqlite')

import import_provenance  # noqa: F401,E402  asserts orgtree resolves inside this checkout

import orgtree  # noqa: E402
from orgtree import orgtx  # noqa: E402

CASES = [
    # (backend, ORGTREE_ORGTX_FENCE or None, expected fence)
    ('postgres', None, False),
    ('postgres', '', False),
    ('postgres', '1', True),
    ('postgres', '0', False),
    ('sqlite', None, True),
    ('sqlite', '', True),
    ('sqlite', '1', True),
    ('sqlite', '0', False),
    ('json', None, True),
]

_CHILD = r"""
import json, orgtree
from orgtree import halt, orgtx, store
print(json.dumps({'file': orgtree.__file__, 'backend': store.STORE_BACKEND,
                  'fence': orgtx.TRANSITION_FENCE, 'halt': halt._fence() is store.FENCE}))
"""


def tearDownModule() -> None:
    _temp.cleanup()


class FenceDefault(unittest.TestCase):
    def test_helper_gives_every_answer(self):
        for backend, raw, want in CASES:
            env = {} if raw is None else {'ORGTREE_ORGTX_FENCE': raw}
            with self.subTest(backend=backend, env=raw):
                self.assertIs(orgtx.default_transition_fence(env, backend), want)

    def test_helper_reads_the_store_backend_when_not_told(self):
        self.assertIs(orgtx.default_transition_fence({}), orgtx.store.STORE_BACKEND != 'postgres')

    def test_a_fresh_import_gets_the_default(self):
        root = Path(orgtree.__file__).resolve().parents[1]      # engine/backend
        for backend, raw, want in [c for c in CASES if c[0] != 'json']:
            env = {k: v for k, v in os.environ.items() if k != 'ORGTREE_ORGTX_FENCE'}
            env.update(ORGTREE_STORE=backend, PYTHONPATH=os.pathsep.join(sys.path))
            if raw is not None:
                env['ORGTREE_ORGTX_FENCE'] = raw
            with self.subTest(backend=backend, env=raw):
                out = subprocess.run([sys.executable, '-c', _CHILD], env=env, cwd=str(root),
                                     capture_output=True, text=True, timeout=120)
                self.assertEqual(out.returncode, 0, out.stderr[-2000:])
                got = json.loads(out.stdout.strip().splitlines()[-1])
                # the child must have imported THIS checkout, not an installed build
                self.assertEqual(Path(got['file']).resolve(), Path(orgtree.__file__).resolve())
                self.assertEqual(got['backend'], backend)
                self.assertIs(got['fence'], want)
                self.assertIs(got['halt'], want)


if __name__ == '__main__':
    unittest.main()
