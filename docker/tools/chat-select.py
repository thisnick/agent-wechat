#!/usr/bin/env python3
"""
Programmatic chat selection for WeChat Linux.

Selects a chat by username (e.g. "wxid_xxx", "123@chatroom", "filehelper")
without requiring manual user interaction. Works on both aarch64 and x86_64.

Usage:
    chat-select <username>          # Select a chat, output JSON result
    chat-select --list              # List all sessions as JSON

Output (JSON):
    {"ok": true, "username": "filehelper", "index": 3}
    {"ok": false, "error": "Chat not found in session list"}
    {"ok": true, "sessions": {"filehelper": 0, "wxid_xxx": 1, ...}}
"""
import subprocess
import time
import sys
import json
import os
import re
import shutil
import queue
import threading
import tempfile
import fcntl
from contextlib import contextmanager

# ── Per-build constants ──────────────────────────────────────────────────────
# Keyed by first 8 hex chars of ELF BuildID (same pattern as extract-keys.py).

BUILD_PROFILES = {
    "e9f1cd04": {
        "ARCH": "aarch64", "SELECT_SESSION": 0x48543d0,
        "USERNAME_OFF": 0x130, "ELEM_SIZE": 16,
        "MANAGER_VT_OFF": 0x9fe8c58, "CTRL_OFF": 0xf8,
        "CUR_SESS_OFF": 0x40, "CUR_SESS_UNAME_OFF": 0x130,
        "VEC_KEY_OFF": 0x178, "VECTOR_LAYOUT": "controller_map",
    },
    # WeChat Linux v4.1.13.23 x86_64 (BuildID: ce28c3471d532eeb1f136482eeb4d0bdfd59c06e)
    "ce28c347": {
        "ARCH": "x86_64",
        "SELECT_SESSION": 0x48ae510,
        "USERNAME_OFF": 0x130,
        "ELEM_SIZE": 16,
        "MANAGER_VT_OFF": 0xa6a7d78,
        "CTRL_OFF": 0x1a8,
        "CUR_SESS_OFF": 0x40,
        "CUR_SESS_UNAME_OFF": 0x98,
        "VEC_KEY_OFF": 0x178,
        "VEC_MAP_OFF": 0xf8,
    },
    # WeChat Linux v4.1.0.16 aarch64 (BuildID: 5233a112...)
    "5233a112": {
        "ARCH": "aarch64",
        "SELECT_SESSION": 0x38bd3d0,
        "USERNAME_OFF": 0x120,
        "ELEM_SIZE": 16,
        "MANAGER_VT_OFF": 0x7b3be28,
        "CTRL_OFF": 0xd8,
        "CUR_SESS_OFF": 0x40,
        "CUR_SESS_UNAME_OFF": 0x120,
        "VEC_KEY_OFF": 0x158,
    },
    # WeChat Linux v4.1.0.16 x86_64 (BuildID: f8713825...)
    "f8713825": {
        "ARCH": "x86_64",
        "SELECT_SESSION": 0x3909e50,
        "USERNAME_OFF": 0x138,
        "ELEM_SIZE": 16,
        "MANAGER_VT_OFF": 0x7fc7f50,
        "CTRL_OFF": 0x180,
        "CUR_SESS_OFF": 0x40,
        "CUR_SESS_UNAME_OFF": 0x98,
        "VEC_KEY_OFF": 0x168,
        "VEC_MAP_OFF": 0xe8,
    },
    # WeChat Linux 4.x aarch64 (BuildID: 3eda8254...)
    "3eda8254": {
        "ARCH": "aarch64",
        "SELECT_SESSION": 0x3937ff8,
        "USERNAME_OFF": 0x120,
        "ELEM_SIZE": 16,
        "MANAGER_VT_OFF": 0x7ce8ea8,
        "CTRL_OFF": 0xd8,
        "CUR_SESS_OFF": 0x40,
        "CUR_SESS_UNAME_OFF": 0x120,
        "VEC_KEY_OFF": 0x158,
    },
    # WeChat Linux 4.x x86_64 (BuildID: eba86b80...)
    "eba86b80": {
        "ARCH": "x86_64",
        "SELECT_SESSION": 0x3988e60,
        "USERNAME_OFF": 0x120,
        "ELEM_SIZE": 16,
        "MANAGER_VT_OFF": 0x8197d10,
        "CTRL_OFF": 0x180,
        "CUR_SESS_OFF": 0x40,
        "CUR_SESS_UNAME_OFF": 0x98,
        "VEC_KEY_OFF": 0x168,
        "VEC_MAP_OFF": 0xe8,
    },
}

