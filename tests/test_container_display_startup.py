"""Regression guards for the single-session container display configuration."""
import pathlib
import shlex
import subprocess
import unittest


ENTRYPOINT = pathlib.Path(__file__).resolve().parents[1] / "docker" / "entrypoint.sh"


class ContainerDisplayStartupTests(unittest.TestCase):
    def test_shell_syntax(self):
        subprocess.run(["bash", "-n", str(ENTRYPOINT)], check=True)

    def command_for(self, program):
        lines = ENTRYPOINT.read_text().splitlines()
        matches = [shlex.split(line) for line in lines
                   if line.strip().startswith("su ") and f"exec {program} " in line]
        self.assertEqual(len(matches), 1)
        command = matches[0]
        self.assertEqual(command[0], "su")
        self.assertEqual(command[-2:], ["wechat", "&"])
        return shlex.split(command[command.index("-c") + 1])

    def test_x_server_runs_as_gui_user_without_tcp_listener(self):
        command = self.command_for("Xvfb")
        self.assertIn("-nolisten", command)
        self.assertEqual(command[command.index("-nolisten") + 1], "tcp")

    def test_vnc_runs_as_gui_user_and_remains_read_only(self):
        command = self.command_for("x11vnc")
        self.assertIn("-viewonly", command)
        self.assertEqual(command[command.index("-listen") + 1], "127.0.0.1")

    def test_proxy_credentials_use_private_nonsticky_directory(self):
        source = ENTRYPOINT.read_text()
        self.assertNotIn("/tmp/redsocks.conf", source)
        self.assertIn("install -d -m 700 -o redsocks -g redsocks /run/agent-wechat-proxy", source)
        self.assertIn("umask 077; cat > /run/agent-wechat-proxy/redsocks.conf", source)


if __name__ == "__main__":
    unittest.main()
