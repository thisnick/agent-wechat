#!/usr/bin/env python3
"""
WeChat data access setup.

Requirements:
  - WeChat must be running and logged in

Usage:
  python3 extract-keys.py [--output db_keys.json]
"""

import subprocess
import json
import os
import sys
import glob
import argparse
import time
import struct

# ── DB access pattern ─────────────
CIPHER_CTX_PATTERN = bytes([
    0x20, 0x00, 0x00, 0x00,  # reserve_sz = 32
    0x10, 0x00, 0x00, 0x00,  # iv_sz = 16
    0x10, 0x00, 0x00, 0x00,  # hmac_sz = 16
    0x00, 0x10, 0x00, 0x00,  # page_sz = 4096
])


def find_wechat_pid():
    for cmd in [["pgrep", "-x", "wechat"], ["pgrep", "-f", "/opt/wechat/wechat"]]:
        try:
            r = subprocess.run(cmd, capture_output=True, text=True)
            pids = r.stdout.strip().split()
            if pids:
                return int(pids[0])
        except Exception:
            pass
    return None


def find_active_account(pid):
    """Detect which wxid account is active by checking open file descriptors."""
    try:
        fd_dir = f"/proc/{pid}/fd"
        for fd in os.listdir(fd_dir):
            try:
                target = os.readlink(os.path.join(fd_dir, fd))
                if "db_storage" in target and target.endswith(".db"):
                    # Path: .../xwechat_files/<account_dir>/db_storage/...
                    idx = target.find("xwechat_files/")
                    if idx >= 0:
                        rest = target[idx + len("xwechat_files/"):]
                        account_dir = rest.split("/")[0]
                        if account_dir:
                            return account_dir
            except (OSError, PermissionError):
                continue
    except (OSError, PermissionError):
        pass
    return None


def find_databases(account_dir=None):
    """Find all WeChat databases, optionally for a specific account."""
    # Try both paths: direct ~/xwechat_files and ~/Documents/xwechat_files
    for candidate in ["~/xwechat_files", "~/Documents/xwechat_files"]:
        base = os.path.expanduser(candidate)
        if os.path.isdir(base):
            break
    if account_dir:
        search = os.path.join(base, account_dir, "db_storage/**/*.db")
    else:
        search = os.path.join(base, "*/db_storage/**/*.db")
    dbs = sorted(f for f in glob.glob(search, recursive=True) if os.path.getsize(f) > 0)
    return dbs


def extract_candidates(pid):
    """Extract DB access credentials from the running process."""
    regions = []
    with open(f"/proc/{pid}/maps") as f:
        for line in f:
            if "rw-" in line:
                parts = line.split()
                addr_range = parts[0].split("-")
                start = int(addr_range[0], 16)
                end = int(addr_range[1], 16)
                regions.append((start, end))

    # Find all matching structures
    ctx_addrs = []
    with open(f"/proc/{pid}/mem", "rb") as mem:
        for start, end in regions:
            size = end - start
            if size > 100 * 1024 * 1024:
                continue
            try:
                mem.seek(start)
                data = mem.read(size)
                pos = 0
                while True:
                    idx = data.find(CIPHER_CTX_PATTERN, pos)
                    if idx == -1:
                        break
                    ctx_addrs.append(start + idx)
                    pos = idx + 1
            except (OSError, OverflowError):
                continue

    # Walk pointer chains to find 32-byte candidates
    def is_key_like(raw):
        if len(raw) != 32:
            return False
        nonzero = sum(1 for b in raw if b != 0)
        unique = len(set(raw))
        printable = sum(1 for b in raw if 0x20 <= b <= 0x7e)
        return nonzero >= 20 and unique >= 10 and printable <= 26

    all_keys = set()

    with open(f"/proc/{pid}/mem", "rb") as mem:
        def _read(addr, size):
            mem.seek(addr)
            return mem.read(size)

        def _read_u64(addr):
            return struct.unpack("<Q", _read(addr, 8))[0]

        def _try_key(addr):
            try:
                raw = _read(addr, 32)
                if is_key_like(raw):
                    all_keys.add(raw.hex())
            except Exception:
                pass

        for ctx in ctx_addrs:
            # Direct offsets around ctx
            for off in range(-256, 513, 8):
                _try_key(ctx + off)
            # Pointer chasing (depth 1 + 2)
            for off in range(-128, 257, 8):
                try:
                    val = _read_u64(ctx + off)
                    if val < 0x10000 or val > 0xffffffffffff:
                        continue
                    for koff in range(0, 129, 8):
                        _try_key(val + koff)
                    for poff in range(0, 65, 8):
                        try:
                            val2 = _read_u64(val + poff)
                            if val2 < 0x10000 or val2 > 0xffffffffffff:
                                continue
                            _try_key(val2)
                            for koff2 in range(8, 65, 8):
                                _try_key(val2 + koff2)
                        except Exception:
                            pass
                except Exception:
                    pass

    return len(ctx_addrs), list(all_keys)