FRIDA_BIN = shutil.which("frida") or "/usr/local/bin/frida"


def log(msg):
    """Log to stderr (not mixed with JSON stdout)."""
    print(msg, file=sys.stderr, flush=True)


_GH_RE = re.compile(r'^gh_[0-9a-f]+$')

def is_official_account(username):
    """WeChat official/service accounts match gh_<hex>."""
    return bool(_GH_RE.match(username))


def result_json(ok, **kwargs):
    """Print JSON result and exit."""
    out = {"ok": ok, **kwargs}
    print(json.dumps(out))
    sys.exit(0 if ok else 1)


def get_pid():
    """Get WeChat PID."""
    for cmd in [["pgrep", "-x", "wechat"], ["pgrep", "-f", "/opt/wechat/wechat"]]:
        try:
            r = subprocess.run(cmd, capture_output=True, text=True)
            pids = r.stdout.strip().split()
            if pids:
                return pids[0]
        except Exception:
            pass
    return None


def get_build_id(pid):
    """Read the WeChat binary's BuildID from /proc/pid/maps + readelf."""
    wechat_path = None
    try:
        with open(f"/proc/{pid}/maps") as f:
            for line in f:
                if "/wechat" in line and line.strip().endswith("/wechat"):
                    wechat_path = line.split()[-1]
                    break
    except Exception:
        pass
    if not wechat_path:
        return None
    try:
        r = subprocess.run(["readelf", "-n", wechat_path], capture_output=True, text=True)
        for line in r.stdout.split("\n"):
            if "Build ID:" in line:
                return line.split("Build ID:")[1].strip()
    except Exception:
        pass
    return None


def get_profile(pid):
    """Look up build profile by BuildID prefix."""
    build_id = get_build_id(pid)
    if not build_id:
        return None, "Could not read WeChat BuildID"
    prefix = build_id[:8]
    log(f"[chat-select] BuildID: {build_id[:16]}... prefix={prefix}")
    profile = BUILD_PROFILES.get(prefix)
    if not profile:
        return None, f"Unknown BuildID prefix: {prefix}. Known: {list(BUILD_PROFILES.keys())}"
    log(f"[chat-select] Profile: SELECT_SESSION=0x{profile['SELECT_SESSION']:x} USERNAME_OFF=0x{profile['USERNAME_OFF']:x}")
    return profile, None


def has_open_chat_pane(tree):
    """Off-screen chats have no SELECTED row; their message pane is still visible."""
    found = set()

    def walk(node):
        if not isinstance(node, dict):
            return
        bounds = node.get('bounds') or {}
        if bounds.get('width', 0) > 0 and bounds.get('height', 0) > 0:
            if (node.get('role'), node.get('name')) in (
                    ('list', 'Messages'), ('push-button', 'Send')):
                found.add((node['role'], node['name']))
        for child in node.get('children', []):
            walk(child)

    walk(tree)
    return len(found) == 2


