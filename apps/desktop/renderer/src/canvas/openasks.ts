// canvas/openasks.ts — EVERY OPEN REQUEST WAITING ON THE USER, from one rule.
//
// Two surfaces list the user's open questions: the inbox (as ask rows among
// the mail) and the Attention view. They used to read them only off
// `node.ask`, the batched desk card the tree carries on each node. In v3 the
// tree is a SELECTED read (foregroundtree.ts: "only missing_requested proves
// absence"), so a node outside the selection has no `node.ask` in the payload
// at all — and its open question was listed nowhere (user report 2026-09-29:
// a question card was open while the Attention view listed only a ticket and
// an urgent mail).
//
// The header's `tree.asks` is not selected: it carries EVERY open row
// (ledger header: open questions, pending credit and scope requests) plus the
// recent resolved history. So:
//   • a node that IS in the tree keeps its batched card (FR-14: one card per
//     agent, the union of its open question, credit and scope requests);
//   • every open header row whose node has no batched card here is listed on
//     its own, as the raw row — the same rows the docket already renders with
//     the real AskCard for questions attached to an item.
// Both surfaces call this, so they cannot disagree about what is waiting.

import type { AskInfo, TreeNode, TreePayload } from '../types'
import type { MailRow } from './shared'

export const askIsOpen = (a: AskInfo | undefined | null): boolean =>
  !!a && (a.status === 'open' || a.status === 'pending')

/** The open requests to list, one entry per batched card or unbatched row.
 *  `nodes` is every node of the tree, flattened. */
export function openAsks(tree: Pick<TreePayload, 'asks'> | null | undefined,
  nodes: Iterable<TreeNode>): AskInfo[] {
  const out: AskInfo[] = []
  const batched = new Set<string>()
  for (const n of nodes) {
    if (askIsOpen(n.ask)) { out.push(n.ask!); batched.add(n.id) }
  }
  for (const a of tree?.asks ?? []) {
    if (askIsOpen(a) && !batched.has(a.node)) out.push(a)
  }
  return out
}

/** An open or resolved request as a row of the user's mailbox (user ruling
 *  2026-08-04: asks ride the inbox as their OWN mail rows, interleaved with
 *  real mail; the reading pane shows the response UI instead of a reply box).
 *  The Attention view lists questions with this same row. */
export const askMailRow = (a: AskInfo): MailRow => ({
  id: 'ask:' + a.id, from: a.node, at: a.at,
  kind: a.kind === 'batch' ? 'request batch'
    : (a.kind === 'credit' || a.old != null) ? 'credit request'
    : a.kind === 'scope' ? 'scope request' : 'question',
  body: a.kind === 'batch'
    ? `${(a.tabs ?? []).length} request(s) awaiting one submit`
    : a.kind === 'scope'
      ? 'requests scope: ' + (a.items ?? [])
        .map((it) => it.kind === 'dir' ? it.path
          : it.kind === 'permission_mode' ? `mode ${it.mode}`
          : it.tool ?? it.server ?? it.kind).join(', ')
      : a.question ?? `asks for credits: ${a.old} → ${a.new}`,
  _ask: a,
} as MailRow)
