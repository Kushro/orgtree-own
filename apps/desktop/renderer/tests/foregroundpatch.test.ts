// foreground-tree F1: a ws metadata patch (cache forecast, MCP counts) must
// not drop the SELECTED tree's ETag. The server moves its runtime stamp
// before it sends such a frame, so a kept ETag can only be answered 304 when
// the server's current body equals the kept one; otherwise the answer is a
// delta against the kept base. The full-tree (legacy) cache is still fenced.
import './harness'
import test from 'node:test'
import assert from 'node:assert/strict'
import { getSelectedTree, invalidateTreeCache, patchedTreeCache } from '../src/api'
import { FOREGROUND_TREE_FORMAT as format } from '../src/foregroundtree'

const selection = { include: [], hideRetired: true, fronts: {} }
const node = (forecast: string) => ({
  id: 'active', parent: null, children: [], axis: 'org', state: 'live',
  hidden_retired_children: 0, lineage_loaded: false, lineage_count: 0,
  predecessor: null, successor: null, cache_forecast: forecast,
})
const header = { slug: 'org', hidden_retired_roots: 0, retired_total: 0, archived_defaults: { busy: false } }

// A server that answers like the real one: 304 only for its CURRENT tag, a
// delta from a tag it still holds, a snapshot otherwise.
let current = 1
const forecast = (v: number) => `forecast-${v}`
const tag = (v: number) => `W/"foreground-r${v}"`
const snapshot = (v: number) => ({ format, kind: 'snapshot', revision: `r${v}`, catalog_revision: 'org:1',
  org_rev: v, sync_rev: v, nodes: { active: node(forecast(v)) }, roots: ['active'],
  missing_requested: [], header })
const delta = (from: number, to: number) => ({ format, kind: 'delta', base: `r${from}`, revision: `r${to}`,
  catalog_revision: 'org:1', org_rev: to, sync_rev: to, roots: ['active'], missing_requested: [],
  header: { set: {}, unset: [] }, removed: [],
  nodes: { active: { set: { cache_forecast: forecast(to) }, unset: [] } } })
const sent: (string | undefined)[] = []
;(globalThis as unknown as { fetch: unknown }).fetch = async (url: string, init?: { headers?: Record<string, string> }) => {
  assert.match(String(url), /\/foreground-tree/)
  const since = init?.headers?.['If-None-Match']
  sent.push(since)
  const reply = (status: number, body: unknown) => ({
    status, ok: status === 200, headers: new Headers({ ETag: tag(current) }), json: async () => body,
  })
  if (since === tag(current)) return reply(304, null)
  const base = since ? Number(/r(\d+)/.exec(since)?.[1]) : NaN
  return base > 0 && base < current ? reply(200, delta(base, current)) : reply(200, snapshot(current))
}
const shown = (tree: { roots: { cache_forecast?: unknown }[] }) => tree.roots[0].cache_forecast

test('a metadata patch keeps the selected ETag: the next read is a delta, never a revalidated pre-patch body', async () => {
  invalidateTreeCache('org')
  sent.length = 0
  current = 1
  assert.equal(shown(await getSelectedTree('org', selection) as never), forecast(1))
  current = 2                           // the frame's value is in the server's body now
  patchedTreeCache('org')               // App.tsx on cache_forecast / mcp_* frames
  const after = await getSelectedTree('org', selection)
  assert.equal(shown(after as never), forecast(2), 'a pre-patch body was served')
  assert.deepEqual(sent, [undefined, tag(1)], 'the kept ETag was not sent: the patch forced a full snapshot')
  const again = await getSelectedTree('org', selection)
  assert.equal(shown(again as never), forecast(2))
  assert.deepEqual(sent, [undefined, tag(1), tag(2)])
})

test('an unchanged server answers the kept ETag with 304 and the kept body is kept', async () => {
  invalidateTreeCache('org')
  sent.length = 0
  current = 5
  const first = await getSelectedTree('org', selection)
  patchedTreeCache('org')               // a frame whose value the body does not carry
  const second = await getSelectedTree('org', selection)
  assert.equal(shown(second as never), shown(first as never))
  assert.deepEqual(sent, [undefined, tag(5)])
})
