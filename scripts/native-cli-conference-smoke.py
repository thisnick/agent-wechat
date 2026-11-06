#!/usr/bin/env python3
"""Generated PCM through the actual CLI, Rust API and isolated Pulse devices.

No WeChat account, provider, profile or network outside loopback is used.
Run via native-cli-conference-smoke.sh for the isolated test container.
"""
import os, sys, json, time, subprocess, pty, fcntl, termios, struct, threading, select, math, wave, signal, array
from pathlib import Path
import argparse, tempfile, shutil
parser = argparse.ArgumentParser(description='Account-free real CLI/API conference audio and terminal check; requires PulseAudio tools.')
parser.add_argument('--wx', required=True, type=Path)
parser.add_argument('--fixture', required=True, type=Path)
args = parser.parse_args()
wx_binary = str(args.wx.resolve())
fixture_binary = str(args.fixture.resolve())
for tool in ['pulseaudio', 'parec', 'paplay']:
    if shutil.which(tool) is None:
        raise SystemExit('Missing test tool: ' + tool)
temporary = tempfile.TemporaryDirectory(prefix='wechat-cli-conference-')
root = Path(temporary.name)
out = root / 'logs'
out.mkdir(mode=448)
runtime = root / 'runtime'
runtime.mkdir(mode=448, exist_ok=True)
env = dict(os.environ, XDG_RUNTIME_DIR=str(runtime), PULSE_RUNTIME_PATH=str(runtime), PULSE_SERVER='unix:' + str(runtime / 'native'), TERM='xterm-256color', TMPDIR=str(root), XDG_CONFIG_HOME=str(root / 'config'), XDG_DATA_HOME=str(root / 'data'))
config = root / 'pulse.pa'
config.write_text(f'load-module module-native-protocol-unix socket={runtime}/native auth-anonymous=1\nload-module module-null-sink sink_name=cli_playback rate=16000 channels=1\nload-module module-null-sink sink_name=cli_capture rate=16000 channels=1\nset-default-sink cli_playback\nset-default-source cli_capture.monitor\n')
children = []
clients = []
pump_stop = threading.Event()
audio_samples = []
samples_lock = threading.Lock()
fixture = None

class Client:

    def __init__(self, args):
        self.master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 28, 120, 0, 0))
        self.text = bytearray()
        self.lock = threading.Lock()
        self.stop = threading.Event()

        def session():
            os.setsid()
            fcntl.ioctl(0, termios.TIOCSCTTY, 0)
        self.process = subprocess.Popen([wx_binary, '--server', ready['baseUrl'], '--token-file', ready['callsTokenFile'], '--account', ready['accountId'], *args], stdin=slave, stdout=slave, stderr=slave, env=env, preexec_fn=session)
        os.close(slave)
        clients.append(self)
        self.reader = threading.Thread(target=self.read, daemon=True)
        self.reader.start()

    def read(self):
        while not self.stop.is_set():
            if select.select([self.master], [], [], 0.1)[0]:
                try:
                    raw = os.read(self.master, 65536)
                except OSError:
                    return
                if not raw:
                    return
                with self.lock:
                    self.text.extend(raw)

    def output(self):
        with self.lock:
            return bytes(self.text).decode(errors='replace')

    def send(self, text):
        os.write(self.master, (text + '\n').encode())

    def expect(self, text, start=0, timeout=12):
        until = time.monotonic() + timeout
        while time.monotonic() < until:
            if text in self.output()[start:]:
                return
            if self.process.poll() is not None:
                raise RuntimeError('cli_exited_before_expected_state:' + text)
            time.sleep(0.02)
        raise RuntimeError('cli_state_timeout:' + text)

    def finish(self):
        code = self.process.wait(timeout=12)
        time.sleep(0.1)
        assert code == 0, ('cli_exit', code)
        text = self.output()
        assert 'ownerToken' not in text and 'synthetic-owner' not in text
        (out / f"client-{len(list(out.glob('client-*.log')))}.log").write_text(text)
        self.stop.set()
        self.reader.join(timeout=1)
        os.close(self.master)
lock = threading.Lock()
serial = 0

def request(op, **fields):
    global serial
    with lock:
        serial += 1
        ident = str(serial)
        fixture.stdin.write(json.dumps(dict(id=ident, op=op, **fields)) + '\n')
        fixture.stdin.flush()
        if not select.select([fixture.stdout], [], [], 8)[0]:
            raise RuntimeError('fixture_timeout:' + op)
        value = json.loads(fixture.stdout.readline())
        assert value['id'] == ident and value.get('ok'), (op, value.get('code'))
        return value

