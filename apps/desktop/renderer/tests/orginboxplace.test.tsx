// orginboxplace.test.tsx — user report 2026-10-01 (3.0.9): "in circular view,
// when there are enough agents at the right depth, they can overlap this [the
// org inbox]. can you make it so that no matter what the arrangement of agents
// is, this always stays far enough out from the center to not overlap
// anything?" Ruling: conservative — the inbox keeps its usual place while
// nothing is drawn there, moves out only as far as it must, and comes back.
// Run:  cd apps/desktop/renderer && node tests/run.mjs orginboxplace
declare const __SRC_DIR__: string
import test from 'node:test'
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import path from 'node:path'
import { INBOX, INBOX_CLEAR, INBOX_H, layout, placeOrgInbox, sizeOf, USER, USER_W } from '../src/canvas/shared'
import type { CanvasNode, ChartLayout } from '../src/canvas/shared'

type Pt = { x: number; y: number }
const node = (id: string, children: CanvasNode[] = []): CanvasNode =>
  ({ id, title: id, tier: 't', state: 'live', children } as unknown as CanvasNode)
const eye = (kids: CanvasNode[]): CanvasNode =>
  ({ id: USER, title: 'you', tier: null, state: 'user', children: kids } as CanvasNode)
const flat = (n: number, pre = 'k') => Array.from({ length: n }, (_, i) => node(`${pre}${i}`))
const tree = (fan: number, depth: number, pre = 'n'): CanvasNode[] =>
  depth === 0 ? [] : Array.from({ length: fan }, (_, i) => node(`${pre}${i}`, tree(fan, depth - 1, `${pre}${i}.`)))

const usual = (t: Map<string, Pt>): Pt => {
  const e = t.get(USER)!
  return { x: e.x + USER_W + 260, y: e.y - INBOX_H - 96 }
}
// the smallest gap between the inbox and any other card (negative = overlap)
const clearance = (t: Map<string, Pt>, at: Pt): number => {
  const iw = USER_W, ih = INBOX_H
  let min = Infinity
  for (const [id, p] of t) {
    if (id === INBOX) continue
    const { w, h } = sizeOf(id)
    const gx = Math.max(p.x - (at.x + iw), at.x - (p.x + w))
    const gy = Math.max(p.y - (at.y + ih), at.y - (p.y + h))
    min = Math.min(min, Math.max(gx, gy))
  }
  return min
}
const place = (root: CanvasNode, mode: ChartLayout) => {
  const t = layout(root, new Map(), mode)
  return { t, at: placeOrgInbox(t)! }
}
const same = (a: Pt, b: Pt) => Math.abs(a.x - b.x) < 1e-6 && Math.abs(a.y - b.y) < 1e-6

const arrangements: [string, CanvasNode][] = [
  ...[1, 2, 3, 4, 6, 8, 9, 12, 20, 40].map((n) => [`${n} top-level`, eye(flat(n))] as [string, CanvasNode]),
  ['4x2', eye(tree(4, 2))],
  ['10x2', eye(tree(10, 2))],
  ['4x3', eye(tree(4, 3))],
  ['a big second ring weighted toward the inbox angle (it encircles it)', eye([node('a', flat(2, 'a')), node('b', flat(40, 'b')), node('c', flat(1, 'c'))])],
  ['a big second ring weighted away from it', eye([node('a', flat(40, 'a')), node('b', flat(2, 'b')), node('c', flat(1, 'c'))])],
  ['1500 agents', eye(tree(10, 1).concat(Array.from({ length: 50 }, (_, i) => node(`w${i}`, tree(5, 1, `w${i}.`).concat(tree(2, 2, `w${i}x`))))))],
]

test('circle view: the org inbox clears every card by the margin, whatever the rings hold', () => {
  let moved = 0
  for (const [name, root] of arrangements) {
    const { t, at } = place(root, 'circular')
    const c = clearance(t, at)
    assert.ok(c >= INBOX_CLEAR - 1e-6, `${name}: inbox is ${c.toFixed(1)}px from the nearest card (needs ${INBOX_CLEAR})`)
    if (!same(at, usual(t))) moved++
  }
  assert.ok(moved >= 3, `the crowded cases really did reach the usual place (${moved} moved)`)
})