def find_chat_item_from_a11y(require_open_chat=False):
    """Use a11y-dump to find a clickable chat list item. Returns (x, y) or None."""
    try:
        log("[chat-select] Getting a11y tree...")
        r = subprocess.run(
            ["/opt/tools/a11y-dump", "--format", "json"],
            capture_output=True, text=True, timeout=10,
            env={**os.environ, "QT_ACCESSIBILITY": "1", "QT_LINUX_ACCESSIBILITY_ALWAYS_ON": "1"}
        )
        if r.returncode != 0:
            log(f"[chat-select] a11y-dump failed: {r.stderr}")
            return None

        tree = json.loads(r.stdout)
        # Walk tree to find: list[name="Chats"] > list-item with bounds
        items = []
        _find_chat_list_items(tree, items, in_chat_list=False)
        if require_open_chat and not (
                any('SELECTED' in item.get('states', []) for item in items)
                or has_open_chat_pane(tree)):
            return None
        if not items:
            log("[chat-select] No list-item found in Chats list")
            return None

        # Return center of the first suitable item with valid bounds.
        item = items[0]
        b = item["bounds"]
        cx = b["x"] + b["width"] // 2
        cy = b["y"] + b["height"] // 2
        log(f"[chat-select] Found chat item: name={item.get('name', '?')!r} bounds={b} -> click ({cx}, {cy})")
        return (cx, cy)
    except Exception as e:
        log(f"[chat-select] a11y error: {e}")
        return None


def _find_chat_list_items(node, items, in_chat_list):
    """Recursively find list-item nodes inside the Chats list."""
    if not node or not isinstance(node, dict):
        return

    role = node.get("role", "")
    name = node.get("name", "")

    # Detect if we're inside the chat list
    if role == "list" and name == "Chats":
        in_chat_list = True

    if in_chat_list and role == "list-item" and node.get("bounds"):
        items.append(node)

    for child in node.get("children", []):
        _find_chat_list_items(child, items, in_chat_list)


def write_js(path, content):
    with open(path, "w") as f:
        f.write(content)


@contextmanager
def script_path():
    fd, path = tempfile.mkstemp(prefix='wechat-chat-select-', suffix='.js')
    os.close(fd)
    try:
        yield path
    finally:
        os.unlink(path)


def read_lines_until(proc, timeout, marker):
    """Bounded reads, including when Frida produces no output at all."""
    if not hasattr(proc, '_output_queue'):
        proc._output_queue = queue.Queue()

        def pump():
            try:
                for line in proc.stdout:
                    proc._output_queue.put(line.rstrip())
            finally:
                proc._output_queue.put(None)

        threading.Thread(target=pump, daemon=True).start()
    lines = []
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            line = proc._output_queue.get(timeout=max(0, deadline - time.monotonic()))
        except queue.Empty:
            break
        if line is None:
            break
        lines.append(line)
        if marker and line.strip() == marker:
            break
    return lines


READ_STD_STRING_JS = """
function readStdString(addr) {
    try {
        if (!addr || addr.isNull() || addr.compare(ptr(0x10000)) < 0) return null;
        var b0 = addr.readU8();
        if (b0 & 1) {
            var len = Number(addr.add(8).readU64());
            var dp = addr.add(16).readPointer();
            if (len > 0 && len < 512 && dp && !dp.isNull()) return dp.readUtf8String(len);
        } else {
            var len = b0 >> 1;
            if (len > 0 && len <= 22) return addr.add(1).readUtf8String(len);
        }
    } catch(e) {}
    return null;
}
"""


def run_frida_script(pid, script_path, timeout=30, stop_on="SCRIPT_DONE"):
    """Run a frida script, return output lines."""
    proc = subprocess.Popen(
        [FRIDA_BIN, "-p", pid, "-l", script_path, "--runtime=v8", "-q"],
        stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
        stdin=subprocess.PIPE, text=True, bufsize=1,
    )
    try:
        return read_lines_until(proc, timeout, stop_on)
    finally:
        kill_frida(proc)


