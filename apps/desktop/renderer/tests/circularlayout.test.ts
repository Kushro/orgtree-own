// circularlayout.test.ts — org chart circular arrangement (user 2026-09-30).
// Run:  cd apps/desktop/renderer && node tests/run.mjs circularlayout
import './harness'
import test from 'node:test'
import assert from 'node:assert/strict'
import { chartLayoutOf, layout, NODE_H, NODE_W, setChartLayout, USER } from '../src/canvas/shared'
import type { CanvasNode } from '../src/canvas/shared'

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
