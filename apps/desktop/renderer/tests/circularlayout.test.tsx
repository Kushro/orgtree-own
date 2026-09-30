// circularlayout.test.ts — org chart circular arrangement (user 2026-09-30).
// Run:  cd apps/desktop/renderer && node tests/run.mjs circularlayout
declare const __SRC_DIR__: string
import { inAct, mountView } from './harness'
import test from 'node:test'
import assert from 'node:assert/strict'
import { chartLayoutOf, layout, NODE_H, NODE_W, peerOrder, setChartLayout, USER } from '../src/canvas/shared'
import type { CanvasNode } from '../src/canvas/shared'
import { ChartLayoutSetting } from '../src/canvas/accounts'

const node = (id: string, children: CanvasNode[] = []): CanvasNode =>
  ({ id, title: id, tier: 't', state: 'live', children } as unknown as CanvasNode)
const eye = (kids: CanvasNode[]): CanvasNode =>
  ({ id: USER, title: 'you', tier: null, state: 'user', children: kids } as CanvasNode)
const tree = (fan: number, depth: number, pre = 'n'): CanvasNode[] =>
  depth === 0 ? [] : Array.from({ length: fan }, (_, i) => node(`${pre}${i}`, tree(fan, depth - 1, `${pre}${i}.`)))
const centre = (p: { x: number; y: number }) => ({ x: p.x + NODE_W / 2, y: p.y + NODE_H / 2 })
const overlaps = (t: Map<string, { x: number; y: number }>) => {
  const ps = [...t.values()].sort((a, b) => a.x - b.x)
  for (let i = 0; i < ps.length; i++)
    for (let j = i + 1; j < ps.length && ps[j]!.x - ps[i]!.x < NODE_W; j++)
      if (Math.abs(ps[j]!.y - ps[i]!.y) < NODE_H) return true
  return false
}

test('row is the default and the setting round-trips', () => {
  localStorage.removeItem('orgtree-chart-layout')
  assert.equal(chartLayoutOf(), 'row')
  setChartLayout('circular'); assert.equal(chartLayoutOf(), 'circular')
  setChartLayout('row'); assert.equal(chartLayoutOf(), 'row')
})

test('row mode is unchanged: circular is only used when asked', () => {
  const root = eye(tree(3, 2))
  assert.deepEqual([...layout(root)], [...layout(root, new Map(), 'row')])
  assert.notDeepEqual([...layout(root)], [...layout(root, new Map(), 'circular')])
})

test('eye at centre, depths on increasing rings, every agent placed', () => {
  const root = eye(tree(4, 3))
  const t = layout(root, new Map(), 'circular')
  assert.equal(t.size, 1 + 4 + 16 + 64)
  const c = centre(t.get(USER)!)
  const rad = (id: string) => Math.hypot(centre(t.get(id)!).x - c.x, centre(t.get(id)!).y - c.y)
  for (let i = 0; i < 4; i++) assert.ok(Math.abs(rad(`n${i}`) - rad('n0')) < 1e-6)
  assert.ok(rad('n0.0') > rad('n0') && rad('n0.0.0') > rad('n0.0'))
  assert.equal(overlaps(t), false)
})

test('a team sits in the wedge behind its parent, sized by team size', () => {
  const root = eye([node('big', tree(5, 1, 'b')), node('small', [node('s0')])])
  const t = layout(root, new Map(), 'circular')
  const c = centre(t.get(USER)!)
  const ang = (id: string) => Math.atan2(centre(t.get(id)!).y - c.y, centre(t.get(id)!).x - c.x)
  const wrap = (a: number) => Math.atan2(Math.sin(a), Math.cos(a))
  // a child lies within its parent's wedge: 5/6 of the circle for big, so
  // each of its kids is within half that of the parent's angle
  for (let i = 0; i < 5; i++) assert.ok(Math.abs(wrap(ang(`b${i}`) - ang('big'))) <= Math.PI * 5 / 6 + 1e-9)
  assert.ok(Math.abs(wrap(ang('s0') - ang('small'))) < 1e-9)
})