def run_frida_bg(pid, script_path):
    """Start frida in background, wait for READY, return process."""
    proc = subprocess.Popen(
        [FRIDA_BIN, "-p", pid, "-l", script_path, "--runtime=v8", "-q", "-t", "inf"],
        stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
        stdin=subprocess.PIPE, text=True, bufsize=1,
    )
    lines = read_lines_until(proc, 10, 'READY')
    if 'READY' not in lines:
        kill_frida(proc)
        raise RuntimeError('Selection hook did not become ready')
    return proc


def kill_frida(proc):
    try:
        proc.stdin.close()
    except Exception:
        pass
    proc.terminate()
    try:
        proc.wait(timeout=3)
    except Exception:
        proc.kill()
        proc.wait(timeout=3)


def vector_access_js(profile):
    """Resolve the live ordered vector from a controller named ctrl."""
    if profile.get('VECTOR_LAYOUT') == 'controller_map' or 'VEC_MAP_OFF' in profile:
        inner = ('ctrl' if profile.get('VECTOR_LAYOUT') == 'controller_map'
                 else f"ctrl.add(0x{profile['VEC_MAP_OFF']:x}).readPointer()")
        return f"""
    var hmInner = {inner};
    var hmNode = hmInner.add(0x28).readPointer();
    var vectorBegin = ptr(0), vectorEnd = ptr(0);
    for (var n = 0; n < 100 && !hmNode.isNull(); n++) {{
        if (readStdString(hmNode.add(0x10)) === 'normal_key') {{
            vectorBegin = hmNode.add(0x28).readPointer();
            vectorEnd = hmNode.add(0x30).readPointer();
            break;
        }}
        hmNode = hmNode.readPointer();
    }}
"""
    return """
    var vectorBegin = ctrl.add(0x0).readPointer();
    var vectorEnd = ctrl.add(0x8).readPointer();
"""


def uses_filtered_indices(profile):
    # Legacy ARM exposes an unfiltered direct vector. Map-backed profiles use
    # the native ordered vector and must preserve its exact indices.
    return 'VEC_MAP_OFF' not in profile and profile.get('VECTOR_LAYOUT') != 'controller_map'


