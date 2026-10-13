"""回归：相同版本/依赖别名绝不能绕过 source 与传递图隔离检查。"""
import copy
import unittest
from check_isolation import ROOT, check_oracle, check_kernel, check_production

class IsolationTests(unittest.TestCase):
    def test_production_pdu_source_path_version_and_session_edge(self):
        names = [('ironrdp-graphics', '0.9.0'), ('ironrdp-session', '0.11.0'), ('ironrdp-pdu', '0.9.0')]
        graph = {'packages': [{'name': name, 'version': version, 'id': name, 'source': None, 'manifest_path': str(ROOT / 'third_party' / (name + '-' + version) / 'Cargo.toml')} for name, version in names] + [{'name': 'frd-rdp-legacy-kernels', 'id': 'kernel'}], 'resolve': {'nodes': [{'id': 'ironrdp-session', 'dependencies': ['ironrdp-graphics', 'ironrdp-pdu']}, {'id': 'ironrdp-graphics', 'dependencies': ['kernel']}]}}
        check_production(graph)
        for key, value in [('source', 'registry+https://github.com/rust-lang/crates.io-index'), ('manifest_path', '/tmp/ironrdp-pdu-0.9.0/Cargo.toml'), ('version', '0.9.1')]:
            changed = copy.deepcopy(graph)
            changed['packages'][2][key] = value
            with self.subTest(key=key), self.assertRaises(AssertionError):
                check_production(changed)
        changed = copy.deepcopy(graph)
        changed['resolve']['nodes'][0]['dependencies'].remove('ironrdp-pdu')
        with self.assertRaises(AssertionError):
            check_production(changed)
        changed = copy.deepcopy(graph)
        duplicate = dict(changed['packages'][2], id='registry-pdu', source='registry+https://github.com/rust-lang/crates.io-index')
        changed['packages'].insert(0, duplicate)
        with self.assertRaises(AssertionError):
            check_production(changed)

    def oracle(self):
        versions = [('ironrdp-graphics','0.9.0'),('ironrdp-session','0.11.0'),('ironrdp-pdu','0.9.0'),('yuv','0.8.17')]
        return {'packages': [{'name': n, 'version': v, 'source':'registry+https://github.com/rust-lang/crates.io-index'} for n,v in versions]}

    def test_same_version_local_patch_cannot_be_reference(self):
        graph = self.oracle()
        check_oracle(graph)
        graph['packages'][0]['source'] = None
        with self.assertRaises(AssertionError):
            check_oracle(graph)

    def test_transitive_dev_edge_is_not_exempt(self):
        graph = {'packages':[{'id':'k','name':'frd-rdp-legacy-kernels'},{'id':'helper','name':'helper'},{'id':'original-alias','name':'ironrdp-graphics'}], 'resolve':{'nodes':[{'id':'k','dependencies':['helper']},{'id':'helper','dependencies':['original-alias']},{'id':'original-alias','dependencies':[]}]}}
        with self.assertRaises(AssertionError):
            check_kernel(graph)
        graph['resolve']['nodes'][1]['dependencies'] = []
        check_kernel(graph)

    def test_version_drift_is_rejected(self):
        graph = self.oracle()
        graph['packages'][0]['version'] = '0.9.1'
        with self.assertRaises(AssertionError):
            check_oracle(graph)

if __name__ == '__main__':
    unittest.main()
