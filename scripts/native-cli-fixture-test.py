#!/usr/bin/env python3
"""Real CLI transport tests against a synthetic HTTP fixture; never calls WeChat."""
import base64, hashlib, json, os, pathlib, signal, subprocess, sys, tempfile, threading, time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import urlsplit
binary=str(pathlib.Path(sys.argv[1]).resolve())
with tempfile.TemporaryDirectory(prefix="native-cli-fixture-") as temporary:
    root=pathlib.Path(temporary);token=root/"token";token.write_text("fixture-token-"*4);token.chmod(0o600)
    config=root/"client.toml"; state={"posts":0,"login_posts":0,"cancelled":0,"mode":"accepted","media_transfers":0,"official_posts":[],"official_first":False,"recall_posts":0,"recall_mode":"succeeded","recall_operations":{},"recall_drop":False,"recall_wrong_post":False,"recall_wrong_get":False}
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
            if path.endswith("/operations/login-fixture"):
                if state.get("lan_mode"):
                    state["lan_polls"]=state.get("lan_polls",0)+1
                    return self.reply({"id":"login-fixture","state":"succeeded" if state["lan_polls"]>=2 else "awaiting_approval","result":{"qrAvailable":False,"qrGeneration":1,"lanChallenge":"waiting_for_phone"}})
                return self.reply({"id":"login-fixture","state":"awaiting_scan","result":{"qrAvailable":True,"qrGeneration":1}})
            if "/operations/recall-" in path:
                operation=state["recall_operations"][path.split("/")[-1]]
                return self.reply({**operation,"id":"wrong-operation"} if state["recall_wrong_get"] else operation)
            if path.endswith("/messages/m"):
                if state.get("recalled_message"):
                    return self.reply({"id":"m","threadId":"filehelper","senderId":"original-author","direction":"outgoing","type":10000,"body":"[Message recalled]","recall":{"observedAt":"2026-10-23T00:00:00Z","replacementText":"Server notice <text> & detail"}})
                return self.reply({"id":"m","threadId":"filehelper"})
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
            if path.endswith("/recall"):
                assert set(body)=={"serverId"} and isinstance(body["serverId"],str)
                key=self.headers.get("Idempotency-Key");assert key
                receipts=[json.loads(p.read_text()) for p in (root/"receipts").glob("*.json")]
                assert any(r["kind"]=="message_recall" and r["idempotencyKey"]==key and r["state"]=="submitting" for r in receipts),"recall receipt was not durable before submission"
                state["recall_posts"]+=1
                operation_id="recall-"+hashlib.sha256(key.encode()).hexdigest()[:16]
                operation=state["recall_operations"].setdefault(operation_id,{"id":operation_id,"accountId":"a","kind":"message_recall","state":state["recall_mode"],"result":{"messageId":path.split("/")[-2],"serverId":body["serverId"],"status":"needs_review" if state["recall_mode"]=="needs_review" else "recalled"}})
                if state["recall_drop"]:
                    state["recall_drop"]=False;self.close_connection=True;return
                reply={**operation,"state":"queued"}
                if state["recall_wrong_post"]:reply["result"]={**operation["result"],"serverId":"24"}
                return self.reply(reply,202)
            if path.endswith("/sends"):
                receipts=list((root/"receipts").glob("*.json"));assert receipts,"receipt was not durable before first request"
                assert any(json.loads(p.read_text())["idempotencyKey"]==self.headers.get("Idempotency-Key") for p in receipts)
                if body.get("kind")=="share":state["share"]=body
                state["posts"]+=1;return self.reply({"id":"send-fixture","state":"queued"},202)
            if path.endswith("/auth/login"):
                assert body["strategy"]=="qr" and body["qrRefresh"]=={"mode":"manual","maxRefreshes":3}
                receipts=list((root/"login-receipts").glob("*.json"));assert receipts,"login receipt was not durable before first request"
                assert any(json.loads(p.read_text())["idempotencyKey"]==self.headers.get("Idempotency-Key") for p in receipts)
                state["login_posts"]+=1
                if state.get("lan_mode"):
                    state["lan_polls"]=0
                    return self.reply({"id":"login-fixture","state":"awaiting_approval","result":{"qrAvailable":False,"qrGeneration":1,"lanChallenge":"waiting_for_phone"}},202)
                return self.reply({"id":"login-fixture","state":"awaiting_scan","result":{"qrAvailable":True,"qrGeneration":1}},202)
            self.reply({"error":{"code":"fixture_not_found"}},404)
        def do_DELETE(self):
            self.authenticated();assert self.path.endswith("/operations/login-fixture");state["cancelled"]+=1;self.reply({"id":"login-fixture","state":"cancelled","result":{}})
    server=ThreadingHTTPServer(("127.0.0.1",0),Fixture);threading.Thread(target=server.serve_forever,daemon=True).start()
    env={k:v for k,v in os.environ.items() if not k.startswith("AGENT_WECHAT_")};env.update(HOME=str(root),XDG_CONFIG_HOME=str(root/"config"))
    args=[binary,"--json","--config",str(config),"--server",f"http://127.0.0.1:{server.server_port}","--token-file",str(token)]
    def run(*command,code=0):
        result=subprocess.run(args+list(command),env=env,capture_output=True,text=True,timeout=10);assert result.returncode==code,(command,result.returncode,result.stdout,result.stderr)
        structured=result.stdout or next((line for line in reversed(result.stderr.splitlines()) if line.startswith("{")), "")
        return json.loads(structured)
    try:
        run("config","server","add","fixture","--url",f"http://127.0.0.1:{server.server_port}","--token-file",str(token))
        for group in ["chats", "messages"]:
            result=run(group,"read","filehelper","--expected-version","fixture-version")
            assert result["readScope"]=="thread" and result["phoneNotification"]["state"]=="accepted"
            retired=subprocess.run(args+[group,"read","filehelper","--expected-version","fixture-version","--message-id","m"],env=env,capture_output=True,timeout=10)
            assert retired.returncode==2,"retired exact-read flag must not become a whole-thread read"
        result=run("messages","send","filehelper","--text","synthetic-text","--idempotency-key","fixture-send")
        assert result["state"]=="accepted" and state["posts"]==1
        receipt=json.loads(pathlib.Path(result["receiptPath"]).read_text());assert receipt["threadId"]=="filehelper" and "synthetic-text" not in json.dumps(receipt)
        run("messages","send","filehelper","--text","synthetic-text","--idempotency-key","fixture-send");assert state["posts"]==1
        run("messages","send","filehelper","--text","different","--idempotency-key","fixture-send",code=2);assert state["posts"]==1
        state["mode"]="needs_review";run("messages","send","filehelper","--text","uncertain","--idempotency-key","fixture-uncertain",code=8);assert state["posts"]==2
        state["mode"]="accepted";before_share=state["posts"]
        share=["messages","send","filehelper","--share","https://example.invalid/article?a=1&b=2","--title","Shared & title","--description","Explicit description","--thumbnail-url","https://example.invalid/preview.jpg","--idempotency-key","fixture-share"]
        result=run(*share);assert result["state"]=="accepted" and state["posts"]==before_share+1
        assert state["share"]=={"kind":"share","threadId":"filehelper","title":"Shared & title","url":"https://example.invalid/article?a=1&b=2","description":"Explicit description","thumbnailUrl":"https://example.invalid/preview.jpg"}
        receipt=json.loads(pathlib.Path(result["receiptPath"]).read_text());assert receipt["kind"]=="share" and "Shared & title" not in json.dumps(receipt) and "article?a=1" not in json.dumps(receipt)
        run(*share);assert state["posts"]==before_share+1
        changed=share.copy();changed[changed.index("Shared & title")]="Changed title"
        run(*changed,code=2);assert state["posts"]==before_share+1
        missing_title=subprocess.run(args+["messages","send","filehelper","--share","https://example.invalid/article"],env=env,capture_output=True,timeout=10)
        assert missing_title.returncode==2 and state["posts"]==before_share+1
        recalled=run("messages","recall","m","--server-id","18446744073709551615","--idempotency-key","fixture-recall")
        assert recalled["state"]=="succeeded" and state["recall_posts"]==1
        recall_receipt=json.loads(pathlib.Path(recalled["receiptPath"]).read_text());assert recall_receipt["operationId"]==recalled["id"] and recall_receipt["kind"]=="message_recall" and recall_receipt["messageId"]=="m" and recall_receipt["serverId"]=="18446744073709551615" and "threadId" not in recall_receipt
        run("messages","recall","m","--server-id","18446744073709551615","--idempotency-key","fixture-recall");assert state["recall_posts"]==1
        run("messages","recall","m","--server-id","9007199254740993","--idempotency-key","fixture-recall",code=2);assert state["recall_posts"]==1
        for server_id in ["0","01","18446744073709551616"]:
            run("messages","recall","m","--server-id",server_id,code=2)
        assert state["recall_posts"]==1
        missing_server=subprocess.run(args+["messages","recall","m"],env=env,capture_output=True,timeout=10);assert missing_server.returncode==2 and state["recall_posts"]==1
        state["recall_mode"]="needs_review"
        run("messages","recall","m2","--server-id","9007199254740993","--idempotency-key","fixture-recall-uncertain",code=8);assert state["recall_posts"]==2
        run("messages","recall","m2","--server-id","9007199254740993","--idempotency-key","fixture-recall-uncertain",code=8);assert state["recall_posts"]==2
        state["recall_mode"]="succeeded";state["recall_drop"]=True
        run("messages","recall","m3","--server-id","23","--idempotency-key","fixture-recall-lost",code=3);assert state["recall_posts"]==3,"lost response must not cause automatic resubmission"
        run("messages","recall","m3","--server-id","23","--idempotency-key","fixture-recall-lost");assert state["recall_posts"]==4
        state["recall_wrong_post"]=True
        run("messages","recall","m4","--server-id","23","--idempotency-key","fixture-recall-wrong-target",code=8);assert state["recall_posts"]==5
        state["recall_wrong_post"]=False;state["recall_wrong_get"]=True
        run("messages","recall","m5","--server-id","23","--idempotency-key","fixture-recall-wrong-poll",code=8);assert state["recall_posts"]==6
        run("messages","recall","m","--server-id","18446744073709551615","--idempotency-key","fixture-recall",code=8);assert state["recall_posts"]==6
        state["recall_wrong_get"]=False
        image=root/"image";result=run("messages","media","filehelper","m","--quality","best","--output",str(image));assert result["actualVariant"]=="original" and image.read_bytes()==content["original"]
        assert run("messages","media","filehelper","m","--quality","standard","--cached-only",code=9)["state"]=="not_cached"
        state["recalled_message"]=True
        message=run("messages","get","m")
        assert message["senderId"]=="original-author" and message["direction"]=="outgoing"
        assert message["body"]=="[Message recalled]" and message["recall"]["replacementText"]=="Server notice <text> & detail"
        state["recalled_message"]=False
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
        state["lan_mode"]=True
        human_args=[arg for arg in args if arg!="--json"]
        lan=subprocess.run(human_args+["auth","login","--strategy","qr","--qr-refresh","manual","--idempotency-key","fixture-lan-human"],env=env,capture_output=True,text=True,timeout=10)
        assert lan.returncode==0,(lan.stdout,lan.stderr)
        assert lan.stderr.count("WeChat requires a local network check.")==1,lan.stderr
        assert "synthetic-token" not in lan.stdout+lan.stderr
        quiet=subprocess.run(args+["auth","login","--strategy","qr","--qr-refresh","manual","--idempotency-key","fixture-lan-json"],env=env,capture_output=True,text=True,timeout=10)
        assert quiet.returncode==0 and json.loads(quiet.stdout)["state"]=="succeeded",(quiet.stdout,quiet.stderr)
        assert "WeChat requires a local network check." not in quiet.stderr
        state["lan_mode"]=False
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
        print("Native CLI fixture passed: pre-request send/recall/login receipts, same-key lookup/conflict, exact recall IDs, recall uncertainty/lost-response recovery without automatic resubmission, queued acceptance/uncertain exits, best/strict cached quality, attached-login QR/SIGINT cancellation, official-file login discovery/explicit server paths, and token-free offline inspection. No WeChat request.")
    finally:server.shutdown();server.server_close()