def enumerate_sessions(pid, profile):
    """Find manager via vtable, read live vector + current selection.

    Manager-anchored approach: finds the live manager object via its vtable
    pointer (heap scan), then reads the session vector directly from the
    controller. This is immune to stale session data after re-login.

    Returns (dict of {username: index}, vector_base_hex, vector_count, current_sel_username|None).
    """
    username_off = profile["USERNAME_OFF"]
    elem_size = profile["ELEM_SIZE"]
    manager_vt_off = profile["MANAGER_VT_OFF"]
    ctrl_off = profile["CTRL_OFF"]
    cur_sess_off = profile["CUR_SESS_OFF"]
    cur_sess_uname_off = profile["CUR_SESS_UNAME_OFF"]
    vec_key_off = profile["VEC_KEY_OFF"]

    # Manager validation: check "normal_key" string at VEC_KEY_OFF.
    # On x86_64 multiple managers share the same vtable; this picks the right one.
    validate_js = f'var k = readStdString(hit.address.add(0x{vec_key_off:x})); if (k !== "normal_key") return;'

    vec_access_js = vector_access_js(profile)

    source = f"""
var w = Process.getModuleByName("wechat");
var b = w.base;
var UNAME_OFF = 0x{username_off:x};
var ELEM_SZ = {elem_size};
var MANAGER_VT = b.add(0x{manager_vt_off:x});
var CTRL_OFF = 0x{ctrl_off:x};
var CUR_SESS_OFF = 0x{cur_sess_off:x};
var CUR_SESS_UNAME = 0x{cur_sess_uname_off:x};
{READ_STD_STRING_JS}

function ptrToPattern(p) {{
    var buf = Memory.alloc(8);
    buf.writePointer(p);
    var hex = [];
    for (var i = 0; i < 8; i++) hex.push(("0" + buf.add(i).readU8().toString(16)).slice(-2));
    return hex.join(" ");
}}

// Step 1: Find manager via vtable scan
var vtPattern = ptrToPattern(MANAGER_VT);
var manager = null;

Process.enumerateRanges("rw-").forEach(function(range) {{
    if (manager || range.size > 200*1024*1024) return;
    try {{
        Memory.scanSync(range.base, range.size, vtPattern).forEach(function(hit) {{
            if (manager) return;
            try {{
                var ctrl = hit.address.add(CTRL_OFF).readPointer();
                if (!ctrl.isNull() && ctrl.compare(ptr(0x10000)) >= 0) {{
                    {validate_js}
                    manager = hit.address;
                }}
            }} catch(e) {{}}
        }});
    }} catch(e) {{}}
}});

if (!manager) {{
    console.log("ERROR: manager not found via vtable scan");
    console.log("SCRIPT_DONE");
}} else {{
    console.log("MANAGER " + manager);

    // Step 2: Get vector begin/end
    var ctrl = manager.add(CTRL_OFF).readPointer();
{vec_access_js}
    if (vectorBegin.isNull() || vectorEnd.isNull() || vectorEnd.compare(vectorBegin) <= 0) {{
        console.log("ERROR: invalid vector pointers begin=" + vectorBegin + " end=" + vectorEnd);
        console.log("SCRIPT_DONE");
    }} else {{
        var count = vectorEnd.sub(vectorBegin).toInt32() / ELEM_SZ;
        console.log("VECTOR " + vectorBegin + " count=" + count);

        // Step 3: Enumerate sessions
        for (var i = 0; i < count; i++) {{
            try {{
                var ep = vectorBegin.add(i * ELEM_SZ).readPointer();
                if (ep.isNull() || ep.compare(ptr(0x10000)) < 0) continue;
                var u = readStdString(ep.add(UNAME_OFF));
                if (u) console.log("SESSION " + i + " " + u);
            }} catch(e) {{}}
        }}

        // Step 4: Read current selection
        var curSelName = "NONE";
        try {{
            var curPtr = ctrl.add(CUR_SESS_OFF).readPointer();
            if (!curPtr.isNull() && curPtr.compare(ptr(0x10000)) >= 0) {{
                var s = readStdString(curPtr.add(CUR_SESS_UNAME));
                if (s) curSelName = s;
            }}
        }} catch(e) {{}}
        console.log("CURRENT_SEL " + curSelName);

        console.log("SCRIPT_DONE");
    }}
}}
"""

    for attempt in range(3):
        if attempt > 0:
            log(f"[chat-select] Enumerate retry {attempt}...")
            time.sleep(2)
        log(f"[chat-select] Running Frida enumerate script (attempt {attempt})...")
        with script_path() as path:
            write_js(path, source)
            lines = run_frida_script(pid, path, timeout=45)
        # Parse raw sessions from Frida output (raw vector index -> username)
        raw_sessions = []  # [(raw_index, username), ...] in vector order
        vector_base = None
        vector_count = 0
        current_sel = None
        for line in lines:
            stripped = line.strip()
            if stripped.startswith("VECTOR "):
                parts = stripped.split()
                vector_base = parts[1]
                vector_count = int(parts[2].split("=")[1])
            elif stripped.startswith("CURRENT_SEL "):
                sel = stripped.split(None, 1)[1]
                if sel not in ("NONE",):
                    current_sel = sel
            elif stripped.startswith("SESSION"):
                parts = stripped.split(None, 2)
                if len(parts) >= 3:
                    raw_sessions.append((int(parts[1]), parts[2]))

        if raw_sessions:
            raw_sessions.sort(key=lambda x: x[0])

            gh_count = sum(1 for _, u in raw_sessions if is_official_account(u))
            log(f"[chat-select] Raw vector: {len(raw_sessions)} sessions ({gh_count} official accounts), base={vector_base} count={vector_count}")

            # Preserve native vector indices even when excluding an account
            # from the public result. Never shift later entries across a hole.
            sessions = {}
            for raw_index, uname in raw_sessions:
                if is_official_account(uname):
                    continue
                sessions[uname] = len(sessions) if uses_filtered_indices(profile) else raw_index

            if current_sel:
                log(f"[chat-select] Current selection: {current_sel}")

            log(f"[chat-select] Filtered: {len(sessions)} sessions (excluded {gh_count} official accounts)")

            return sessions, vector_base, vector_count, current_sel
    return {}, None, 0, None


