"""Per-org listing summaries in a single committed snapshot.

Only active funding rows are materialized. The public producer has an explicit
field allowlist and never reads kiosk tokens. Admin normalization uses shared
pure helpers; incompatible legacy documents keep the existing complete reader.
"""
import copy
import json
import sqlite3

from . import foreground_store, org_listing, store
from .foreground_context import CompatibilityRequired, _compatible
from .ledger import Org, _q
from .readonly_projection import ProjectionDoc

_ADMIN_KEYS = tuple('''slug name created net_identity kiosk tiers models deleted_cost_usd
 workspace sandbox disk spend_frozen storage_blocked _actors_typed whole_grants_v1
 _migrations fable_lock nodes'''.split())


class _AdminSummary:
    _read_only_projection = True
    node = Org.node
    seat_cost = Org.seat_cost
    children = Org.children
    children_index = Org.children_index

    def __init__(self, settings, nodes, cost):
        # Listing does not use prompt/cache predecessor identity. Only the
        # settings migrations affect this projection's normalization.
        _compatible(settings, {})
        self.d = ProjectionDoc(copy.deepcopy(settings))
        self.d['nodes'] = copy.deepcopy(nodes)
        Org._normalize_display_basics(self)
        Org._normalize_display_models(self)
        self._cost = cost

    @property
    def nodes(self):
        return self.d['nodes']

    def cost_total(self):
        # Deliberately identical to the tree header's decimal-total display.
        return round(float(self._cost) + float(self.d.get('deleted_cost_usd') or 0.0), 4)


def top_level_holds(org):
    if isinstance(org, _AdminSummary):
        return _q(sum(org.seat_cost(nid) + org.nodes[nid]['grant']
                      for nid in org.children(None)))
    return org.audit()['top_level_holds']


def _read(slug, public):
    def snapshot(raw, stamp):
        # A pre-row-store node blob has no validated native count projection.
        if raw.execute("SELECT 1 FROM doc WHERE key='nodes'").fetchone():
            raise CompatibilityRequired('legacy node blob')
        if public:
            fields = dict(raw.execute(
                "SELECT CASE WHEN key='net_identity' AND jsonb_typeof(val::jsonb) NOT IN ('object','null') "
                "THEN '_unsupported_net_identity' ELSE key END,CASE key "
                "WHEN 'kiosk' THEN CASE WHEN val::jsonb='null'::jsonb THEN 'false' ELSE 'true' END "
                "WHEN 'net_identity' THEN coalesce((val::jsonb->'slug')::text,'null') "
                "ELSE val END FROM doc "
                "WHERE key IN ('slug','name','kiosk','net_identity','created')").fetchall())
            if '_unsupported_net_identity' in fields:
                raise CompatibilityRequired('legacy network identity shape')
            fields = {key: json.loads(val) for key, val in fields.items()}
            if fields.get('slug', slug) != slug:
                raise CompatibilityRequired('organization identity changed')
            states = raw.execute(
                "SELECT n.val::jsonb->'state' FROM node_index i JOIN nodes n ON n.id=i.id "
                "WHERE i.meta->>'state'<>'archived' ORDER BY i.ord,i.id").fetchall()
            return {'slug': fields.get('slug', slug), 'name': fields.get('name', slug),
                    'nodes': stamp['node_count'], 'live': sum(state == 'live' for state, in states),
                    'kiosk': True, 'net_slug': fields.get('net_identity'),
                    'created': fields.get('created')}
        if raw.execute('SELECT 1 FROM nodes WHERE public.orgtree_summary_cost_exception(val) LIMIT 1').fetchone():
            raise CompatibilityRequired('legacy cost conversion is required')
        settings = {key: json.loads(value) for key, value in raw.execute(
            'SELECT key,val FROM doc WHERE key=ANY(%s)', (list(_ADMIN_KEYS),)).fetchall()}
        if settings.get('slug') != slug:
            raise CompatibilityRequired('organization identity changed')
        nodes = {}
        for nid, ordinal, value in raw.execute(
                "SELECT n.id,n.ord,n.val FROM node_index i JOIN nodes n ON n.id=i.id "
                "WHERE i.meta->>'state'<>'archived' ORDER BY i.ord,i.id").fetchall():
            node = json.loads(value)
            node.setdefault('ui_order', float(ordinal))
            nodes[nid] = node
        row = store._summary_row(slug, dict(settings, nodes=nodes))
        row['nodes'] = stamp['node_count']
        context = _AdminSummary(settings, nodes, stamp['cost'])
        return row, context
    return foreground_store.read_snapshot(slug, snapshot)


def public_rows(slug):
    if org_listing._native():
        try:
            return [_read(slug, True)]
        except foreground_store.OrgNotFound:
            return []
        except (CompatibilityRequired, sqlite3.Error):
            pass
    return [{**row, 'kiosk': True} for row in store.list_orgs() if row['slug'] == slug]


def admin_rows():
    if not org_listing._native():
        return store.list_orgs_with_docs()
    rows = []
    for slug in store.org_slugs():
        try:
            rows.append(_read(slug, False))
        except foreground_store.OrgNotFound:
            continue
        except (CompatibilityRequired, sqlite3.Error):
            # Only this legacy organization takes the compatibility path.
            # Read the raw summary before Org normalization, as _scan_orgs does.
            try:
                doc = store._bounded_read(slug, lambda conn: store._load_lazy(conn, slug))
                row = store._summary_row(slug, doc)
            except (store.LedgerError, sqlite3.Error, ValueError, OSError):
                continue
            rows.append((row, Org(doc)))
    return rows
