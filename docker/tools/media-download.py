#!/usr/bin/env python3
"""Private line-delimited IPC worker; one attachment per WeChat process/account."""
import json
from contextlib import ExitStack
from pathlib import Path
import re
import subprocess
import sys

BUILDS = {
    'ce28c3471d532eeb1f136482eeb4d0bdfd59c06e',
    'e9f1cd045de714536a9e739aafa6d8362f317cd0',
}
MAX_INPUT = 2 * 1024 * 1024


def validate_metadata(value):
    if not isinstance(value, dict):
        raise ValueError('Expected object')
    for field in ('accountId', 'senderId'):
        text = value.get(field)
        if (not isinstance(text, str) or not re.fullmatch(r'[A-Za-z0-9_-]{1,128}', text)):
            raise ValueError('Invalid peer')
    chat = value.get('chatId')
    if not isinstance(chat, str) or not re.fullmatch(r'(?:[A-Za-z0-9_-]{1,128}|[0-9]{1,64}@chatroom)', chat):
        raise ValueError('Invalid chat')
    if chat == value['accountId']:
        raise ValueError('Unvalidated peer')
    if chat == 'filehelper' and value['senderId'] != value['accountId']:
        raise ValueError('File Transfer sender mismatch')
    if not chat.endswith('@chatroom') and value['senderId'] not in (chat, value['accountId']):
        raise ValueError('Sender mismatch')
    if type(value.get('local_type')) is not int or value['local_type'] not in (3, 43, (6 << 32) | 49):
        raise ValueError('Unvalidated message type')
    for field in ('local_id', 'create_time'):
        if type(value.get(field)) is not int or not 0 < value[field] <= 0xffffffff:
            raise ValueError('Invalid integer')
    for field in ('server_id', 'sort_seq'):
        text = value.get(field)
        if not isinstance(text, str) or not re.fullmatch(r'[1-9][0-9]{0,19}', text) or int(text) > 2**64 - 1:
            raise ValueError('Invalid identifier')
    text = value.get('content')
    if not isinstance(text, str) or not text or '\0' in text or len(text.encode()) > 1024 * 1024:
        raise ValueError('Invalid body')
    fields = ('chatId', 'accountId', 'senderId', 'local_id', 'local_type', 'server_id', 'sort_seq', 'create_time', 'content')
    return {field: value[field] for field in fields}


def build_id(pid):
    notes = subprocess.check_output(['readelf', '-n', f'/proc/{pid}/exe'], text=True, timeout=5)
    found = re.findall(r'Build ID:\s*([0-9a-f]+)', notes)
    if len(found) != 1 or found[0] not in BUILDS:
        raise ValueError('Unsupported build')
    return found[0]


def close_attachment(session, script):
    # The synchronous RPC has completed before EOF/error cleanup reaches here.
    # Explicitly release callbacks before disconnecting from the target.
    try:
        if not session.is_detached and script is not None:
            script.unload()
    finally:
        if not session.is_detached:
            session.detach()


def serve(cleanup):
    import frida
    session = script = None
    identity = None
    while True:
        line = sys.stdin.buffer.readline(MAX_INPUT + 1)
        if not line:
            return
        try:
            if len(line) > MAX_INPUT or not line.endswith(b'\n'):
                return
            request = json.loads(line)
            metadata = validate_metadata(request['metadata'])
            pid = request['pid']
            if type(pid) is not int or pid <= 0:
                raise ValueError('Invalid process')
            start = Path(f'/proc/{pid}/stat').read_text().rsplit(')', 1)[1].split()[19]
            current = (pid, start, metadata['accountId'])
            if identity is not None and identity != current:
                # Rust creates a new worker for a changed process/account.
                raise ValueError('Stale worker')
            if session is None:
                build = build_id(pid)
                session = frida.attach(pid)
                cleanup.callback(lambda: close_attachment(session, script))
                source = Path(__file__).with_suffix('.js').read_text()
                script = session.create_script('const buildId=' + json.dumps(build) + ';\n' + source)
                script.load()
                identity = current
            if session.is_detached:
                raise ValueError('Detached process')
            result = script.exports_sync.enqueue(metadata)
            print(json.dumps(result), flush=True)
        except Exception:
            # Never emit message XML, credentials, Frida source or native addresses.
            print('{"status":"error"}', flush=True)
            return


def main():
    with ExitStack() as cleanup:
        serve(cleanup)


if __name__ == '__main__':
    main()
