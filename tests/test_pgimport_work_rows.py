"""SQLite and legacy JSON -> per-item PG rows, using the real importer."""
import json
from unittest.mock import patch
import unittest
import test_pgimport as f
from orgtree import workrows
from test_work_item_rows import items


class WorkImport(f.Base):
    def populate(self):
        a=f.sample_doc(); a['work_items']=items()
        f.write_db(self.orgs()/'acme.db',a)
        b=f.sample_doc('Beta'); b['work_items']=items()
        (self.orgs()/'beta.json').write_text(json.dumps(b),encoding='utf-8')

    def test_both_sources_destination_manifests_and_resume(self):
        self.populate(); before=f.tree_digest(self.orgs())
        report=f.pgimport.dry_run(self.root)
        self.assertTrue(report['importable'],report['refused'])
        for row in report['orgs'].values():
            self.assertEqual(row['work_items'],workrows.checksum(items()))
            self.assertNotEqual(row['source_manifest_sha256'],row['manifest_sha256'])
            self.assertEqual(row['manifest']['tables']['doc']['count'],row['source_manifest']['tables']['doc']['count']+2)
        result=f.pgimport.import_root(self.root,self.sink())
        for slug in result['orgs']:
            doc=dict(self.sink().read_org(slug)['doc'])
            rows={k:v for k,v in doc.items() if k=='work_items' or k.startswith(workrows.PREFIX)}
            self.assertEqual(workrows.assemble(rows),items())
            self.assertEqual(f.pgimport.manifest(self.sink().read_org(slug)),report['orgs'][slug]['manifest'])
        again=f.pgimport.import_root(self.root,self.sink())
        self.assertTrue(all(v['action']=='already_imported' for v in again['orgs'].values()))
        self.assertEqual(f.tree_digest(self.orgs()),before)

    def test_duplicate_slugs_refuse_before_sink_write(self):
        self.populate()
        path=self.orgs()/'beta.json'; doc=json.loads(path.read_text());doc['work_items'].append(doc['work_items'][0]);path.write_text(json.dumps(doc))
        sink=self.sink()
        with self.assertRaisesRegex(f.ImportRefused,'duplicate'):
            f.pgimport.import_root(self.root,sink)
        self.assertIsNone(sink.recorded('acme'))

    def test_missing_item_readback_refuses_marker(self):
        self.populate(); sink=self.sink(); original=sink.read_org; fired=[]
        def corrupted(slug):
            value=original(slug)
            value['doc']=[row for row in value['doc'] if row[0]!=workrows.PREFIX+'one']
            fired.append(slug); return value
        sink.read_org=corrupted
        with self.assertRaisesRegex(f.ImportRefused,'read-back'):
            f.pgimport.import_root(self.root,sink)
        self.assertEqual(fired,['acme']); self.assertEqual(sink.finished,[])


if __name__=='__main__': unittest.main()
