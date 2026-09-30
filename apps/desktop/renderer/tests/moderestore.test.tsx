// moderestore.test.tsx — restore-previous-windows returns to the Attention view.
// Mounts the real hook. Run:  cd apps/desktop/renderer && node tests/run.mjs moderestore
import { inAct, mountView } from './harness'
import test from 'node:test'
import assert from 'node:assert/strict'
import { useModeRestore } from '../src/attention/moderestore'
import { forgetAttentionMode, orgView, setOrgView, useOrgView } from '../src/attention/mode'
import type { NativePreferences } from '../src/desktop'

const prefs = (p: Partial<NativePreferences>): NativePreferences => p as NativePreferences
const reset = () => { localStorage.clear(); forgetAttentionMode() }

function Probe({ slug, p, persist }: { slug: string; p: NativePreferences | null; persist: (o: string[]) => void }) {
  const view = useOrgView(slug)
  useModeRestore(slug, view, p, persist)
  return <i data-view={view} />
}
const seen = (el: HTMLElement) => el.querySelector('i')!.getAttribute('data-view')

async function boot(local: 'attention' | 'canvas', native: Partial<NativePreferences>) {
  reset(); if (local === 'attention') setOrgView('acme', 'attention')
  const saved: string[][] = []
  const persist = (o: string[]) => { saved.push(o) }
  const view = await mountView(<Probe slug="acme" p={null} persist={persist} />, seen)
  assert.equal(saved.length, 0, 'nothing is decided before preferences arrive')
  await view.render(<Probe slug="acme" p={prefs(native)} persist={persist} />)
  return { view, saved }
}

test('native Attention restores Attention with the local view key gone', async () => {
  const { view, saved } = await boot('canvas', { startupMode: 'restore', attentionOrgs: ['acme'] })
  assert.equal(view.last(), 'attention'); assert.equal(orgView('acme'), 'attention')
  assert.deepEqual(saved, [])
  await view.unmount()
})

test('native Canvas (empty list) beats a stale local Attention', async () => {
  const { view, saved } = await boot('attention', { startupMode: 'restore', attentionOrgs: [] })
  assert.equal(view.last(), 'canvas')
  assert.deepEqual(saved, [], 'the saved Canvas is not overwritten')
  await view.unmount()
})

test('a window closed in Canvas reopens in Canvas', async () => {
  const { view } = await boot('canvas', { startupMode: 'restore', attentionOrgs: ['other'] })
  assert.equal(view.last(), 'canvas')
  await view.unmount()
})

test('older native preferences without attentionOrgs keep the local view and record it', async () => {
  const { view, saved } = await boot('attention', { startupMode: 'restore' })
  assert.equal(view.last(), 'attention')
  assert.deepEqual(saved, [['acme']])
  await view.unmount()
})

test('homepage startup does not restore; the live view is recorded instead', async () => {
  const { view, saved } = await boot('canvas', { startupMode: 'homepage', attentionOrgs: ['acme'] })
  assert.equal(view.last(), 'canvas')
  assert.deepEqual(saved, [[]])
  await view.unmount()
})

test('a user switch after startup is mirrored and never undone by the restore', async () => {
  const { view, saved } = await boot('canvas', { startupMode: 'restore', attentionOrgs: ['acme'] })
  assert.equal(view.last(), 'attention')
  await inAct(() => setOrgView('acme', 'canvas'))
  assert.equal(view.last(), 'canvas')
  await view.render(<Probe slug="acme" p={prefs({ startupMode: 'restore', attentionOrgs: ['acme'] })} persist={o => saved.push(o)} />)
  assert.equal(view.last(), 'canvas')
  assert.ok(saved.some(o => o.length === 0), 'Canvas was recorded')
  await view.unmount()
})
