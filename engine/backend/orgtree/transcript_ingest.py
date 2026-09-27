"""Capture transcript records independently of whether an agent desk is open.

Active conversations are caught up every second and at both turn boundaries.
Existing histories are imported in bounded background slices; projection into
UI messages remains demand-driven. Provider files are retained for CLI resume.

Idle cost (slice C, engine-cpu-per-request-at-n-100): a backfill slice used to
open about five database connections and hash about 12 KB of files for every
node on every pass, even when nothing had changed ("8 agents per second,
forever"). Now a slice that finds the node SETTLED — every source fully
imported, as of a pass whose files had exactly this path, size and mtime — is
skipped with a few stat() calls. File or conversation identity changes make
the next slice run as before. Only ingestion backs off: stat checks keep the
same cadence, so no extra sleep delays a newly appended record. At a stable N,
idle nodes are visited every ceil(N / 8) one-second sleeps plus sweep work;
busy nodes still get one-second capture and both turn-boundary captures.
"""
from __future__ import annotations

import collections
import logging
import os
import threading
import time

_log = logging.getLogger(__name__)
_lock = threading.Lock()
_started = False
_fresh: set[str] = set()
#: Bounded (slug, nid) -> (settled fingerprint, source keys); durable data is untouched.
_SETTLED_LIMIT = 512
_settled: dict[tuple[str, str], tuple] = collections.OrderedDict()


def _remember_settled(key, value):
    # The source database remains authoritative. Eviction only causes another
    # bounded signature/offset check; it never discards transcript records.
    _settled.pop(key, None)
    _settled[key] = value
    while len(_settled) > _SETTLED_LIMIT:
        _settled.pop(next(iter(_settled)))


def _stat(path):
    if not path:
        return None
    try:
        st = os.stat(path)
    except FileNotFoundError:
        return (str(path), None)
    return (str(path), st.st_size, st.st_mtime_ns, st.st_ctime_ns, st.st_dev, st.st_ino)


def _is_settled(sources, views, fingerprint) -> bool:
    """True when the database holds everything the files had at `fingerprint`:
    each transcript source imported down to byte 0 and (unless the database
    owns it) up to the file's size, and the prompt-view sidecar read to its
    size. A missing file has nothing to import."""
    from . import transcript_records as records
    stats = fingerprint[-2]
    with records.database() as conn:
        for (source, filename), st in zip(sources, stats):
            if not filename or st is None or st[1] is None:
                continue
            meta = conn.execute("SELECT lower_byte, upper_byte FROM transcript_sources WHERE source=?",
                                (source,)).fetchone()
            if meta is None or meta[0] != 0:
                return False
            owned = conn.execute("SELECT 1 FROM transcript_owned WHERE source=?", (source,)).fetchone()
            if not owned and meta[1] != st[1]:
                return False
        vsource, vst = views, fingerprint[-1]
        if vst is not None and vst[1] is not None:
            row = conn.execute("SELECT upper FROM transcript_view_sources WHERE source=?",
                               (vsource,)).fetchone()
            if row is None or row[0] != vst[1]:
                return False
    return True



class _SourceView:
    """Private read-only resolver input; never stored or normalized as Org."""
    _shared_snapshot = True

    def __init__(self, doc):
        self.d = doc
        self.nodes = doc['nodes']

    def node(self, nid):
        return self.nodes[nid]


def _source_view(slug, nid):
    from . import store
    doc = store.read_transcript_source(slug, nid)
    if doc is not None:
        node = doc['nodes'][nid]
        if not node.get('session_id'):
            return _SourceView(doc)  # capture returns before path/identity resolution
        # Source naming and legacy identity initialization remain owned by
        # their existing routines. Never let a partial view enter a mint.
        if (doc.get('reply_incarnation') and node.get('transcript_incarnation')
                and node.get('reply_incarnation') and node.get('model')
                and node.get('session_id') and 'generation' in node):
            return _SourceView(doc)
    return store.cached_org(slug)