FILTER_LEVELS = [
    # (max_printable, max_zeros, min_unique) — strict to relaxed
    (19, 6, 16),   # Level 0: strict
    (24, 12, 12),  # Level 1: moderate (previous default)
    (28, 18, 8),   # Level 2: relaxed
]


def filter_candidates(keys, level=1):
    max_printable, max_zeros, min_unique = FILTER_LEVELS[level]
    filtered = []
    for key in keys:
        if len(key) != 64:
            continue
        raw = bytes.fromhex(key)
        if sum(1 for b in raw if 0x20 <= b <= 0x7e) > max_printable:
            continue
        if raw.count(0) > max_zeros:
            continue
        if len(set(raw)) < min_unique:
            continue
        filtered.append(key)
    return list(dict.fromkeys(filtered))


def test_key(db_path, key):
    try:
        sql = f"PRAGMA key = \"x'{key}'\";\nPRAGMA cipher_compatibility = 4;\nSELECT count(*) FROM sqlite_master;"
        r = subprocess.run(
            ["sqlcipher", db_path],
            input=sql, capture_output=True, text=True, timeout=5
        )
        if r.returncode == 0:
            lines = [l.strip() for l in r.stdout.strip().split('\n')
                     if l.strip() and l.strip() != 'ok']
            return lines[-1] if lines else "0"
    except subprocess.TimeoutExpired:
        pass
    return None


# ── Image access setup ────────────────────────────────────────────────────────
# Per-build constants: keyed by BuildID prefix (first 8 hex chars).

BUILD_PROFILES = {
    # WeChat Linux v4.1.0.16 aarch64 (BuildID: 5233a112...)
    "5233a112": {
        "image_xor_mask": bytes.fromhex(
            "33927e4d6cb29059a74057c9d988394f"
            "c4ee2c476aae869415d3ed07f826a174"
        ),
    },
    # WeChat Linux v4.1.0.16 x86_64 (BuildID: f8713825...)
    "f8713825": {
        "image_xor_mask": bytes.fromhex(
            "60431bbab72c0b5b868c98f9570bbb1d"
            "41ec867286d3b4af910b6c5bb8c9ee49"
        ),
    },
    # WeChat Linux 4.x aarch64 (BuildID: 3eda8254...)
    "3eda8254": {
        "image_xor_mask": bytes.fromhex(
            "ce41c7c149777ded7b5b2f29d277e277"
            "dcc8a3dc80f5b623ebe0befe633dd9a8"
        ),
    },
    # WeChat Linux 4.x x86_64 (BuildID: eba86b80...)
    "eba86b80": {
        "image_xor_mask": bytes.fromhex(
            "29c63ae609ae3c9826a786367d9a4c3a"
            "7146d19c2fcbfac10f6ed2aa2a4034f4"
        ),
    },
}


def get_build_id(pid):
    """Read the WeChat binary's BuildID.

    Uses /proc/pid/maps to find the actual binary path, since /proc/pid/exe
    may point to a translator (e.g. Rosetta) instead of the real binary.
    """
    wechat_path = None
    with open(f"/proc/{pid}/maps") as f:
        for line in f:
            if "/wechat" in line and line.strip().endswith("/wechat"):
                wechat_path = line.split()[-1]
                break
    if not wechat_path:
        return None
    r = subprocess.run(["readelf", "-n", wechat_path], capture_output=True, text=True)
    for line in r.stdout.split("\n"):
        if "Build ID:" in line:
            return line.split("Build ID:")[1].strip()
    return None


def get_build_profile(pid):
    """Detect the WeChat build and return its memory layout constants."""
    build_id = get_build_id(pid)
    if build_id:
        prefix = build_id[:8]
        if prefix in BUILD_PROFILES:
            print(f"Build: {build_id[:16]}... ({prefix})")
            return BUILD_PROFILES[prefix]
        print(f"WARNING: Unknown BuildID {build_id}. Image key extraction may fail.")
    # Fall back to first profile (aarch64)
    return next(iter(BUILD_PROFILES.values()))


def extract_image_aes_key(pid, profile):
    """Extract the image access key from the running process.

    Returns: 32-char hex string
    """
    import re
    mask = profile["image_xor_mask"]
    hex_bytes = list(range(0x30, 0x3a)) + list(range(0x61, 0x67))
    valid_at = [set(h ^ mask[i] for h in hex_bytes) for i in range(32)]

    # Build regex for fast filtering
    def _byte_class(valid_set):
        return b"[" + b"".join(re.escape(bytes([b])) for b in sorted(valid_set)) + b"]"
    rx = re.compile(b"".join(_byte_class(valid_at[i]) for i in range(4)))

    regions = []
    with open(f"/proc/{pid}/maps") as f:
        for line in f:
            if "rw-" in line:
                parts = line.split()
                addr_range = parts[0].split("-")
                start = int(addr_range[0], 16)
                end = int(addr_range[1], 16)
                regions.append((start, end))

    with open(f"/proc/{pid}/mem", "rb") as mem:
        for start, end in regions:
            size = end - start
            if size > 100 * 1024 * 1024:
                continue
            try:
                mem.seek(start)
                data = mem.read(size)
            except (OSError, OverflowError):
                continue
            for m in rx.finditer(data):
                i = m.start()
                if i + 32 > len(data):
                    break
                raw = data[i:i + 32]
                if all(raw[j] in valid_at[j] for j in range(4, 32)):
                    deobf = bytes(raw[j] ^ mask[j] for j in range(32))
                    decoded = deobf.decode("ascii")
                    if len(set(decoded)) >= 8:
                        return decoded

    raise RuntimeError("Could not find image key. "
                       "Make sure WeChat has sent/received at least one image.")