def selection_script(profile, target):
    return f"""
var b = Process.getModuleByName('wechat').base;
var target = {json.dumps(target)};
var attempted = false, returned = false, controller = null, selectedIndex = -1;
{READ_STD_STRING_JS}
Interceptor.attach(b.add(0x{profile['SELECT_SESSION']:x}), {{
    onEnter: function(args) {{
        if (attempted) return;
        try {{
            var manager = args[0];
            if (!manager.readPointer().equals(b.add(0x{profile['MANAGER_VT_OFF']:x})) ||
                readStdString(manager.add(0x{profile['VEC_KEY_OFF']:x})) !== 'normal_key') return;
            attempted = true;
            var ctrl = manager.add(0x{profile['CTRL_OFF']:x}).readPointer();
            {vector_access_js(profile)}
            var count = vectorEnd.sub(vectorBegin).toInt32() / {profile['ELEM_SIZE']};
            if (vectorBegin.isNull() || count < 1 || count > 10000 || !Number.isInteger(count))
                throw new Error('invalid live vector');
            var filteredIndex = 0;
            for (var i = 0; i < count; i++) {{
                var session = vectorBegin.add(i * {profile['ELEM_SIZE']}).readPointer();
                if (session.isNull()) continue;
                var username = readStdString(session.add(0x{profile['USERNAME_OFF']:x}));
                if (!username || /^gh_[0-9a-f]+$/.test(username)) continue;
                if (username === target) {{
                    selectedIndex = {'filteredIndex' if uses_filtered_indices(profile) else 'i'};
                    break;
                }}
                filteredIndex++;
            }}
            if (selectedIndex < 0) throw new Error('target no longer in live vector');
            controller = ctrl;
            args[1] = ptr(selectedIndex);
            this.redirected = true;
            console.log('REDIRECT');
        }} catch(e) {{
            // An invalid index makes the native selector return without opening
            // the incidental chat that was clicked to enter the UI thread.
            args[1] = ptr(-1);
            console.log('SELECTION_ERROR ' + e);
        }}
    }},
    onLeave: function() {{
        if (this.redirected) returned = true;
    }}
}});
// Do not detach from inside a native callback. Python unloads after return and
// readback, or after a bounded failure. No direct NativeFunction invocation.
var timer = setInterval(function() {{
    if (!returned || controller === null) return;
    try {{
        var current = controller.add(0x{profile['CUR_SESS_OFF']:x}).readPointer();
        if (!current.isNull() && readStdString(current.add(0x{profile['CUR_SESS_UNAME_OFF']:x})) === target) {{
            console.log('SELECTED_INDEX ' + selectedIndex);
            console.log('SELECTION_VERIFIED');
            clearInterval(timer);
        }}
    }} catch(e) {{}}
}}, 100);
Interceptor.flush();
console.log('READY');
"""


def select_chat(pid, profile, target, click_coords):
    """Resolve identity on the UI thread, then require return and readback."""
    with script_path() as path:
        write_js(path, selection_script(profile, target))
        proc = run_frida_bg(pid, path)
        try:
            cx, cy = click_coords
            click = subprocess.run(['/opt/tools/click', str(cx), str(cy)],
                                   timeout=5, capture_output=True, text=True)
            if click.returncode != 0:
                return None
            lines = read_lines_until(proc, 8, 'SELECTION_VERIFIED')
            if 'SELECTION_VERIFIED' not in lines:
                log('[chat-select] Selection did not return with the requested identity')
                log('[chat-select] Hook status: ' + repr(lines))
                return None
            for line in lines:
                if line.startswith('SELECTED_INDEX '):
                    return int(line.split()[1])
            return None
        finally:
            kill_frida(proc)


