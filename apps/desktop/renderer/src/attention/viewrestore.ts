// attention/viewrestore.ts — RESTORE THE CANVAS/ATTENTION VIEW AT STARTUP.
//
// The view lives in this origin's localStorage, which a relaunch can lose
// (a moved engine port or a new profile). The native preferences document
// survives that, so the view is mirrored there and read back at startup when
// restore-previous-windows is on.

import { useEffect, useRef } from 'react'
import { desktop, startupMode } from '../desktop'
import type { NativePreferences } from '../desktop'
import { setOrgView } from './mode'
import type { OrgView } from './mode'

export type ViewRestorePlan =
  | { kind: 'restore' }
  | { kind: 'mirror'; attentionOrgs: string[] }
  | { kind: 'none' }

/** What to do for one org window. `first` is true until the first decision.
 *  The first decision may restore; every later one only mirrors the live view. */
export function planViewRestore(
  slug: string, view: OrgView, prefs: NativePreferences, first: boolean,
): ViewRestorePlan {
  const saved = prefs.attentionOrgs ?? []
  const has = saved.includes(slug)
  if (first && startupMode(prefs) === 'restore' && has && view === 'canvas') return { kind: 'restore' }
  if ((view === 'attention') === has) return { kind: 'none' }
  return {
    kind: 'mirror',
    attentionOrgs: view === 'attention' ? [...saved, slug] : saved.filter(s => s !== slug),
  }
}

export function useViewRestore(slug: string | null, view: OrgView, prefs: NativePreferences | null): void {
  const first = useRef(true)
  useEffect(() => {
    if (!slug || !prefs) return
    const plan = planViewRestore(slug, view, prefs, first.current)
    first.current = false
    if (plan.kind === 'restore') setOrgView(slug, 'attention')
    else if (plan.kind === 'mirror') desktop()?.setPreferences({ attentionOrgs: plan.attentionOrgs }).catch(() => {})
  }, [slug, view, prefs])
}
