"""Owned PG probe: which view does capture read per visit, and why an org_tx.

Throwaway C:/Temp/cold-ingest-* root and private PostgreSQL only.
"""
import json
import os
from pathlib import Path
import subprocess
import sys
import uuid

REPO = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO / 'tools'))
sys.path.insert(0, str(REPO / 'tools/scale'))
from assert_repo_import import assert_repo_import
from cold_ingest_profile import TOOL, PGBIN, write_transcript


def main(root):
    root = Path(root).resolve()
    if root.exists() or not root.name.startswith('cold-ingest-'):
        raise ValueError('new owned C:/Temp/cold-ingest-* root required')
    root.mkdir()
    pgroot = root / 'pg'

    def pg(action):
        cp = subprocess.run([TOOL, action, '--root', str(pgroot), '--pg-bin', PGBIN],
                            capture_output=True, text=True, timeout=60)
        if cp.returncode:
            raise RuntimeError(action + ': ' + cp.stdout + cp.stderr)
        return json.loads(cp.stdout)
    out = {}
    pg('init-root'); pg('init'); pg('start')
    try:
        from seed import _create_db
        os.environ['ORGTREE_PG_URL'] = _create_db(pg('urls')['urls']['P03_PG_ADMIN_URL'], 'orgtree_mint_probe')
        os.environ['ORGTREE_STORE'] = 'postgres'
        os.environ['ORGTREE_DATA'] = str(root / 'data')
        os.environ['ORGTREE_V2_TOKEN'] = 'mint-probe-only'
        (root / 'data').mkdir()
        prov = assert_repo_import(REPO)
        from engine.launch import load_app
        load_app()
        from orgtree import store, ledger, pgstore, orgtx, supervisor as sup, transcript_ingest as ingest
        from orgtree import transcript_records as records
        pgstore.migrate(os.environ['ORGTREE_PG_URL'])
        org = store.create_org('mint-' + uuid.uuid4().hex[:8])
        slug = org.d['slug']
        for nid in ('worker-000', 'worker-001'):
            org.hire(ledger.USER, None, 'haiku', 0, nid)
        store.save_org(org)
        org = store.load_org(slug)
        paths = {}
        for nid in ('worker-000', 'worker-001'):
            p = root / f'{nid}.jsonl'
            write_transcript(p, 180, 1460, nid)
            paths[org.node(nid)['session_id']] = str(p)
        sup.transcript_path = lambda sid, *a, **k: paths.get(str(sid))
        events = []
        real_view, real_tx, real_inc = ingest._source_view, orgtx.org_tx, records.incarnation

        def view(slug_, nid):
            v = real_view(slug_, nid)
            doc = store.read_transcript_source(slug_, nid)
            dnode = (doc or {}).get('nodes', {}).get(nid, {})
            events.append(dict(event='view', nid=nid, kind=type(v).__name__,
                               has_inc=bool(v.node(nid).get('transcript_incarnation')),
                               doc_reply=bool((doc or {}).get('reply_incarnation')),
                               node_fields={k: (k in dnode) for k in ('transcript_incarnation', 'reply_incarnation',
                                                                     'model', 'session_id', 'generation')}))
            return v

        def tx(*a, **k):
            events.append(dict(event='org_tx', args=[str(x) for x in a], kw={k2: str(v2) for k2, v2 in k.items()}))
            return real_tx(*a, **k)

        def inc(o, nid):
            events.append(dict(event='incarnation', nid=nid, kind=type(o).__name__,
                               has=bool(o.node(nid).get('transcript_incarnation'))))
            return real_inc(o, nid)
        ingest._source_view, orgtx.org_tx, records.incarnation = view, tx, inc
        for visit in range(3):
            events.append(dict(event='visit', n=visit))
            ingest.capture(slug, 'worker-000', backfill=True)
        out['events'] = events
        out['org_tx'] = sum(e['event'] == 'org_tx' for e in events)
        records.close_all()
        pgstore.close_idle()
        prov.write_result(root / 'result.json', out)
    finally:
        out['pg_stop'] = pg('stop')
        print(json.dumps(out, indent=1, default=str), flush=True)


if __name__ == '__main__':
    main(sys.argv[1])
