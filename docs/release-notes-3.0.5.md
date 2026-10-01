# Orgtree 3.0.5

- **Jump cards fold later, and you can turn it off.** A desk now folds its
  jump cards (reports and watchdogs) into one counted button only when it has
  more than 16, instead of 8 or more. The new switch **Collapse many jump
  cards on desks** in App settings > Display turns this off; it is on by
  default.
- **Reply quote button removed.** The "Remove retained reply quotes" button
  is gone from agent settings.
- **Live effort changes are safer.** Changing an agent's effort, or
  interrupting it, while it is mid-turn can no longer garble what Orgtree is
  sending to the Claude process.
- **Forwarded reports wake the superior.** When an agent submits a report to
  its superior, the superior now gets a turn to read it instead of the report
  waiting unread in an idle mailbox.
- **Moving an agent checks permission first.** Asking to move an agent under
  the superior it already has no longer reveals that superior to an agent
  with no authority over it.
- **Agents with self-only visibility know their superior.** Their identity
  prompt now names their superior, matching what each turn already showed.

This update contains only these six changes on top of 3.0.4.
