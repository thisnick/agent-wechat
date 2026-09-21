"""Selection must return, read back identity, and remain bounded on failure."""
import importlib.util
from pathlib import Path
import subprocess
import sys
import time
import unittest
from unittest.mock import Mock, patch

spec = importlib.util.spec_from_file_location(
    'chat_select', Path(__file__).resolve().parents[1] / 'docker/tools/chat-select.py')
select = importlib.util.module_from_spec(spec)
spec.loader.exec_module(select)
PROFILE = select.BUILD_PROFILES['ce28c347']


class SelectionTests(unittest.TestCase):
    def invoke(self, lines, click=None):
        with patch.object(select, 'run_frida_bg', return_value=Mock()) as start, \
             patch.object(select, 'read_lines_until', return_value=lines), \
             patch.object(select, 'kill_frida') as cleanup, \
             patch.object(select.subprocess, 'run', return_value=click or Mock(returncode=0)), \
             patch.object(select, 'log'):
            result = select.select_chat('42', PROFILE, 'wxid_fixture', (100, 100))
            cleanup.assert_called_once_with(start.return_value)
            return result

    def test_redirect_alone_is_not_success(self):
        self.assertIsNone(self.invoke(['REDIRECT']))

    def test_silent_hook_is_not_success(self):
        self.assertIsNone(self.invoke([]))

    def test_verified_index_zero_is_success(self):
        self.assertEqual(self.invoke(['REDIRECT', 'SELECTED_INDEX 0', 'SELECTION_VERIFIED']), 0)

    def test_click_failure_is_not_success(self):
        self.assertIsNone(self.invoke(['SELECTION_VERIFIED'], Mock(returncode=1)))

    def test_click_timeout_still_cleans_up(self):
        with patch.object(select, 'run_frida_bg', return_value=Mock()) as start, \
             patch.object(select, 'kill_frida') as cleanup, \
             patch.object(select.subprocess, 'run', side_effect=subprocess.TimeoutExpired('click', 5)):
            with self.assertRaises(subprocess.TimeoutExpired):
                select.select_chat('42', PROFILE, 'wxid_fixture', (100, 100))
            cleanup.assert_called_once_with(start.return_value)

    def test_script_resolves_live_identity_and_waits_for_native_return(self):
        source = select.selection_script(PROFILE, 'wxid_fixture')
        self.assertIn('manager = args[0]', source)
        self.assertIn('var hmInner = ctrl.add(0xf8).readPointer()', source)
        self.assertIn('selectedIndex = i;', source)
        self.assertIn('if (!returned || controller === null) return', source)
        self.assertIn('current.add(0x98)', source)
        self.assertIn('=== target', source)
        self.assertNotIn('hook.detach()', source)
        self.assertNotIn('this.context.', source)
        self.assertLess(source.index('Interceptor.flush()'), source.index("console.log('READY')"))

    def test_native_map_indices_are_not_renumbered(self):
        lines = ['VECTOR 0x10000 count=4', 'SESSION 0 wxid_first',
                 'SESSION 1 gh_abc123', 'SESSION 3 wxid_last', 'CURRENT_SEL wxid_first']
        with patch.object(select, 'run_frida_script', return_value=lines), patch.object(select, 'log'):
            sessions, _, _, _ = select.enumerate_sessions('42', PROFILE)
        self.assertEqual(sessions, {'wxid_first': 0, 'wxid_last': 3})

    def test_legacy_arm_retains_filtered_indexing(self):
        source = select.selection_script(select.BUILD_PROFILES['3eda8254'], 'wxid_fixture')
        self.assertIn('selectedIndex = filteredIndex;', source)
        self.assertIn('var vectorBegin = ctrl.add(0x0)', source)

    def test_unique_scripts_are_removed(self):
        with select.script_path() as first, select.script_path() as second:
            self.assertNotEqual(first, second)
            self.assertEqual(Path(first).stat().st_mode & 0o777, 0o600)
        self.assertFalse(Path(first).exists())
        self.assertFalse(Path(second).exists())

    def test_silent_process_deadline(self):
        proc = subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(30)'],
                                stdout=subprocess.PIPE, stdin=subprocess.PIPE, text=True)
        try:
            start = time.monotonic()
            self.assertEqual(select.read_lines_until(proc, .1, 'READY'), [])
            self.assertLess(time.monotonic() - start, 1)
        finally:
            select.kill_frida(proc)
            proc.stdout.close()

    def test_missing_ready_cleans_up(self):
        with patch.object(select.subprocess, 'Popen', return_value=Mock()) as start, \
             patch.object(select, 'read_lines_until', return_value=['script error']), \
             patch.object(select, 'kill_frida') as cleanup:
            with self.assertRaises(RuntimeError):
                select.run_frida_bg('42', 'unused.js')
            cleanup.assert_called_once_with(start.return_value)

    def test_already_selected_but_unresponsive_ui_is_failure(self):
        with patch.object(sys, 'argv', ['chat-select', 'wxid_fixture']), \
             patch.object(select, 'get_pid', return_value='42'), \
             patch.object(select, 'get_profile', return_value=(PROFILE, None)), \
             patch.object(select, 'enumerate_sessions', return_value=(
                 {'wxid_fixture': 0}, '0x10000', 1, 'wxid_fixture')), \
             patch.object(select, 'find_chat_item_from_a11y', return_value=None), \
             patch.object(select, 'result_json', side_effect=SystemExit) as result, \
             patch.object(select, 'log'):
            with self.assertRaises(SystemExit):
                select.main()
            self.assertFalse(result.call_args.args[0])

    def test_forced_repeat_is_idempotent_only_with_visible_selection(self):
        for visible in (True, False):
            with self.subTest(visible=visible), \
                 patch.object(sys, 'argv', ['chat-select', '--force', 'wxid_fixture']), \
                 patch.object(select, 'get_pid', return_value='42'), \
                 patch.object(select, 'get_profile', return_value=(PROFILE, None)), \
                 patch.object(select, 'enumerate_sessions', return_value=(
                     {'wxid_fixture': 0}, '0x10000', 1, 'wxid_fixture')), \
                 patch.object(select, 'find_chat_item_from_a11y',
                              side_effect=lambda require_open_chat=False:
                                  (100, 100) if not require_open_chat or visible else None), \
                 patch.object(select, 'select_chat', return_value=None) as click, \
                 patch.object(select, 'result_json', side_effect=SystemExit) as result, \
                 patch.object(select, 'log'):
                with self.assertRaises(SystemExit):
                    select.main()
                if visible:
                    click.assert_not_called()
                    self.assertTrue(result.call_args.kwargs['skipped'])
                    self.assertTrue(result.call_args.args[0])
                else:
                    click.assert_called_once()
                    self.assertFalse(result.call_args.args[0])

    def test_selected_item_can_be_after_first_row(self):
        items = []
        tree = {'role': 'list', 'name': 'Chats', 'children': [
            {'role': 'list-item', 'bounds': {'x': 0}},
            {'role': 'list-item', 'bounds': {'x': 1}, 'states': ['SELECTED']}]}
        select._find_chat_list_items(tree, items, False)
        self.assertEqual(len(items), 2)
        self.assertIn('SELECTED', items[1]['states'])

    def test_offscreen_chat_has_open_pane_without_selected_row(self):
        tree = {'children': [
            {'role': 'list', 'name': 'Messages', 'bounds': {'width': 500, 'height': 400}},
            {'role': 'push-button', 'name': 'Send', 'bounds': {'width': 50, 'height': 25}}]}
        self.assertTrue(select.has_open_chat_pane(tree))
        tree['children'].pop()
        self.assertFalse(select.has_open_chat_pane(tree))


if __name__ == '__main__':
    unittest.main()
