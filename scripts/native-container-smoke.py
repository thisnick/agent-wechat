#!/usr/bin/env python3
"""Run only a synthetic ephemeral native image, without ports or account material."""
import json
import select
import subprocess
import sys
import time
import uuid

image = sys.argv[1]
# Check the image directories before overlaying disposable tmpfs. This catches
# incompatible 0755 roots before a fresh Docker volume would inherit them.
subprocess.run(["docker", "run", "--rm", "--network", "none", "--read-only", "--cap-drop", "ALL",
                "--security-opt", "no-new-privileges", "--entrypoint", "/bin/sh", image, "-c",
                'test "$(id -u)" = 10001 && test "$(stat -c %a /var/lib/agent-wechat)" = 700 '
                '&& test "$(stat -c %a /var/cache/agent-wechat)" = 700 '
                '&& test "$TMPDIR" = /var/cache/agent-wechat'], check=True)
name = "native-image-validation-" + uuid.uuid4().hex[:12]
process = subprocess.Popen([
    "docker", "run", "--rm", "--name", name, "--network", "none", "--read-only", "--cap-drop", "ALL",
    "--security-opt", "no-new-privileges", "--tmpfs", "/var/lib/agent-wechat:rw,uid=10001,gid=10001,mode=0700,size=32m",
    "--tmpfs", "/var/cache/agent-wechat:rw,uid=10001,gid=10001,mode=0700,size=16m",
    image, "serve", "--listen", "127.0.0.1:0", "--data-dir", "/var/lib/agent-wechat",
], stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
try:
    ready = False
    deadline = time.monotonic() + 20
    while time.monotonic() < deadline and process.poll() is None:
        readable, _, _ = select.select([process.stderr], [], [], min(1, max(0, deadline - time.monotonic())))
        if not readable:
            continue
        line = process.stderr.readline()
        try:
            value = json.loads(line)
        except (ValueError, TypeError):
            continue
        if value.get("state") == "ready":
            ready = True
            break
    assert ready, "native image did not reach readiness as non-root with read-only image and private data/scratch mounts"
    subprocess.run(["docker", "stop", "--time", "10", name], check=True, stdout=subprocess.DEVNULL)
    process.wait(timeout=15)
    assert process.returncode == 0, "native image did not stop gracefully"
    print("Native image smoke passed: UID10001, private data/scratch permissions, read-only image startup and graceful stop. No ports, profile, login or send.")
finally:
    if process.poll() is None:
        subprocess.run(["docker", "stop", "--time", "10", name], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        process.wait(timeout=15)