def until(fn, timeout=12):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        value = fn()
        if value:
            return value
        time.sleep(0.03)
    raise RuntimeError('condition_timeout')

def stats():
    return request('stats')['stats']

def sources(mic_a=True, b=True, generation_b=1):
    rows = [dict(trackId=7, generation=1, contactId='Alice', microphoneEnabled=mic_a)]
    if b:
        rows.append(dict(trackId=8, generation=generation_b, contactId='Bob', microphoneEnabled=True))
    request('conference_sources', sources=rows)
stream_state = {'a': True, 'b': True, 'generation_b': 1}
pump_error = []

def pump():
    sequence = {7: 0, 8: 0}
    origin = time.monotonic()
    try:
        while not pump_stop.is_set():
            slot = int((time.monotonic() - origin) / 0.02)
            for track, value, enabled, generation in [(7, 1000, stream_state['a'], 1), (8, 2000, stream_state['b'], stream_state['generation_b'])]:
                if enabled:
                    request('conference_audio', trackId=track, generation=generation, sequence=sequence[track], sampleOffset=slot * 320, value=value)
                    sequence[track] += 1
            pump_stop.wait(max(0, origin + (slot + 1) * 0.02 - time.monotonic()))
    except BaseException as e:
        if not pump_stop.is_set():
            pump_error.append(str(e))