def main():
    # Parse args: chat-select [--force] [--click-xy X Y] [--list] <username>
    args = sys.argv[1:]
    if not args:
        result_json(False, error="Usage: chat-select [--force] [--click-xy X Y] <username> | chat-select --list")

    click_xy = None
    positional = []

    i = 0
    while i < len(args):
        if args[i] == "--force":
            # Compatibility flag: never toggle an already verified-open chat.
            # A UI with no selected chat still requires the click below.
            i += 1
        elif args[i] == "--click-xy":
            if i + 2 >= len(args):
                result_json(False, error="--click-xy requires X Y arguments")
            click_xy = (int(args[i + 1]), int(args[i + 2]))
            i += 3
        else:
            positional.append(args[i])
            i += 1

    if not positional:
        result_json(False, error="Usage: chat-select [--force] [--click-xy X Y] <username> | chat-select --list")

    pid = get_pid()
    if not pid:
        result_json(False, error="WeChat is not running")
    log(f"[chat-select] WeChat PID={pid}")

    profile, err = get_profile(pid)
    if not profile:
        result_json(False, error=err)

    # Enumerate sessions
    log("[chat-select] Enumerating sessions...")
    sessions, vector_base, vector_count, current_sel = enumerate_sessions(pid, profile)
    if not sessions:
        result_json(False, error="No sessions found. Is WeChat logged in with chats visible?")

    # --list mode
    if positional[0] == "--list":
        result_json(True, sessions=sessions)

    target = positional[0]
    if is_official_account(target):
        result_json(False, error=f"'{target}' is an official account and cannot be opened")
    if target not in sessions:
        matches = [u for u in sessions if target.lower() in u.lower()]
        if matches:
            result_json(False, error=f"'{target}' not found. Close matches: {matches[:5]}")
        else:
            result_json(False, error=f"'{target}' not found in session list ({len(sessions)} sessions)")

    target_index = sessions[target]
    log(f"[chat-select] Target: {target} -> index {target_index}")

    # A cached/native current identity is not proof that the UI is responsive.
    available_click = find_chat_item_from_a11y()
    if not available_click:
        result_json(False, error='Chat UI is not responsive or no chat list is visible')

    # ARM reselecting the current native session toggles it closed. A native
    # identity alone is insufficient (the UI may be closed/stale), so require
    # an open chat UI as well (the selected row may be off-screen). This also
    # applies to legacy --force.
    if current_sel == target and find_chat_item_from_a11y(require_open_chat=True):
        log(f"[chat-select] Target already selected (current_sel={current_sel}), skipping")
        result_json(True, username=target, index=target_index, skipped=True)

    # Find click coordinates: use --click-xy if provided, else fall back to a11y
    click_coords = click_xy or available_click
    if not click_coords:
        result_json(False, error="No clickable chat item found in a11y tree. Is the chat list visible?")

    # Hook and click
    if not vector_base:
        result_json(False, error="Session vector base address not found")
    index = select_chat(pid, profile, target, click_coords)
    if index is not None and find_chat_item_from_a11y(require_open_chat=True):
        result_json(True, username=target, index=index)
    else:
        result_json(False, error="Selection not verified: requested chat did not open or UI did not respond")


if __name__ == "__main__":
    # Serialize selectors: two simultaneous argument-rewriting hooks are unsafe.
    with open('/tmp/wechat-chat-select.lock', 'a') as lock:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            result_json(False, error='Another chat selection is in progress')
        try:
            main()
        except Exception as exc:
            log(f'[chat-select] {type(exc).__name__}: {exc}')
            result_json(False, error='Chat selection failed or timed out')
