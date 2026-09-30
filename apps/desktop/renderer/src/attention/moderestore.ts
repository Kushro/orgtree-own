// attention/moderestore.ts — RESTORE THE CANVAS/ATTENTION VIEW AT STARTUP.
//
// The view lives in this origin's localStorage, which a relaunch can lose
// (a moved engine port or a new profile). The native preferences document
// survives that, so the view is mirrored there as `attentionOrgs` (the orgs
// last left in Attention) and read back at startup.
//
// Precedence at the first preferences read, startup mode 'restore':
//   attentionOrgs present  -> the native record wins, for BOTH views
//   attentionOrgs absent   -> older document: keep the local view and record it
// Startup mode 'homepage' never restores; the live view is only recorded.
// After that first decision the live view is only mirrored, never overridden.

import { useEffect, useRef } from 'react'
import { startupMode } from '../desktop'
import type { NativePreferences } from '../desktop'
import { setOrgView } from './mode'
import type { OrgView } from './mode'

export type ModeRestorePlan =
  | { kind: 'apply'; view: OrgView }
  | { kind: 'mirror'; attentionOrgs: string[] }
  | { kind: 'none' }

export function planModeRestore(
  slug: string, view: OrgView, prefs: NativePreferences, first: boolean,
): ModeRestorePlan {
  const saved = prefs.attentionOrgs
  if (first && saved && startupMode(prefs) === 'restore') {
    const want: OrgView = saved.includes(slug) ? 'attention' : 'canvas'
    return want === view ? { kind: 'none' } : { kind: 'apply', view: want }
  }
  const list = saved ?? []
  const has = list.includes(slug)
  if (saved && (view === 'attention') === has) return { kind: 'none' }
  if (!saved && view === 'canvas') return { kind: 'mirror', attentionOrgs: [] }
  return {
    kind: 'mirror',
    attentionOrgs: view === 'attention' ? [...list, slug] : list.filter(s => s !== slug),
  }
}

export function useModeRestore(
  slug: string | null, view: OrgView, prefs: NativePreferences | null,
  persist: (attentionOrgs: string[]) => void,
): void {
  const first = useRef(true)
  useEffect(() => {
    if (!slug || !prefs) return
    const plan = planModeRestore(slug, view, prefs, first.current)
    first.current = false
    if (plan.kind === 'apply') setOrgView(slug, plan.view)
    else if (plan.kind === 'mirror') persist(plan.attentionOrgs)
  }, [slug, view, prefs, persist])
}
