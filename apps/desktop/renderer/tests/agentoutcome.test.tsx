// agentoutcome.test.tsx — canonical agent references on a SELECTED tree:
// omission is not absence. Only the backend's explicit `missing` (in the
// snapshot or an exact reference answer) may call an agent absent.
//
// Run:  cd apps/desktop/renderer && node tests/run.mjs agentoutcome

// ⚠ THE HARNESS IMPORT COMES FIRST — see the import-order note in harness.ts.
import { flush, inAct, mountView } from './harness'
import test from 'node:test'
import type { TestContext } from 'node:test'
import assert from 'node:assert/strict'
import { ForegroundViewContext, resolveRef, useRefRoutes } from '../src/canvas/reflinks'
import { proseCandidates, useProseAgentIndex } from '../src/canvas/docket'
import type { MentionIndex } from '../src/canvas/workrefs'
import type { RefRoutes } from '../src/canvas/reflinks'
import { FOREGROUND_TREE_FORMAT as format } from '../src/foregroundtree'
import type { TreePayload } from '../src/types'

const agent = (id: string) => ({ kind: 'agent' as const, org: 'org', id })
const nodeBox = (node: string) => ({ kind: 'mail' as const, org: 'org', box: 'node' as const, node, id: 'm1' })

function stubReferences(t: TestContext, found: string[], fail = false): string[][] {
  const asked: string[][] = []
  const had = (globalThis as { fetch?: typeof fetch }).fetch;
  (globalThis as unknown as { fetch: typeof fetch }).fetch = (async (url: string) => {
    const query = new URL(String(url), 'http://x').searchParams.getAll('include')
    asked.push(query)
    if (fail) return { ok: false, status: 503, headers: new Headers(), json: async () => ({ detail: 'down' }) }
    const body = { format, kind: 'references', revision: 'r', catalog_revision: 'cat-' + t.name,
      org_rev: 1, sync_rev: 1,
      references: Object.fromEntries(query.filter(id => found.includes(id)).map(id => [id,
        { id, tier: 'astra', state: 'archived', generation: 1, axis: 'org', successor: null }])),
      missing: query.filter(id => !found.includes(id)) }
    return { ok: true, status: 200, headers: new Headers(), json: async () => body }
  }) as unknown as typeof fetch
  t.after(() => { (globalThis as { fetch?: typeof fetch }).fetch = had })
  return asked
}

async function mountWorld(t: TestContext, view: TreePayload['foreground']) {
  let routes: RefRoutes | null = null
  function Probe() {
    routes = useRefRoutes('org', new Map([['live', {}]]), { onFocusAgent: () => {}, onOpenMail: () => {}, view })
    return null
  }
  const v = await mountView(<Probe />, h => h)
  t.after(() => v.unmount())
  return () => routes!.world
}

test('a selected tree looks up omitted agents and never calls them absent on a guess', async (t) => {
  const asked = stubReferences(t, ['old'])
  const world = await mountWorld(t, { catalog_revision: 'cat-' + t.name,
    present: ['live', 'lineage-only'], missing: ['erased'] })
  assert.equal(resolveRef(agent('live'), world()).outcome, 'ready')
  assert.equal(resolveRef(agent('lineage-only'), world()).outcome, 'ready', 'an off-axis row is present')
  assert.equal(resolveRef(agent('erased'), world()).outcome, 'absent', 'explicit snapshot absence is final')
  assert.equal(resolveRef(agent('old'), world()).outcome, 'pending', 'omitted: looked up, not absent')
  assert.equal(resolveRef(nodeBox('never'), world()).outcome, 'pending')
  await inAct(async () => { await flush(4) })
  assert.deepEqual(asked.flat().sort(), ['never', 'old'], 'only omitted identities were asked for')
  assert.equal(resolveRef(agent('old'), world()).outcome, 'ready')
  assert.equal(resolveRef(nodeBox('never'), world()).outcome, 'absent', 'the exact answer said missing')
  assert.equal(asked.length, 1, 'one batched read')
})

test('a failed lookup stays pending rather than absent', async (t) => {
  stubReferences(t, [], true)
  const world = await mountWorld(t, { catalog_revision: 'c', present: [], missing: [] })
  assert.equal(resolveRef(agent('old'), world()).outcome, 'pending')
  await inAct(async () => { await flush(4) })
  assert.equal(resolveRef(agent('old'), world()).outcome, 'pending')
})

test('a complete legacy tree keeps its authoritative map judgement and asks nothing', async (t) => {
  const asked = stubReferences(t, ['old'])
  const world = await mountWorld(t, undefined)
  assert.equal(resolveRef(agent('old'), world()).outcome, 'absent')
  assert.equal(resolveRef(nodeBox('old'), world()).outcome, 'absent')
  await inAct(async () => { await flush(4) })
  assert.equal(asked.length, 0)
})

async function mountIndex(t: TestContext, view: TreePayload['foreground'], source: unknown, base: MentionIndex) {
  let index: MentionIndex | null = null
  function Probe() { index = useProseAgentIndex('org', source, base); return null }
  const v = await mountView(<ForegroundViewContext.Provider value={view}><Probe /></ForegroundViewContext.Provider>, h => h)
  t.after(() => v.unmount())
  return () => index!
}

test('bare names in prose resolve omitted agents in a bounded lookup; items keep the collision', async (t) => {
  const asked = stubReferences(t, ['old-agent', 'shared-name'])
  const base: MentionIndex = new Map([['shared-name', { kind: 'item', slug: 'shared-name' }],
    ['live', { kind: 'agent', id: 'live', tier: 'haiku' }]])
  const item = { objective: 'ask old-agent about shared-name, not missing-one', notes: ['live erased'] }
  const index = await mountIndex(t, { catalog_revision: 'cat-' + t.name, present: ['live'], missing: ['erased'] },
    item, base)
  await inAct(async () => { await flush(4) })
  const words = asked.flat()
  assert.ok(words.includes('old-agent') && words.includes('missing-one'))
  assert.ok(!words.includes('shared-name') && !words.includes('live') && !words.includes('erased'),
    'known items, known agents and explicit absences are not looked up')
  assert.deepEqual(index().get('old-agent'), { kind: 'agent', id: 'old-agent', tier: 'astra' })
  assert.equal(index().get('shared-name')?.kind, 'item', 'an item keeps winning a name collision')
  assert.equal(index().has('missing-one'), false)
})

test('a complete tree adds nothing and asks nothing; candidate scanning is bounded', async (t) => {
  const asked = stubReferences(t, ['old-agent'])
  const base: MentionIndex = new Map()
  const index = await mountIndex(t, undefined, { objective: 'old-agent' }, base)
  await inAct(async () => { await flush(4) })
  assert.equal(index(), base)
  assert.equal(asked.length, 0)
  const many = Array.from({ length: 1000 }, (_, n) => 'name' + n).join(' ')
  assert.equal(proseCandidates({ text: many }, () => false).length, 256)
  assert.equal(proseCandidates({ text: many }, () => false, 256, 50).length <= 10, true)
})