def capture(slug, nid, *, beginning=False, backfill=False):
    """Capture one node's transcript sources. Returns False only for a
    backfill slice skipped because the node is settled (see module doc)."""
    from . import store, supervisor as sup, transcript_records as records
    from .chat_window import source_key
    from .desktop_import import imported_history_path
    # A database outage can leave durable output in the recovery spool without
    # changing any provider file. Settled-file skips must not starve its replay.
    records._drain_spool()
    # Resolve only source identity/path inputs. Legacy documents still use
    # their existing normalized Org and locked identity initialization.
    org = _source_view(slug, nid)
    node = org.node(nid)
    if not node.get('session_id'):
        return False
    path = sup.transcript_path_for_node(org, nid)
    imported = imported_history_path(org, nid) if backfill else None
    vpath = sup._prompt_view_path(slug, node['session_id'])
    fingerprint = None
    if backfill:
        # Check before source_key/views_source: resolving legacy identities can
        # itself open databases. These are their identity inputs, not a second
        # implementation of the source naming/migration rules.
        fingerprint = (str(store.DATA_ROOT), org.d.get('reply_incarnation'),
                       node.get('transcript_incarnation'), node.get('reply_incarnation'),
                       node['session_id'], node.get('model'),
                       (_stat(path), _stat(imported)), _stat(vpath))
        with _lock:
            previous = _settled.get((slug, nid))
            if previous and previous[0] == fingerprint and not any(s in _fresh for s in previous[1]):
                return False
    else:
        with _lock:
            _settled.pop((slug, nid), None)
    key = source_key(org, nid)
    if beginning and not path:
        with _lock:
            _fresh.add(key)
    sources = [(key, path)]
    if backfill:
        sources.append((source_key(org, nid, True), imported))
    vsource = records.views_source(slug, node['session_id'], records.incarnation(org, nid))
    for source, filename in sources:
        if not filename:
            continue
        with _lock:
            fresh = source in _fresh
        count = 1
        if fresh:
            # Registered before the first provider record: capture the entire
            # new file on discovery, then only suffixes on later passes.
            count = 1 << 50
        elif backfill:
            with records.database() as conn:
                meta = conn.execute('SELECT lower_byte FROM transcript_sources WHERE source=?',
                                    (source,)).fetchone()
                # A fully imported source needs signature/suffix checking,
                # not COUNT(*) over all its lifetime records.
                needs_history = not meta or bool(meta[0])
            if not needs_history:
                # A replacement may create a new epoch. Preserve the old
                # backfill window for changed files rather than capturing
                # only their last record after an in-memory cache eviction.
                needs_history = records._ingest_needed(
                    source, str(filename), 1, {'bytes_read': 0}, None)
            if needs_history:
                with records.database() as conn:
                    have = conn.execute('SELECT COUNT(*) FROM transcript_records WHERE source=?',
                                        (source,)).fetchone()[0]
                count = have + 64
            else:
                count = 1
        records.ingest(source, str(filename), count, {'bytes_read': 0})
        with _lock:
            _fresh.discard(source)
    records.ingest_prompt_views(vsource, vpath)
    if backfill:
        # the fingerprint was taken BEFORE this pass: a file that grew during
        # it no longer matches, so the next slice runs again
        settled = _is_settled(sources, vsource, fingerprint)
        with _lock:
            if settled:
                _remember_settled((slug, nid), (fingerprint, tuple(s for s, _ in sources)))
            else:
                _settled.pop((slug, nid), None)
    return True


def capture_safely(slug, nid, **kwargs):
    try:
        return capture(slug, nid, **kwargs)
    except Exception:
        # Capture must be retried by the worker. Never claim migration complete
        # or discard source files when a database/file read failed.
        _log.exception('Transcript capture failed for %s/%s; source retained for retry', slug, nid)
        with _lock:
            _settled.pop((slug, nid), None)
        return True


class _SweepState:
    """Active queue plus a bounded, independent historical discovery cursor."""
    def __init__(self):
        self.root = None
        self.orgs = collections.deque()
        self.cursor = ''
        self.round = 0
        self.active = collections.OrderedDict()
        self.hot = collections.deque()

    def discover(self):
        from . import store
        root = str(store.DATA_ROOT)
        if self.root != root:
            self.__init__()
            self.root = root
        if not self.orgs:
            self.round += 1
            self.orgs.extend(store.org_slugs())
        archived = []
        budget = 8
        while self.orgs and budget:
            slug = self.orgs[0]
            page = store.read_transcript_nodes_page(slug, self.cursor, budget)
            if page is None:
                # JSON/old blob compatibility keeps existing normalization.
                org = store.cached_org(slug)
                rows = [(nid, org.node(nid).get('state'))
                        for nid in sorted(org.nodes) if nid > self.cursor][:budget + 1]
                page = {'rows': rows[:budget], 'more': len(rows) > budget}
            rows = page['rows']
            for nid, state in rows:
                key = (slug, nid)
                if state == 'archived':
                    self.active.pop(key, None)
                    archived.append(key)
                else:
                    self.active[key] = self.round
            budget -= len(rows)
            if rows:
                self.cursor = rows[-1][0]
            if not page['more']:
                self.orgs.popleft()
                self.cursor = ''
            elif not rows:
                raise RuntimeError('transcript discovery cursor made no progress')
        if not self.orgs:
            for key in [key for key, seen in self.active.items() if seen != self.round]:
                del self.active[key]
        return archived


def _sweep(state):
    """Busy capture, active backfill, then bounded historical reconciliation.

    At most eight historical node rows are discovered per tick. Settled
    archived sources never enter the active queue, but retain stat/signature
    checks and re-enter ingestion immediately when their slice finds change.
    """
    from . import supervisor as sup, transcript_records as records
    records._drain_spool()
    with sup._state_lock:
        busy = [key for key, value in sup._state.items() if value.get('busy')]
    for slug, nid in busy:
        capture_safely(slug, nid)
    archived = state.discover()
    if not state.hot:
        state.hot.extend(state.active)
    for _ in range(min(8, len(state.hot))):
        key = state.hot.popleft()
        if key in state.active:
            capture_safely(*key, backfill=True)
    for key in archived:
        capture_safely(*key, backfill=True)


def start():
    global _started
    with _lock:
        if _started:
            return
        _started = True

    def run():
        from . import transcript_records as records
        queue = _SweepState()
        with records.reuse_database():
            while True:
                try:
                    _sweep(queue)
                except Exception:
                    _log.exception('Transcript capture sweep failed; retrying')
                time.sleep(1)

    threading.Thread(target=run, name='transcript-capture', daemon=True).start()