test('1500 agents: no overlap and cheap', () => {
  const root = eye(tree(10, 1).concat(Array.from({ length: 50 }, (_, i) => node(`w${i}`, tree(5, 1, `w${i}.`).concat(tree(2, 2, `w${i}x`)))))) 
  const t0 = performance.now()
  const t = layout(root, new Map(), 'circular')
  const ms = performance.now() - t0
  assert.ok(t.size > 600)
  assert.equal(overlaps(t), false)
  assert.ok(ms < 200, `took ${ms}ms`)
})

test('hidden subtrees take no space', () => {
  const root = eye([node('a', [node('a0')]), node('b')])
  const t = layout(root, new Map([['a0', 'a']]), 'circular')
  assert.ok(!t.has('a0'))
})

test('the Display setting shows, saves and follows the stored layout', async (t) => {
  localStorage.removeItem('orgtree-chart-layout')
  t.after(() => localStorage.removeItem('orgtree-chart-layout'))
  const view = await mountView(<ChartLayoutSetting />, el => el)
  t.after(() => view.unmount())
  const sel = () => view.el.querySelector('select[aria-label="Org chart layout"]') as HTMLSelectElement
  assert.equal(sel().value, 'row', 'unset shows Row')
  await inAct(() => {
    sel().value = 'circular'
    sel().dispatchEvent(new (globalThis as unknown as { window: { Event: typeof Event } }).window.Event('change', { bubbles: true }))
  })
  assert.equal(localStorage.getItem('orgtree-chart-layout'), 'circular')
  assert.equal(sel().value, 'circular')
  await inAct(() => { setChartLayout('row') })
  assert.equal(sel().value, 'row', 'another writer flips the mounted select')
})

test('ring neighbour lines join angular neighbours, not an up-and-down zig-zag', () => {
  const kids = Array.from({ length: 12 }, (_, i) => node(`k${i}`))
  const root = eye(kids)
  const t = layout(root, new Map(), 'circular')
  const ids = kids.map(k => k.id)
  const gap = (order: string[]) => {
    const d: number[] = []
    for (let i = 0; i + 1 < order.length; i++) {
      const a = centre(t.get(order[i]!)!), b = centre(t.get(order[i + 1]!)!)
      d.push(Math.hypot(a.x - b.x, a.y - b.y))
    }
    return d
  }
  const ring = gap(peerOrder(ids, t, true))
  assert.ok(Math.max(...ring) - Math.min(...ring) < 1, 'every ring link is one equal step along the ring')
  const row = gap(peerOrder(ids, t, false))
  assert.ok(Math.max(...row) > 1.5 * Math.max(...ring), 'the row order (sorted by x) would jump up and down the ring sides')
  const sorted = peerOrder(['b', 'a'], new Map([['b', { x: 5, y: 0 }], ['a', { x: 1, y: 9 }]]), false)
  assert.deepEqual(sorted, ['a', 'b'], 'row mode still reads left to right')
})

test('the animated sibling route uses the same neighbour order as the drawn lines', async () => {
  const { readFileSync } = await import('node:fs')
  const path = await import('node:path')
  const src = readFileSync(path.join(__SRC_DIR__, 'canvas', 'OrgCanvas.tsx'), 'utf8').split('\r\n').join('\n')
  const at = src.indexOf('const launchSpark = useCallback')
  const body = src.slice(at, src.indexOf('}, [])', at))
  assert.match(body, /peerOrder\(/, 'the route orders siblings through peerOrder')
  assert.doesNotMatch(body, /\.sort\(/, 'and does not x-sort them itself')
  assert.match(body, /wireRef\.current\.peerSeg/, 'with the latest wire builders, since launchSpark is memoised once')
})
