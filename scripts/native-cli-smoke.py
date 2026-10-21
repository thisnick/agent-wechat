#!/usr/bin/env python3
"""Exercise the actual CLI/daemon using fresh temporary data and no native login."""
import json, os, pathlib, signal, socket, subprocess, sys, tempfile, time
binary = str(pathlib.Path(sys.argv[1]).resolve())
with tempfile.TemporaryDirectory(prefix="native-cli-smoke-") as temporary:
    root=pathlib.Path(temporary)
    sock=socket.socket(); sock.bind(("127.0.0.1",0)); port=sock.getsockname()[1]; sock.close()
    config=root/"client.toml"; data=root/"data"; token=root/"token"
    env={k:v for k,v in os.environ.items() if not k.startswith("AGENT_WECHAT_")}
    env.update(HOME=str(root),XDG_CONFIG_HOME=str(root/"config"))
    # Invalid/incomplete credential pipes must fail before initializing a store.
    pipe_data=root/"invalid-pipe-data"
    pipe_args=[binary,"--json","--data-dir",str(pipe_data),"serve","--token-stdin"]
    for supplied in [b"",b"short",b"x"*1025,b"x"*40+b"\nsecond-line",b"\xff"*40]:
        rejected=subprocess.run(pipe_args,input=supplied,env=env,capture_output=True,timeout=10)
        assert rejected.returncode==2,(rejected.returncode,rejected.stderr)
        assert not pipe_data.exists(),"invalid token initialized native data"
        assert b"second-line" not in rejected.stderr
    blocked=subprocess.Popen(pipe_args,stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=subprocess.PIPE,env=env)
    try:
        blocked.wait(timeout=10)
        assert blocked.returncode==2 and b"timed out" in blocked.stderr.read()
        assert not pipe_data.exists(),"incomplete token initialized native data"
    finally:
        if blocked.poll() is None: blocked.kill(); blocked.wait()
        blocked.stdin.close()
    conflict=root/"conflicting-server.toml"
    conflict.write_text("schemaVersion=1\ntokenFile='missing-secret'\n")
    rejected=subprocess.run(pipe_args+["--server-config",str(conflict)],input=b"fixture-"*8,env=env,capture_output=True,timeout=10)
    assert rejected.returncode==2 and b"cannot be combined" in rejected.stderr
    assert not pipe_data.exists()
    token.write_text("synthetic-cli-token-"*3); token.chmod(0o600)
    base=[binary,"--json","--config",str(config),"--data-dir",str(data),"--token-file",str(token),"--listen",f"127.0.0.1:{port}"]
    owned=False
    def run(*args,code=0):
        result=subprocess.run(base+list(args),env=env,capture_output=True,text=True,timeout=30)
        assert result.returncode==code,(args,result.returncode,result.stdout,result.stderr)
        output=result.stdout.strip() or result.stderr.strip()
        return json.loads(output) if output else None
    try:
        assert run("daemon","status")["state"]=="stopped"
        first=run("daemon","start"); owned=True
        assert first["version"]["backend"]=="native"
        assert run("daemon","start")["pid"]==first["pid"]
        assert run("daemon","status")["ownershipGeneration"]==first["ownershipGeneration"]
        run("config","server","add","local","--url",f"http://127.0.0.1:{port}","--token-file",str(token))
        run("config","server","use","local")
        assert run("auth","status",code=2)["error"]["code"]=="account_not_selected"
        a=run("accounts","create","a","--alias","alpha"); assert a["id"]=="a"
        assert run("accounts","get")["id"]=="a"
        run("accounts","create","b")
        assert run("auth","status",code=2)["error"]["code"]=="account_ambiguous"
        run("accounts","default","alpha")
        assert run("account","status")["id"]=="a"
        run("accounts","alias","a","renamed")
        assert run("--account","renamed","account","status")["id"]=="a"
        checkpoint=root/"cursor.json"
        watcher=subprocess.Popen(base+["events","watch","--cursor-file",str(checkpoint)],env=env,stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True)
        try:
            time.sleep(.3); run("account","disconnect")
            deadline=time.monotonic()+5
            while not checkpoint.exists() and time.monotonic()<deadline: time.sleep(.05)
            assert checkpoint.exists(),"actual daemon events were not emitted/checkpointed"
            saved=json.loads(checkpoint.read_text()); assert saved["accountId"]=="a" and saved["progress"]=="emission"
            watcher.send_signal(signal.SIGINT); output,error=watcher.communicate(timeout=5)
            assert watcher.returncode==130,(output,error,watcher.returncode)
            frames=[json.loads(line) for line in output.splitlines()]
            assert frames and all(frame["accountId"]=="a" and isinstance(frame["cursor"],str) for frame in frames)
        finally:
            if watcher.poll() is None: watcher.kill(); watcher.wait()
        assert run("daemon","stop")["logout"] is False; owned=False
        second=run("daemon","start"); owned=True
        assert second["serverId"]==first["serverId"] and second["ownershipGeneration"]!=first["ownershipGeneration"]
        assert len(run("accounts","list")["items"])==2
        run("daemon","stop"); owned=False
        assert run("daemon","status")["state"]=="stopped"
        assert config.stat().st_mode&0o777==0o600 and checkpoint.stat().st_mode&0o777==0o600
        print("Native CLI smoke passed: real background start/status/stop/restart, authenticated identity, scoped server/account defaults, aliases, ambiguity errors and NDJSON emission checkpoints with SIGINT. No login, send or native logout.")
    finally:
        if owned:
            subprocess.run(base+["daemon","stop"],env=env,capture_output=True,timeout=30)
