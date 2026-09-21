"""Keep legacy and ARM ordered-map session layouts separate."""
import importlib.util
from pathlib import Path
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location(
    'chat_select', Path(__file__).resolve().parents[1] / 'docker/tools/chat-select.py')
select = importlib.util.module_from_spec(spec)
spec.loader.exec_module(select)


class VectorLayoutTests(unittest.TestCase):
    def generated_script(self, build):
        lines = ['VECTOR 0x123400 count=2', 'SESSION 0 filehelper',
                 'SESSION 1 wxid_fixture', 'CURRENT_SEL filehelper']
        with patch.object(select, 'write_js') as write, \
             patch.object(select, 'run_frida_script', return_value=lines), \
             patch.object(select, 'log'):
            sessions, _, count, current = select.enumerate_sessions(
                '4242', select.BUILD_PROFILES[build])
        self.assertEqual(sessions, {'filehelper': 0, 'wxid_fixture': 1})
        self.assertEqual(count, 2)
        self.assertEqual(current, 'filehelper')
        return write.call_args.args[1]

    def test_arm_new_uses_ordered_controller_map(self):
        script = self.generated_script('e9f1cd04')
        self.assertIn('var hmInner = ctrl;', script)
        self.assertIn('hmNode.add(0x28).readPointer()', script)
        self.assertNotIn('var vectorBegin = ctrl.add(0x0)', script)

    def test_arm_legacy_keeps_direct_vector(self):
        self.assertIn('var vectorBegin = ctrl.add(0x0)', self.generated_script('3eda8254'))

    def test_x86_keeps_indirect_map(self):
        self.assertIn('var hmInner = ctrl.add(0xf8).readPointer();',
                      self.generated_script('ce28c347'))


if __name__ == '__main__':
    unittest.main()
