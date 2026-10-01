"""An agent that sees only itself is told who its superior is.

Item self-visibility-agents-are-told-their-superior-i, user ruling 2026-10-01
("Yes, show it"). An agent with `org_visibility: self` used to read in its
identity prompt that its superior's identity "is not disclosed", while the
org-state block on every turn named that superior anyway. Now:

  §1  at `self`, the identity prompt names the superior and says nothing
      about it being hidden
  §2  a top-level `self` agent is told its superior is the user
  §3  every other visibility level keeps the same line
"""
import os
import tempfile
import unittest
import uuid
from pathlib import Path

fx = tempfile.TemporaryDirectory(prefix='self-vis-superior-', ignore_cleanup_errors=True)
os.environ['ORGTREE_DATA'] = str(Path(fx.name) / 'data')
os.environ['HOME'] = str(Path(fx.name) / 'home')
os.environ['USERPROFILE'] = os.environ['HOME']
Path(os.environ['ORGTREE_DATA']).mkdir()
Path(os.environ['HOME']).mkdir()
os.environ['ORGTREE_V2_TOKEN'] = 'self-vis-only'
for k in ('ORGTREE_V1_ROOT', 'ORGTREE_V1_DATA_ROOT', 'ORGTREE_V2_PORT'):
    os.environ.pop(k, None)

import import_provenance  # noqa: F401  asserts orgtree resolves inside this checkout

from engine.launch import load_app                                   # noqa: E402
load_app()
from orgtree import ledger, store                                   # noqa: E402
from orgtree import supervisor as sup                                # noqa: E402
assert Path(store.DATA_ROOT).resolve() == Path(os.environ['ORGTREE_DATA']).resolve(), \
    'this process would have written to the live root'

TOOLS = {'bash': False, 'web': False, 'edit': False, 'subagents': False, 'mcp': []}
slugs: list[str] = []


def tearDownModule() -> None:
    for s in slugs:
        store._POOL.close_all(s)


class SelfVisibilitySuperior(unittest.TestCase):
    def setUp(self) -> None:
        self.slug = 'selfvis-' + uuid.uuid4().hex[:8]
        slugs.append(self.slug)
        self.org = store.create_org(self.slug)
        self.org.hire(ledger.USER, None, 'opus', 10, 'boss')
        store.save_org(self.org)

    def hire(self, visibility: str, parent: str | None = 'boss') -> str:
        name = 'w' + uuid.uuid4().hex[:6]
        actor = 'boss' if parent else ledger.USER
        self.org.hire(actor, parent, 'haiku', 0, name, add_dirs=[], tools=TOOLS,
                      org_visibility=visibility, charter='fixture agent')
        store.save_org(self.org)
        return name

    def test_self_visibility_names_the_superior(self) -> None:
        text = sup.identity_prompt(self.org, self.hire('self'))
        self.assertIn('Your superior: boss.', text)
        self.assertNotIn('not disclosed', text)

    def test_a_top_level_self_agent_is_told_the_user(self) -> None:
        text = sup.identity_prompt(self.org, self.hire('self', parent=None))
        self.assertIn('Your superior: the user.', text)
        self.assertNotIn('not disclosed', text)

    def test_every_level_names_the_superior(self) -> None:
        for level in ('self', 'team', 'full'):
            with self.subTest(level=level):
                text = sup.identity_prompt(self.org, self.hire(level))
                self.assertIn('Your superior: boss.', text)


if __name__ == '__main__':
    unittest.main()
