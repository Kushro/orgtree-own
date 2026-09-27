"""PostgreSQL Sent projection ordering, source preservation and bounded plans."""
import json
import unittest

import test_mail_archive_bounds_pg as fixture
from orgtree import store

tearDownModule = fixture.tearDownModule


@unittest.skipUnless(fixture.ADMIN, 'ORGTREE_TEST_PG_ADMIN_URL not set: NOT RUN')
class MailSentIndex(unittest.TestCase):
    setUpClass = classmethod(fixture.MailArchiveBounds.setUpClass.__func__)
    setUp = fixture.MailArchiveBounds.setUp
    query = fixture.MailArchiveBounds.query

    def insert(self, owner, sender, at, seq=None):
        raw = json.dumps({'from': sender, 'at': at, 'body': f'{owner}/{at}/{seq}'})
        with store._POOL.acquire(self.slug) as conn:
            conn.execute('BEGIN')
            if seq is None:
                conn.execute("INSERT INTO log_d(sect,owner,val) VALUES('mail_log',?,?)", (owner, raw))
            else:
                conn.execute("INSERT INTO log_d(seq,sect,owner,val) VALUES(?,'mail_log',?,?)", (seq, owner, raw))
            conn.execute('COMMIT')

    def assert_projection(self):
        want = self.query("SELECT seq,owner,json_extract(val,'$.from'),coalesce(json_extract(val,'$.at'),''),"
                          "min(seq) OVER(PARTITION BY owner) FROM log_d WHERE sect='mail_log' ORDER BY seq")
        self.assertEqual(self.query('SELECT seq,owner,sender,sent_at,owner_pos FROM mail_sent ORDER BY seq'), want)

    def test_equal_time_owner_order_and_user_merge_match_existing_query(self):
        for owner in ('earlier', 'later'):
            for _ in range(7): self.insert(owner, 'sender', 'same')
        for _ in range(3): self.insert('earlier', 'sender', 'same')
        self.insert('later', 'other', 'zzzz')
        with store._POOL.acquire(self.slug) as conn:
            conn.execute('BEGIN')
            conn.execute("INSERT INTO log_l(sect,val) VALUES('user_mail_log',?)",
                         (json.dumps({'from': 'sender', 'at': 'same', 'body': 'user'}),))
            conn.execute('COMMIT')
        want = [dict(json.loads(raw), to=owner) for owner,raw in self.query(
            "SELECT l.owner,l.val FROM log_d l JOIN (SELECT owner,min(seq) pos FROM log_d "
            "WHERE sect='mail_log' GROUP BY owner) o ON o.owner=l.owner "
            "WHERE l.sect='mail_log' AND json_extract(l.val,'$.from')='sender' "
            "ORDER BY coalesce(json_extract(l.val,'$.at'),''),o.pos,l.seq")]
        want.append({'from': 'sender', 'at': 'same', 'body': 'user', 'to': '@user'})
        want.sort(key=lambda row: row.get('at') or '')
        self.assertEqual(store.read_mail_tails(self.slug, 'sender', keep=5, slack=0)[3], want[-5:])
        self.assert_projection()

    def test_first_row_delete_move_and_earlier_restore_repair_only_owner_order(self):
        self.insert('other', 'sender', 'same')
        self.insert('other', 'sender', 'same')
        self.insert('other', 'sender', 'same', seq=-10)
        self.assert_projection()
        with store._POOL.acquire(self.slug) as conn:
            conn.execute('BEGIN')
            conn.execute("UPDATE log_d SET owner='worker' WHERE seq=-10")
            conn.execute('COMMIT')
        self.assert_projection()
        with store._POOL.acquire(self.slug) as conn:
            conn.execute('BEGIN')
            conn.execute('DELETE FROM log_d WHERE seq=-10')
            conn.execute('COMMIT')
        self.assert_projection()

    def test_rebuild_and_rollback_preserve_source_and_projection(self):
        before = self.query('SELECT seq,owner,val FROM log_d ORDER BY seq')
        with store._POOL.acquire(self.slug) as conn:
            conn.execute('BEGIN')
            for _ in range(2): conn.execute('SELECT public.orgtree_install_mail_sent(?)', (conn.org_id,))
            conn.execute('COMMIT')
            conn.execute('BEGIN')
            conn.execute('TRUNCATE log_d')
            self.assertEqual(conn.execute('SELECT count(*) FROM mail_sent').fetchone()[0], 0)
            conn.execute('ROLLBACK')
        self.assertEqual(self.query('SELECT seq,owner,val FROM log_d ORDER BY seq'), before)
        self.assert_projection()

    def test_sender_index_bounds_equal_timestamp_history(self):
        # A large same-time group is the adversarial case for a partial index
        # followed by owner-tie sorting. The complete projection key avoids it.
        with store._POOL.acquire(self.slug) as conn:
            conn.execute('BEGIN')
            conn.execute("INSERT INTO log_d(sect,owner,val) SELECT 'mail_log','history',? "
                         "FROM generate_series(1,1000)", (json.dumps({'from': 'sender', 'at': 'same'}),))
            conn.execute('ANALYZE mail_sent')
            plan = conn.execute("EXPLAIN (ANALYZE,FORMAT JSON) SELECT seq FROM mail_sent "
                "WHERE sender=? ORDER BY sent_at DESC,owner_pos DESC,seq DESC LIMIT 5", ('sender',)).fetchone()[0]
            conn.execute('COMMIT')
        if isinstance(plan, str): plan = json.loads(plan)
        root = plan[0]['Plan']
        nodes = []
        def walk(node):
            nodes.append(node)
            for child in node.get('Plans', []): walk(child)
        walk(root)
        self.assertFalse(any(n['Node Type'] in ('Sort','Seq Scan') for n in nodes), plan)
        index = next(n for n in nodes if n.get('Index Name') == 'ix_mail_sent_tail')
        self.assertEqual(index['Actual Rows'], 5)
        self.assert_projection()


if __name__ == '__main__': unittest.main()