test('circle view: the inbox keeps its usual place while nothing is drawn there, and comes back when the arc shrinks', () => {
  for (const n of [1, 2, 3, 4]) {
    const { t, at } = place(eye(flat(n)), 'circular')
    assert.ok(same(at, usual(t)), `${n} agents in a bottom arc: the inbox stays at its usual place`)
  }
  // a deeper ring that does not reach the inbox does not move it either
  const deep = place(eye([node('a', flat(3, 'a'))]), 'circular')
  assert.ok(same(deep.at, usual(deep.t)), 'a sparse deeper ring leaves it alone')
  // grow the first ring until it reaches the inbox, then shrink it back
  let reached = 0
  for (let n = 1; n <= 30 && !reached; n++) {
    const { t, at } = place(eye(flat(n)), 'circular')
    if (!same(at, usual(t))) reached = n
  }
  assert.ok(reached > 4, `the inbox only moves once the ring grows round to it (at ${reached})`)
  const back = place(eye(flat(3)), 'circular')
  assert.ok(same(back.at, usual(back.t)), 'and returns to its usual place when the ring shrinks')
})

// user ruling 2 (2026-10-01): a ring so large that it passes round OUTSIDE the
// usual place leaves the inbox where it is, inside the ring near the centre
test('circle view: a big ring that encircles the usual place does not move the inbox', () => {
  const r = (t: Map<string, Pt>, id: string) => {
    const e = t.get(USER)!, p = t.get(id)!
    return Math.hypot(p.x - e.x, p.y - e.y)
  }
  for (const [name, root, id] of [
    ['40 top-level agents', eye(flat(40)), 'k0'],
    ['one agent with 60 reports', eye([node('a', flat(60, 'a'))]), 'a0'],
  ] as const) {
    const { t, at } = place(root, 'circular')
    const u = usual(t), e = t.get(USER)!
    assert.ok(r(t, id) > Math.hypot(u.x - e.x, u.y - e.y) + 300, `${name}: the ring really passes outside the usual place`)
    assert.ok(same(at, u), `${name}: the inbox stays at its usual place, inside the ring`)
    assert.ok(clearance(t, at) >= INBOX_CLEAR - 1e-6, `${name}: and clear of every card`)
  }
})

test('circle view: when it moves, it moves along the same line and only as far as it must', () => {
  for (const [name, root] of arrangements) {
    const { t, at } = place(root, 'circular')
    const u = usual(t)
    if (same(at, u)) continue
    const e = t.get(USER)!
    const o = { x: e.x + USER_W / 2, y: e.y + USER_W / 2 }
    const du = { x: u.x + USER_W / 2 - o.x, y: u.y + INBOX_H / 2 - o.y }
    const da = { x: at.x + USER_W / 2 - o.x, y: at.y + INBOX_H / 2 - o.y }
    assert.ok(Math.abs(du.x * da.y - du.y * da.x) < 1e-6 * Math.hypot(du.x, du.y) * Math.hypot(da.x, da.y),
      `${name}: same direction from the eye`)
    const s = Math.hypot(da.x, da.y) / Math.hypot(du.x, du.y)
    assert.ok(s > 1, `${name}: outward`)
    // a little nearer in would be inside the margin of some card
    const near = { x: o.x + du.x * (s - 0.01) - USER_W / 2, y: o.y + du.y * (s - 0.01) - INBOX_H / 2 }
    assert.ok(clearance(t, near) < INBOX_CLEAR, `${name}: no nearer spot on the line clears the cards`)
  }
})

test('row view: the inbox keeps its place, and that place clears every card', () => {
  for (const [name, root] of arrangements) {
    const { t, at } = place(root, 'row')
    assert.ok(same(at, usual(t)), `${name}: row placement unchanged`)
    assert.ok(clearance(t, at) >= INBOX_CLEAR - 1e-6, `${name}: and clear of every card`)
  }
})

test('a satellite card (watchdog) laid out at the usual place also pushes the inbox out', () => {
  const t = layout(eye(flat(2)), new Map(), 'circular')
  const u = usual(t)
  t.set('dog:w1', { x: u.x + 10, y: u.y + 10 })
  const at = placeOrgInbox(t)!
  assert.ok(!same(at, u) && clearance(t, at) >= INBOX_CLEAR - 1e-6)
})

test('OrgCanvas places the inbox through placeOrgInbox, after every other card', () => {
  const src = readFileSync(path.join(__SRC_DIR__, 'canvas', 'OrgCanvas.tsx'), 'utf8').split('\r\n').join('\n')
  const at = src.indexOf('const target = useMemo(')
  const body = src.slice(at, src.indexOf('return t\n', at))
  const inbox = body.indexOf('placeOrgInbox(t)')
  assert.ok(inbox > 0, 'the target layout uses placeOrgInbox')
  for (const before of ["t.set('dog:'", 'n.isBearerOf && t.has'])
    assert.ok(body.indexOf(before) > 0 && body.indexOf(before) < inbox, `${before} is laid out before the inbox`)
  assert.equal(body.indexOf('USER_W + 260'), -1, 'no second copy of the old fixed offset')
})
