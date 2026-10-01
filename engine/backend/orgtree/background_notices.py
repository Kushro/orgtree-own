"""Correlate Claude task notifications with results already returned to the agent."""
from __future__ import annotations

import re
from typing import Any


class BackgroundNotices:
    def __init__(self) -> None:
        self.background: set[str] = set()
        self.task_tools: dict[str, str] = {}
        self.returned: set[str] = set()
        self.consumed: set[str] = set()
        self.reads: dict[str, str] = {}
        self.explicit_background: set[str] = set()

    def observe(self, ev: dict[str, Any]) -> None:
        # Sidechain tool ids belong to a child, not this agent's tool calls.
        if ev.get("parent_tool_use_id"):
            return
        if ev.get("type") == "system":
            subtype = ev.get("subtype")
            rows = ev.get("tasks") if subtype == "background_tasks_changed" else [ev]
            for row in rows if isinstance(rows, list) else []:
                if not isinstance(row, dict):
                    continue
                tid = str(row.get("task_id") or "")
                tool = str(row.get("tool_use_id") or "")
                if not tid:
                    continue
                if tool:
                    self.task_tools[tid] = tool
                if (subtype == "background_tasks_changed"
                        or row.get("is_background") is True
                        or tool in self.explicit_background):
                    self.background.add(tid)
                if subtype == "task_notification":
                    code = exit_code(row)
                    if (row.get("status") == "completed"
                            or (row.get("status") == "failed"
                                and code is not None and code >= 0)):
                        # A normal process exit, including a nonzero exit, is
                        # a completed command; do not later call it a lost job.
                        self.consumed.add(tid)
            return
        content = (ev.get("message") or {}).get("content")
        for block in content if isinstance(content, list) else []:
            if not isinstance(block, dict):
                continue
            if ev.get("type") == "assistant" and block.get("type") == "tool_use":
                tool = str(block.get("id") or "")
                args = block.get("input") or {}
                if not isinstance(args, dict):
                    continue
                if args.get("run_in_background") is True:
                    self.explicit_background.add(tool)
                tid = str(args.get("task_id") or "")
                if tid and block.get("name") == "TaskOutput":
                    self.reads[tool] = tid
                elif tid and block.get("name") == "TaskStop":
                    # The agent requested this stop itself; no second wake is needed.
                    self.consumed.add(tid)
            elif ev.get("type") == "user" and block.get("type") == "tool_result":
                tool = str(block.get("tool_use_id") or "")
                value = block.get("content")
                text = value if isinstance(value, str) else "\n".join(
                    str(b.get("text") or "") for b in value
                    if isinstance(b, dict) and b.get("type") == "text"
                ) if isinstance(value, list) else ""
                # Launch acknowledgements are results too, but do not mean the
                # background job has finished. TaskOutput can also time out.
                pending = ("Command running in background with ID:" in text
                           or "Async agent launched successfully" in text
                           or "<retrieval_status>timeout</retrieval_status>" in text
                           or "<status>running</status>" in text)
                if not pending:
                    self.returned.add(tool)
                    if tool in self.reads and not block.get("is_error"):
                        self.consumed.add(self.reads[tool])

    def should_report(self, ev: dict[str, Any]) -> bool:
        tid = str(ev.get("task_id") or "")
        tool = str(ev.get("tool_use_id") or self.task_tools.get(tid) or "")
        return (not ev.get("parent_tool_use_id")
                and ev.get("subtype") == "task_notification"
                and ev.get("status") in ("failed", "stopped", "cancelled", "killed")
                and tid in self.background and tid not in self.consumed
                and (not tool or tool not in self.returned))


def exit_code(ev: dict[str, Any]) -> int | None:
    code = ev.get("exit_code")
    if isinstance(code, int) and not isinstance(code, bool):
        return code
    match = re.search(r"exit(?:[ -]code)?\s*[:=]?\s*(-?\d+)", str(ev.get("summary") or ""), re.I)
    return int(match.group(1)) if match else None


def stop_summary(ev: dict[str, Any]) -> str:
    """Keep the CLI's exit code, including zero; never invent a missing one."""
    summary = str(ev.get("summary") or "")
    code = exit_code(ev)
    return (f"status: {ev.get('status')}; exit code: "
            f"{code if code is not None else 'unavailable (not reported by CLI)'}"
            + (f"; {summary}" if summary else ""))
