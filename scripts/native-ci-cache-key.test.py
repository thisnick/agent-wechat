#!/usr/bin/env python3
"""Protect cache reuse across releases and invalidation for dependency changes."""
import json
from pathlib import Path
import runpy
import tempfile
import unittest

cache_digest = runpy.run_path(str(Path(__file__).with_name("native-ci-cache-key.py")))["cache_digest"]


class CacheIdentityTest(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.manifest = self.write("packages/native-client-rust/Cargo.toml", '''
[workspace.package]
version = "1.0.0-beta.8"
[workspace.dependencies]
serde = "1"
''')
        self.lock = self.write("packages/native-client-rust/Cargo.lock", '''
version = 4
[[package]]
name = "wx"
version = "1.0.0-beta.8"
dependencies = ["serde"]
[[package]]
name = "serde"
version = "1.0.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "fixture"
''')
        self.write("packages/native-client-rust/crates/wx/Cargo.toml", '''
[package]
name = "wx"
version.workspace = true
[dependencies]
serde.workspace = true
''')
        self.write("package.json", '{"name":"workspace","version":"1.0.0"}')
        self.package = self.write("packages/cli/package.json", '{"name":"cli","version":"1.0.0","engines":{"node":">=24"}}')
        self.write("pnpm-lock.yaml", 'lockfileVersion: "9.0"\n')

    def write(self, name, content):
        path = self.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(content, encoding="utf-8")
        return path

    def test_workspace_release_reuses_rust_dependencies(self):
        before = cache_digest(self.root, "rust")
        for path in [self.manifest, self.lock]:
            path.write_text(path.read_text().replace("1.0.0-beta.8", "1.0.0-beta.9"))
        self.assertEqual(before, cache_digest(self.root, "rust"))

    def test_locked_dependency_upgrade_invalidates_rust_cache(self):
        before = cache_digest(self.root, "rust")
        self.lock.write_text(self.lock.read_text().replace('version = "1.0.0"', 'version = "1.0.1"'))
        self.assertNotEqual(before, cache_digest(self.root, "rust"))

    def test_source_edits_reuse_compiled_dependencies(self):
        before = cache_digest(self.root, "rust")
        self.write("packages/native-client-rust/crates/wx/src/main.rs", 'fn main() { println!("changed"); }\n')
        self.assertEqual(before, cache_digest(self.root, "rust"))

    def test_rust_features_and_build_flags_invalidate_cache(self):
        before = cache_digest(self.root, "rust")
        self.manifest.write_text(self.manifest.read_text().replace('serde = "1"', 'serde = { version = "1", features = ["derive"] }'))
        changed = cache_digest(self.root, "rust")
        self.assertNotEqual(before, changed)
        self.write(".cargo/config.toml", '[build]\nrustflags = ["-C", "target-cpu=native"]\n')
        self.assertNotEqual(changed, cache_digest(self.root, "rust"))

    def test_package_release_reuses_installed_node_dependencies(self):
        before = cache_digest(self.root, "pnpm")
        package = json.loads(self.package.read_text())
        package["version"] = "1.1.0"
        self.package.write_text(json.dumps(package))
        self.assertEqual(before, cache_digest(self.root, "pnpm"))

    def test_node_dependency_and_install_policy_changes_invalidate_cache(self):
        before = cache_digest(self.root, "pnpm")
        self.write("pnpm-lock.yaml", 'lockfileVersion: "9.0"\npackages: {}\n')
        changed = cache_digest(self.root, "pnpm")
        self.assertNotEqual(before, changed)
        self.write(".npmrc", "ignore-scripts=true\n")
        self.assertNotEqual(changed, cache_digest(self.root, "pnpm"))


if __name__ == "__main__":
    unittest.main()
