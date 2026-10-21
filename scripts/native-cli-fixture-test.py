#!/usr/bin/env python3
"""Real CLI transport tests against a synthetic HTTP fixture; never calls WeChat."""
import base64, hashlib, json, os, pathlib, signal, subprocess, sys, tempfile, threading, time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import urlsplit
binary=str(pathlib.Path(sys.argv[1]).resolve())
with tempfile.TemporaryDirectory(prefix="native-cli-fixture-") as temporary:
    root=pathlib.Path(temporary);token=root/"token";token.write_text("fixture-token-"*4);token.chmod(0o600)
    config=root/"client.toml"; state={"posts":0,"login_posts":0,"cancelled":0,"mode":"accepted","media_transfers":0,"official_posts":[],"official_first":False}
    content={"thumbnail":b"synthetic-thumbnail","original":b"synthetic-original"}
    def asset(name):return {"id":name,"state":"available","variant":name,"actualVariant":name,"sha256":hashlib.sha256(content[name]).hexdigest(),"byteLength":len(content[name])}
    class Fixture(BaseHTTPRequestHandler):
        def log_message(self,*args):pass
        def reply(self,value,status=200,kind="application/json"):
            body=value if isinstance(value,bytes) else json.dumps(value).encode();self.send_response(status);self.send_header("Content-Type",kind);self.send_header("Content-Length",str(len(body)));self.end_headers();self.wfile.write(body)
        def authenticated(self):assert self.headers.get("Authorization")==f"Bearer {token.read_text()}"
        def do_GET(self):
            self.authenticated();path=urlsplit(self.path).path
            if path=="/v1/accounts":
                assert not state["official_first"],"catalog queried before official preparation"
                return self.reply({"items":[{"id":"a"}]})
            if path.endswith("/sends/by-idempotency-key") or path.endswith("/sends/send-fixture"):return self.reply({"id":"send-fixture","state":state["mode"]})
            if path.endswith("/operations/login-fixture/qr"):
                assert "generation=1" in self.path
                return self.reply(base64.b64decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+a2ioAAAAASUVORK5CYII="),kind="image/png")
            if path.endswith("/operations/login-fixture"):return self.reply({"id":"login-fixture","state":"awaiting_scan","result":{"qrAvailable":True,"qrGeneration":1}})
            if path.endswith("/messages/m"):return self.reply({"id":"m","threadId":"filehelper"})
            if path.endswith("/messages/m/media"):return self.reply({"items":[asset("thumbnail"),asset("original")]})
            if "/media/" in path and path.endswith("/content"):return self.reply(content[path.split("/")[-2]],kind="image/png")
            self.reply({"error":{"code":"fixture_not_found"}},404)
        def do_POST(self):
            self.authenticated();body=json.loads(self.rfile.read(int(self.headers.get("Content-Length","0"))) or b"{}");path=urlsplit(self.path).path
            if path=="/v1/profiles/official":
                state["official_posts"].append(body);state["official_first"]=False
                return self.reply({"accountId":"a","profilePath":"/daemon-home","storageMode":"official","savedDeviceLoginAvailable":False})
            if path.endswith("/read-state"):
                assert body == {"expectedVersion":"fixture-version"}
                return self.reply({"readScope":"thread","phoneNotification":{"state":"accepted"}})
            if path.endswith("/sends"):
                receipts=list((root/"receipts").glob("*.json"));assert receipts,"receipt was not durable before first request"
                assert any(json.loads(p.read_text())["idempotencyKey"]==self.headers.get("Idempotency-Key") for p in receipts)
                state["posts"]+=1;return self.reply({"id":"send-fixture","state":"queued"},202)
            if path.endswith("/auth/login"):
                assert body["strategy"]=="qr" and body["qrRefresh"]=={"mode":"manual","maxRefreshes":3}
                receipts=list((root/"login-receipts").glob("*.json"));assert receipts,"login receipt was not durable before first request"
                assert any(json.loads(p.read_text())["idempotencyKey"]==self.headers.get("Idempotency-Key") for p in receipts)
                state["login_posts"]+=1
                return self.reply({"id":"login-fixture","state":"awaiting_scan","result":{"qrAvailable":True,"qrGeneration":1}},202)
            self.reply({"error":{"code":"fixture_not_found"}},404)
        def do_DELETE(self):
            self.authenticated();assert self.path.endswith("/operations/login-fixture");state["cancelled"]+=1;self.reply({"id":"login-fixture","state":"cancelled","result":{}})
    server=ThreadingHTTPServer(("127.0.0.1",0),Fixture);threading.Thread(target=server.serve_forever,daemon=True).start()
    env={k:v for k,v in os.environ.items() if not k.startswith("AGENT_WECHAT_")};env.update(HOME=str(root),XDG_CONFIG_HOME=str(root/"config"))
    args=[binary,"--json","--config",str(config),"--server",f"http://127.0.0.1:{server.server_port}","--token-file",str(token)]
    def run(*command,code=0):
        result=subprocess.run(args+list(command),env=env,capture_output=True,text=True,timeout=10);assert result.returncode==code,(command,result.returncode,result.stdout,result.stderr);return json.loads(result.stdout or result.stderr)
    try:
        for group in ["chats", "messages"]:
            result=run(group,"read","filehelper","--expected-version","fixture-version")
            assert result["readScope"]=="thread" and result["phoneNotification"]["state"]=="accepted"
            retired=subprocess.run(args+[group,"read","filehelper","--expected-version","fixture-version","--message-id","m"],env=env,capture_output=True,timeout=10)
            assert retired.returncode==2,"retired exact-read flag must not become a whole-thread read"
        run("config","server","add","fixture","--url",f"http://127.0.0.1:{server.server_port}","--token-file",str(token))
        result=run("messages","send","filehelper","--text","synthetic-text","--idempotency-key","fixture-send")
        assert result["state"]=="accepted" and state["posts"]==1
        receipt=json.loads(pathlib.Path(result["receiptPath"]).read_text());assert receipt["threadId"]=="filehelper" and "synthetic-text" not in json.dumps(receipt)
        run("messages","send","filehelper","--text","synthetic-text","--idempotency-key","fixture-send");assert state["posts"]==1
        run("messages","send","filehelper","--text","different","--idempotency-key","fixture-send",code=2);assert state["posts"]==1
        state["mode"]="needs_review";run("messages","send","filehelper","--text","uncertain","--idempotency-key","fixture-uncertain",code=8);assert state["posts"]==2
        image=root/"image";result=run("messages","media","filehelper","m","--quality","best","--output",str(image));assert result["actualVariant"]=="original" and image.read_bytes()==content["original"]
        assert run("messages","media","filehelper","m","--quality","standard","--cached-only",code=9)["state"]=="not_cached"
        result=run("auth","login","--strategy","qr","--qr-refresh","manual","--detach","--idempotency-key","fixture-login")
        assert state["login_posts"]==1 and pathlib.Path(result["receiptPath"]).is_file()
        run("auth","login","--strategy","qr","--qr-refresh","manual","--detach","--idempotency-key","fixture-login");assert state["login_posts"]==1
        run("auth","login","--strategy","saved-device","--qr-refresh","manual","--detach","--idempotency-key","fixture-login",code=2);assert state["login_posts"]==1
        login=subprocess.Popen(args+["auth","login","--strategy","qr","--qr-refresh","manual"],env=env,stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True)
        try:
            deadline=time.monotonic()+5
            while not list((root/"qr").glob("*.png")) and time.monotonic()<deadline: time.sleep(.02)
            assert list((root/"qr").glob("*.png")),"login QR was not written"
            login.send_signal(signal.SIGINT);output,error=login.communicate(timeout=5);assert login.returncode==130,(output,error,login.returncode)
            result=json.loads(output);assert result["id"]=="login-fixture" and result["cancellationConfirmed"] and state["cancelled"]==1
        finally:
            if login.poll() is None: login.kill();login.wait()
        for flag, path in [("--official", None), ("--official=/daemon/profile", "/daemon/profile")]:
            state["official_first"]=True
            result=run("auth","login",flag,"--strategy","qr","--qr-refresh","manual","--detach","--idempotency-key", "fixture-official-"+str(len(state["official_posts"])))
            assert json.loads(pathlib.Path(result["receiptPath"]).read_text())["accountId"]=="a"
            assert state["official_posts"][-1]=={"path":path,"accountId":None}
        stopped=root/"stopped-home";storage=stopped/"xwechat_files"/"synthetic_account"/"db_storage";storage.mkdir(parents=True);(storage/"contact.db").write_bytes(b"synthetic-inventory-only")
        run("profile","add","archive","--path",str(stopped),"--account-dir","synthetic_account","--keys-file",str(root/"not-yet-exported.keys.json"))
        renamed=token.with_name("token-offline");token.rename(renamed)
        try:
            inspected=run("--local-profile","archive","profile","inspect");assert inspected["keysState"]=="keys_missing" and inspected["databases"][0]["relativePath"]=="contact.db"
        finally:renamed.rename(token)
        print("Native CLI fixture passed: pre-request send/login receipts, same-key lookup/conflict, queued acceptance/uncertain exits, best/strict cached quality, attached-login QR/SIGINT cancellation, official-file login discovery/explicit server paths, and token-free offline inspection. No WeChat request.")
    finally:server.shutdown();server.server_close()