def extract_image_keys(pid, profile):
    """Extract image access key.

    Returns: dict with "_image_aes".
    """
    print("\nExtracting image access keys...")
    aes_key_hex = extract_image_aes_key(pid, profile)
    print(f"  Image key: {aes_key_hex[:8]}...")
    return {"_image_aes": aes_key_hex}


def main():
    parser = argparse.ArgumentParser(description="WeChat data access setup")
    parser.add_argument("--output", "-o", default=None,
                        help="Output JSON path (default: db_keys.json next to databases)")
    parser.add_argument("--pid", type=int, default=None,
                        help="WeChat PID (auto-detected if not specified)")
    args = parser.parse_args()

    pid = args.pid or find_wechat_pid()
    if not pid:
        print("ERROR: WeChat not running. Launch it and log in first.")
        sys.exit(1)
    print(f"WeChat PID: {pid}")

    # Detect active account from open file descriptors
    account_dir = find_active_account(pid)
    if account_dir:
        print(f"Active account: {account_dir}")
    databases = find_databases(account_dir)
    if not databases:
        # Fallback: try all accounts
        databases = find_databases()
    if not databases:
        print("ERROR: No WeChat databases found in ~/Documents/xwechat_files/")
        sys.exit(1)
    print(f"Databases: {len(databases)}")

    profile = get_build_profile(pid)

    print("Extracting key candidates from memory...")
    ctx_count, raw_keys = extract_candidates(pid)
    print(f"  Structures found: {ctx_count}")
    print(f"  Candidates: {len(raw_keys)}")

    results = {}
    tests = 0
    remaining_dbs = list(databases)
    prev_candidates = set()

    for level in range(len(FILTER_LEVELS)):
        candidates = filter_candidates(raw_keys, level=level)
        # Only try candidates not already tested in a previous pass
        new_candidates = [k for k in candidates if k not in prev_candidates]
        prev_candidates.update(candidates)

        if not new_candidates and level == 0:
            print("ERROR: No candidates found. Is WeChat logged in?")
            sys.exit(1)

        if not new_candidates:
            continue

        label = ["strict", "moderate", "relaxed"][level]
        print(f"\n  Pass {level} ({label}): {len(new_candidates)} new candidates, {len(remaining_dbs)} DBs remaining")

        still_remaining = []
        for db_path in remaining_dbs:
            db_name = os.path.basename(db_path)
            found = False
            for key in new_candidates:
                tests += 1
                count = test_key(db_path, key)
                if count is not None:
                    results[db_name] = {"key": key, "tables": count, "path": db_path}
                    print(f"  {db_name}: {key[:16]}... ({count} tables)")
                    found = True
                    break
            if not found:
                still_remaining.append(db_path)
        remaining_dbs = still_remaining

        if not remaining_dbs:
            break

    for db_path in remaining_dbs:
        print(f"  {os.path.basename(db_path)}: NOT FOUND")

    print(f"\nDone: {len(results)}/{len(databases)} databases resolved ({tests} tests)")

    if args.output:
        out_path = args.output
    else:
        db_dir = os.path.dirname(os.path.dirname(databases[0]))
        out_path = os.path.join(db_dir, "db_keys.json")

    # Extract account ID from path
    account = "unknown"
    for p in databases[0].split("/"):
        if p.startswith("wxid_"):
            account = p.split("_", 2)
            account = account[0] + "_" + account[1] if len(account) > 1 else p
            break

    output = {
        "account": account,
        "extracted_at": time.strftime("%Y-%m-%d %H:%M:%S"),
        "note": "DB access keys",
        "keys": {name: info["key"] for name, info in sorted(results.items())},
    }

    # Image access keys
    try:
        image_keys = extract_image_keys(pid, profile)
        output["keys"].update(image_keys)
    except Exception as e:
        print(f"  Image key setup failed: {e}")

    with open(out_path, 'w') as f:
        json.dump(output, f, indent=2)
    print(f"Saved to: {out_path}")

    not_found = [os.path.basename(db) for db in databases if os.path.basename(db) not in results]
    if not_found:
        print(f"NOT FOUND: {', '.join(not_found)}")
        sys.exit(1)


if __name__ == "__main__":
    main()
