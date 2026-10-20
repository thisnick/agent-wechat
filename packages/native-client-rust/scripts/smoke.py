#!/usr/bin/env python3
"""Cross-process native daemon check: synthetic files, no WeChat/production access."""
import hashlib
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import urllib.error
import urllib.request


binary = str(Path(sys.argv[1]).resolve())
http = urllib.request.build_opener(urllib.request.ProxyHandler({}))


def run(*args, success=True):
    result = subprocess.run([binary, "--json", *map(str, args)], capture_output=True, text=True)
    assert (result.returncode == 0) == success, (args, result.stderr)
    return json.loads(result.stdout) if success else result


def tree(root):
    return {str(p.relative_to(root)): hashlib.sha256(p.read_bytes()).hexdigest()
            for p in root.rglob("*") if p.is_file()}


with tempfile.TemporaryDirectory(prefix="wx-native-smoke-") as directory:
    root = Path(directory)
    version = run("version")
    assert version["backend"] == "native" and version["nativeLogin"]
    config_file = root / "server.toml"
    config_file.write_text('schemaVersion=1\ndataDir="data"\nlisten="127.0.0.1:0"\n')
    args = ("--server-config", config_file)
    config = run(*args, "config", "show")
    assert config["dataDir"] == str(root / "data") and not (root / "data").exists()
    run(*args, "store", "list", success=False)
    assert not (root / "data").exists(), "ordinary list must not initialize storage"
    source = root / "source"
    (source / "xwechat_files/account/db_storage/message").mkdir(parents=True)
    (source / "xwechat_files/account/db_storage/message/message_0.db").write_bytes(b"synthetic-main")
    (source / "xwechat_files/account/db_storage/message/message_0.db-wal").write_bytes(b"synthetic-wal")
    (source / "unknown").write_bytes(b"preserve unknown bytes\x00\xff")
    before = tree(source)
    direct = run(*args, "store", "attach", "--account", "direct", "--path", source, "--source-stopped")
    assert direct["profilePath"] == str(source) and direct["schemaCompatibility"] == "unverified"
    copied = run(*args, "store", "import", "--account", "copied", "--source", source, "--source-stopped")
    assert tree(Path(copied["profilePath"])) == before
    export = root / "export"
    run(*args, "store", "export", "--account", "copied", "--destination", export, "--source-stopped")
    assert tree(export) == before and not (export / "agent").exists()
    run(*args, "store", "export", "--account", "copied", "--destination", export, "--source-stopped", success=False)
    assert tree(source) == before

    server_id = None
    token_before = None
    for _ in range(2):
        process = subprocess.Popen([binary, *map(str, args), "serve"], stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        try:
            # select bounds readiness instead of blocking forever on an absent line.
            import select
            ready, _, _ = select.select([process.stderr], [], [], 10)
            assert ready, "server did not become ready"
            ready = json.loads(process.stderr.readline())
            assert ready["state"] == "ready"
            token = (root / "data/api-token").read_text().strip()
            assert os.stat(root / "data/api-token").st_mode & 0o077 == 0
            if token_before is not None:
                assert token == token_before and ready["serverId"] == server_id
            token_before, server_id = token, ready["serverId"]
            base = "http://" + ready["listen"]
            try:
                http.open(base + "/v1/status", timeout=5)
                raise AssertionError("unauthenticated request accepted")
            except urllib.error.HTTPError as error:
                assert error.code == 401
            for endpoint in ("status", "accounts"):
                request = urllib.request.Request(base + "/v1/" + endpoint, headers={"Authorization": "Bearer " + token})
                with http.open(request, timeout=5) as response:
                    value = json.load(response)
                if endpoint == "accounts":
                    assert {a["id"] for a in value["items"]} == {"direct", "copied"}
                else:
                    assert value["version"]["officialDatabaseWrites"]
                    assert value["pid"] == process.pid and value["dataDir"] == str(root / "data")
                    assert value["ownershipGeneration"]
            remote = ("--url", base, "--token-file", root / "data/api-token")
            assert run(*remote, "status")["pid"] == process.pid
            assert run(*remote, "accounts", "list")["items"]
            grant = run(*remote, "tokens", "create", "smoke-read", "--accounts", "copied", "--scopes", "read")
            scoped = root / ("scoped-token-" + str(process.pid))
            scoped.write_text(grant["token"])
            scoped.chmod(0o600)
            restricted = run("--url", base, "--token-file", scoped, "accounts", "list")
            assert [a["id"] for a in restricted["items"]] == ["copied"]
            run("--url", base, "--token-file", scoped, "--account", "direct", "threads", success=False)
            run(*remote, "tokens", "revoke", grant["grant"]["id"])
            run("--url", base, "--token-file", scoped, "status", success=False)
            run(*args, "store", "list", success=False)
        finally:
            process.send_signal(signal.SIGTERM)
            process.wait(timeout=10)
            assert process.returncode == 0
    assert len(run(*args, "store", "list")) == 2
    assert tree(source) == before
print(json.dumps({"result": "passed", "serverRestarts": 2, "wechatRequests": 0, "messageSends": 0}))
