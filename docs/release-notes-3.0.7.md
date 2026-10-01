# Orgtree 3.0.7

## Agent tool fixes

- **No false "background task stopped" notices.** Agents are no longer told
  that a background command stopped when it had already returned or finished
  normally.
- **Review mail names the right commit.** Review requests and verdicts name
  the exact commit under review, review seats are checked again before a
  re-review, and a finished landing frees its slot.
- **Smaller, clearer docket reads.** Reading a ticket returns a compact view
  by default, with a note saying how to ask for the rest. Claim guidance,
  the result of recording a finding, and check receipts are clearer and
  shorter.
- **Dismissing a flag keeps a reviewed ticket reviewed.** When the user
  dismisses an attention flag on a ticket that was already approved, the
  approval is kept.

## Desktop fixes

- **Attention view opens a flagged ticket at the bottom.** Selecting a
  flagged ticket (by click, key, notification, or automatically) scrolls it
  to the bottom once it has fully loaded, so the attention reason is in view.
- **Temporarily opened desks lose the "opened temporarily" label.**
- **The window can be dragged by its top bar again** when a panel is pinned
  flush to the top of the canvas. The panel's top resize handles no longer
  reach into the top bar.
