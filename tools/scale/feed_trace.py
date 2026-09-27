"""Opt-in, bounded stage receipts for disposable scale diagnostics only.

No payload bodies are retained. The existing loader's numbered markers join
server stages to first client receipts. Timestamping uses the host wall clock
for cross-process joins and monotonic time for within-process durations.
"""
from __future__ import annotations

import functools
import re
import threading
import time

MARKER = re.compile(r'\[\[m(\d+)\]\]')


class Trace:
    def __init__(self, limit=200_000):
        self.limit = limit
        self.rows = []
        self.dropped = 0
        self.lock = threading.Lock()
        self.local = threading.local()

    def note(self, stage, marker, window=None):
        if marker is None:
            return
        row = {"stage": stage, "m": marker, "wall_ns": time.time_ns(),
               "mono_ns": time.monotonic_ns(), "thread": threading.get_ident()}
        if window is not None:
            row['w'] = window
        with self.lock:
            if len(self.rows) < self.limit:
                self.rows.append(row)
            else:
                self.dropped += 1

    def snapshot(self):
        with self.lock:
            return {"rows": list(self.rows), "dropped": self.dropped, "limit": self.limit}


def marker(text):
    match = MARKER.search(str(text))
    return int(match.group(1)) if match else None


def install(api, supervisor, assistant_messages, reply_events):
    """Install only in the scale server, after its disposable-root guard."""
    trace = Trace()
    capture = supervisor.capture_reply_stream

    @functools.wraps(capture)
    def traced_capture(slug, nid, payload):
        previous = getattr(trace.local, 'marker', None)
        current = marker(payload.get('text', ''))
        trace.local.marker = current
        trace.note('capture_start', current)
        try:
            return capture(slug, nid, payload)
        finally:
            trace.note('capture_end', current)
            trace.local.marker = previous

    supervisor.capture_reply_stream = traced_capture

    def stage(module, name, label):
        original = getattr(module, name)
        @functools.wraps(original)
        def wrapped(*args, **kwargs):
            current = getattr(trace.local, 'marker', None)
            trace.note(label + '_start', current)
            try:
                return original(*args, **kwargs)
            finally:
                trace.note(label + '_end', current)
        setattr(module, name, wrapped)

    stage(reply_events, 'identity', 'identity')
    stage(assistant_messages, 'scope_ident', 'scope')
    stage(assistant_messages, 'observe', 'observe')
    stage(reply_events, 'annotate_ident', 'annotate')
    send = api.hub._send

    async def traced_send(slug, payload):
        current = marker(payload.get('text', '')) if payload.get('type') == 'node_stream' else None
        trace.note('hub_start', current)
        try:
            return await send(slug, payload)
        finally:
            trace.note('hub_end', current)

    api.hub._send = traced_send
    join = api.hub.join

    async def traced_join(slug, ws, **kwargs):
        send_text = ws.send_text
        window = api._ws_window_id(ws)
        async def traced_text(text):
            # Original delta text precedes the cumulative assistant_row; the
            # first marker is the current delta, not a previously seen one.
            current = marker(text) if text.startswith('{"type":"node_stream"') else None
            trace.note('send_start', current, window)
            try:
                return await send_text(text)
            finally:
                trace.note('send_end', current, window)
        ws.send_text = traced_text
        return await join(slug, ws, **kwargs)

    api.hub.join = traced_join
    return trace
