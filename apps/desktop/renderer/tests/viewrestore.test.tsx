// viewrestore.test.tsx — restore-previous-windows returns to the Attention view.
// Run:  cd apps/desktop/renderer && node tests/run.mjs viewrestore
import './harness'
import test from 'node:test'
import assert from 'node:assert/strict'
import { planViewRestore } from '../src/attention/viewrestore'
import { ORG_VIEW_KEY, forgetAttentionMode, orgView, setOrgView } from '../src/attention/mode'
import type { NativePreferences } from '../src/desktop'

const prefs = (p: Partial<NativePreferences>): NativePreferences => p as NativePreferences

test('a window closed in Attention reopens in Attention even with the local view key gone', () => {
  localStorage.clear(); forgetAttentionMode()
  assert.equal(localStorage.getItem(ORG_VIEW_KEY), null)
  const plan = planViewRestore('acme', orgView('acme'), prefs({ startupMode: 'restore', attentionOrgs: ['acme'] }), true)
  assert.equal(plan.kind, 'restore')
  setOrgView('acme', 'attention')
  assert.equal(orgView('acme'), 'attention')
})

test('a window closed in Canvas reopens in Canvas', () => {
  const plan = planViewRestore('acme', 'canvas', prefs({ startupMode: 'restore', attentionOrgs: ['other'] }), true)
  assert.equal(plan.kind, 'none')
})

test('homepage startup does not restore, and canvas choice clears the saved view', () => {
  const plan = planViewRestore('acme', 'canvas', prefs({ startupMode: 'homepage', attentionOrgs: ['acme'] }), true)
  assert.deepEqual(plan, { kind: 'mirror', attentionOrgs: [] })
})

test('after startup the live view is mirrored to the native preference', () => {
  assert.deepEqual(planViewRestore('acme', 'attention', prefs({ attentionOrgs: ['x'] }), false),
    { kind: 'mirror', attentionOrgs: ['x', 'acme'] })
  assert.deepEqual(planViewRestore('acme', 'canvas', prefs({ attentionOrgs: ['acme', 'x'] }), false),
    { kind: 'mirror', attentionOrgs: ['x'] })
  assert.equal(planViewRestore('acme', 'attention', prefs({ attentionOrgs: ['acme'] }), false).kind, 'none')
})

test('a user switch to Canvas after startup is not undone by the restore', () => {
  assert.equal(planViewRestore('acme', 'canvas', prefs({ startupMode: 'restore', attentionOrgs: ['acme'] }), false).kind, 'mirror')
})