def record(proc):
    while True:
        raw = proc.stdout.read(640)
        if not raw:
            return
        values = array.array('h')
        values.frombytes(raw[:len(raw) // 2 * 2])
        with samples_lock:
            audio_samples.extend(values)
            if len(audio_samples) > 16000 * 20:
                del audio_samples[:len(audio_samples) - 16000 * 20]
try:
    pulse = subprocess.Popen(['pulseaudio', '-n', '--file=' + str(config), '--daemonize=no', '--exit-idle-time=-1', '--use-pid-file=no', '--disable-shm=true'], env=env, stdout=(out / 'pulse.log').open('wb'), stderr=subprocess.STDOUT)
    children.append(pulse)
    until(lambda: (runtime / 'native').exists(), 8)
    capture = subprocess.Popen(['parec', '--device=cli_playback.monitor', '--format=s16le', '--rate=16000', '--channels=1', '--latency-msec=40', '--raw'], env=env, stdout=subprocess.PIPE, stderr=(out / 'capture.log').open('wb'))
    children.append(capture)
    threading.Thread(target=record, args=(capture,), daemon=True).start()
    tone = root / 'microphone.wav'
    with wave.open(str(tone), 'wb') as wav:
        wav.setparams((1, 2, 16000, 0, 'NONE', 'not compressed'))
        wav.writeframes(b''.join((struct.pack('<h', int(5000 * math.sin(2 * math.pi * 220 * i / 16000))) for i in range(16000 * 60))))
    play = subprocess.Popen(['paplay', '--device=cli_capture', str(tone)], env=env, stdout=subprocess.DEVNULL, stderr=(out / 'microphone.log').open('wb'))
    children.append(play)
    fixture = subprocess.Popen([fixture_binary], env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=(out / 'fixture.log').open('w'), text=True, bufsize=1)
    children.append(fixture)
    assert select.select([fixture.stdout], [], [], 12)[0], 'fixture_start_timeout'
    ready = json.loads(fixture.stdout.readline())
    assert ready['event'] == 'ready'
    request('configure_conference', enabled=True)
    client = Client(['calls', 'conference', 'start', 'synthetic-group@chatroom', '--invite', 'synthetic-a'])
    client.expect('Connected.')
    until(lambda: stats()['audioFrames'] > 5)
    sources()
    thread = threading.Thread(target=pump, daemon=True)
    thread.start()
    client.expect('Alice')
    client.expect('Bob')

    def mixed_samples():
        with samples_lock:
            return sum((2600 <= abs(x) <= 3400 for x in audio_samples))
    until(lambda: mixed_samples() > 320, 8)
    near = mixed_samples()
    initial = stats()
    assert initial['nonzeroFrames'] > 0, 'no_microphone_signal'
    mark = len(client.output())
    client.send('s')
    client.expect('Local speaker disabled', mark)
    before = stats()['audioFrames']
    client.send('i missing-synthetic-contact')
    time.sleep(0.6)
    assert stats()['audioFrames'] > before + 10, 'picker_blocked_media'
    client.send('cancel')
    client.send('s')
    client.expect('Local speaker enabled', mark)
    mark = len(client.output())
    client.send('invite synthetic-c')
    until(lambda: stats()['invitations'] == 1)
    client.expect('submitted; waiting for acceptance', mark)
    mark = len(client.output())
    client.send('m')
    client.expect('Microphone muted.', mark)
    time.sleep(0.2)
    assert stats()['lastTxPeak'] == 0, 'mute_not_silent'
    mark = len(client.output())
    client.send('m')
    client.expect('Microphone unmuted.', mark)
    until(lambda: stats()['lastTxPeak'] > 100)
    mark = len(client.output())
    client.send('d')
    client.expect('Use d input NUMBER', mark)
    before = stats()['audioFrames']
    mark = len(client.output())
    client.send('d default')
    client.expect('Local audio devices changed.', mark)
    assert stats()['audioFrames'] > before, 'device_dialog_blocked_media'
    stream_state['a'] = False
    mark = len(client.output())
    sources(mic_a=False)
    client.expect('mic off', mark)
    stream_state['a'] = True
    sources(mic_a=True)
    stream_state['b'] = False
    mark = len(client.output())
    sources(b=False)
    client.expect('left', mark)
    stream_state['generation_b'] = 2
    sources(b=True, generation_b=2)
    stream_state['b'] = True
    time.sleep(0.4)
    assert not pump_error, pump_error
    pump_stop.set()
    thread.join(timeout=3)
    client.send('l')
    client.finish()
    until(lambda: not stats()['active'])
    request('configure_conference', enabled=True)
    join = Client(['calls', 'conference', 'join', 'synthetic-group@chatroom'])
    join.expect('Connected.')
    join.send('h')
    join.finish()
    until(lambda: not stats()['active'])
    monitor = Client(['calls', 'monitor'])
    monitor.expect('Waiting for calls.')
    request('offer', callerId='synthetic-direct')
    monitor.expect('Incoming voice call')
    monitor.send('a')
    monitor.expect('Connected.')
    before = stats()['audioFrames']
    until(lambda: stats()['audioFrames'] > before + 10)
    mark = len(monitor.output())
    monitor.send('l')
    monitor.expect('Waiting for calls.', mark)
    until(lambda: not stats()['active'])
    monitor.process.send_signal(signal.SIGINT)
    monitor.finish()
    final = stats()
    request('shutdown')
    fixture.wait(timeout=8)
    proof = {'passed': True, 'actual_cli_binary': True, 'actual_rust_api': True, 'pulse_null_devices': True, 'wechat_connection': False, 'mixed_3000_sample_count': near, 'participant_meters': ['Alice', 'Bob'], 'outgoing_conference': True, 'current_invitation_join': True, 'late_invite': True, 'mic_mute_unmute': True, 'speaker_toggle': True, 'contact_picker_media_progress': True, 'device_change_media_progress': True, 'remote_mute': True, 'track_leave_rejoin': True, 'monitor_direct_call_and_return': True, 'stats': final}
    (out / 'result.json').write_text(json.dumps(proof, indent=2) + '\n')
    print(json.dumps(proof))
except BaseException as error:
    with samples_lock:
        (out / 'audio-stats.json').write_text(json.dumps({'samples': len(audio_samples), 'peak': max(map(abs, audio_samples), default=0), 'mixedSamples': sum((2600 <= abs(x) <= 3400 for x in audio_samples))}))
    for index, client in enumerate(clients):
        try:
            (out / f'failure-client-{index}.log').write_text(client.output())
        except BaseException:
            pass
    (out / 'failure.txt').write_text(str(error))
    print('CLI_AUDIO_TEST_FAILED:' + str(error))
    for name in ['fixture.log', 'capture.log', 'microphone.log']:
        p = out / name
        if p.exists():
            print(name + ': ' + p.read_text(errors='replace')[-2000:])
    for client in clients:
        print('Synthetic CLI: ' + client.output()[-4000:])
    raise
finally:
    pump_stop.set()
    for client in clients:
        if client.process.poll() is None:
            client.process.terminate()
    for child in reversed(children):
        if child.poll() is None:
            child.terminate()
    for child in reversed(children):
        try:
            child.wait(timeout=3)
        except subprocess.TimeoutExpired:
            child.kill()
    temporary.cleanup()
