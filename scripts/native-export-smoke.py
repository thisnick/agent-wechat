#!/usr/bin/env python3
"""Actual CLI copy guards using synthetic files/sidecars; no native login/network."""
import hashlib
import json
import os
import pathlib
import select
import signal
import sqlite3
import subprocess
import sys
import tempfile

binary = str(pathlib.Path(sys.argv[1]).resolve())
with tempfile.TemporaryDirectory(prefix="native-export-smoke-") as temporary:
    root = pathlib.Path(temporary)
    source = root / "source"
    storage = source / "xwechat_files" / "synthetic_account" / "db_storage"
    storage.mkdir(parents=True)
    (storage / "contact.db").write_bytes(b"synthetic-opaque-main")
    (storage / "contact.db-wal").write_bytes(b"synthetic-opaque-wal")
    (source / "saved-login-material").write_bytes(b"synthetic-private-official-material")
    env = {k: v for k, v in os.environ.items() if not k.startswith("AGENT_WECHAT_")}
    env.update(HOME=str(root), XDG_CONFIG_HOME=str(root / "config"))
    data = root / "data"
    base = [binary, "--json", "--data-dir", str(data), "--account", "test"]

    def tree(path):
        return {str(p.relative_to(path)): hashlib.sha256(p.read_bytes()).hexdigest()
                for p in path.rglob("*") if p.is_file()}

    original = tree(source)

    def run(args, code=0):
        result = subprocess.run(args, env=env, capture_output=True, text=True, timeout=15)
        assert result.returncode == code, (args, result.returncode, result.stdout, result.stderr)
        return json.loads(result.stdout or result.stderr)

    def export(name, *options, code=0):
        destination = root / name
        result = run(base + ["store", "export", "--destination", str(destination), *options], code)
        if code:
            assert not destination.exists(), "refused export published a destination"
        else:
            assert tree(destination) == original, "copy changed official bytes or included sidecars"
        assert tree(source) == original, "copy modified its retained source"
        return result

    run(base + ["store", "attach", "--path", str(source), "--source-stopped"])
    initial = export("initial", "--source-stopped")
    assert not initial["syncCheckpointProjected"] and initial["syncCheckpointEvidence"] == "unverified"
    assert not initial["daemonSidecarIncluded"] and not initial["daemonActiveCredentialsIncluded"]
    assert initial["officialLoginMaterialMayBeIncluded"] and not initial["incompleteCopy"]
    export("missing-stop-assertion", code=2)
    control = data / "accounts" / "test" / "agent" / "control.sqlite"

    def seed(statements):
        with sqlite3.connect(control) as db:
            for table in ["runtime", "apply_intents", "outbox", "media_jobs", "media_assets", "recall_keys", "outgoing_recalls", "message_recalls", "operations"]:
                db.execute(f"DELETE FROM {table}")
            for statement, parameters in statements:
                db.execute(statement, parameters)

    put = "INSERT INTO runtime VALUES(?,?)"
    recall_marker = ("INSERT INTO message_recalls(thread_id,server_id,observed_at,origin,projection_state) VALUES(?,?,?,?,?)", ("peer", "18446744073709551615", "fixture", "live", "pending"))
    def outgoing_recall(phase):
        return [
            ("INSERT INTO operations VALUES(?,?,?,?,?,?)", ("recall", "message_recall", "needs_review" if phase == "needs_review" else "running", "{}", "fixture", "fixture")),
            ("INSERT INTO outgoing_recalls VALUES(?,?,?,?,?)", ("recall", "a" * 64, "b" * 64, phase, b"sealed-request")),
        ]
    cases = [
        ("sync", "pendingProjectionBatches", [("INSERT INTO apply_intents(id,payload,state,created_at) VALUES(?,?,?,?)", ("sync", b"sealed", "pending", "fixture"))]),
        ("contact-sync", "pendingProjectionBatches", [("INSERT INTO apply_intents(id,payload,state,created_at,stream) VALUES(?,?,?,?,?)", ("contact", b"sealed", "pending", "fixture", "contact"))]),
        ("read", "pendingReadProjections", [(put, ("read-pending:thread", b"{}"))]),
        ("recall-projection", "pendingRecallProjections", [recall_marker]),
        *[("recall-" + phase, "pendingMessageRecalls", outgoing_recall(phase)) for phase in ["queued", "submitting", "accepted", "needs_review"]],
        ("cursor-needed", "pendingCursorProjection", [(put, ("cursor-projection-needed", b"1"))]),
        ("cursor-publication", "pendingCursorProjection", [(put, ("cursor-publication", b"sealed"))]),
        ("contact-cursor-publication", "pendingCursorProjection", [(put, ("contact-cursor-publication", b"sealed"))]),
        ("contact-cursor-unpublished", "pendingCursorProjection", [(put, ("contact-received-cursor", b"sealed"))]),
        ("contact-cursor-stale-receipt", "pendingCursorProjection", [(put, ("contact-received-cursor", b"sealed")), (put, ("contact-projected-cursor", b"prior")), (put, ("contact-projected-received-generation", b"a" * 64))]),
        ("saved-login-needed", "pendingSavedLoginProjection", [(put, ("saved-login-projection-needed", b"1"))]),
        ("saved-login-publication", "pendingSavedLoginProjection", [(put, ("saved-login-publication", b"sealed"))]),
        ("saved-login-state-mismatch", "pendingSavedLoginProjection", [(put, ("account-state", b"sealed-state")), (put, ("saved-login-accepted-state-generation", hashlib.sha256(b"sealed-state").hexdigest().encode())), (put, ("saved-login-projected-state-generation", b"b" * 64))]),
        ("saved-login-accepted-mismatch", "pendingSavedLoginProjection", [(put, ("account-state", b"new-sealed-state")), (put, ("saved-login-accepted-state-generation", b"a" * 64)), (put, ("saved-login-projected-state-generation", b"a" * 64))]),
        ("legacy-unpublished-cursor", "pendingCursorProjection", [(put, ("received-cursor", b"sealed"))]),
        ("metadata", "pendingMetadataProjections", [(put, ("metadata-receipt:fixture", b"sealed")), (put, ("metadata-pending:fixture", b"1"))]),
        ("accepted-text", "pendingTextProjections", [("INSERT INTO outbox VALUES(?,?,?,?,?,?,?,?,?,?)", ("text", "key", "hash", "filehelper", b"sealed", "accepted", "1", "18446744073709551615", None, "fixture"))]),
        ("accepted-media", "pendingMediaProjections", [("INSERT INTO media_jobs(id,idempotency_key,request_hash,kind,state,phase,data_json,payload,created_at,updated_at) VALUES(?,?,?,?,?,?,?,?,?,?)", ("media", "media-key", "hash", "send", "accepted", "accepted", '{"result":{"profileProjection":"pending"}}', b"sealed", "fixture", "fixture"))]),
        ("media-history-complete-cache-partial", "incompleteMediaWriteback", [("INSERT INTO media_jobs(id,idempotency_key,request_hash,kind,state,phase,data_json,payload,created_at,updated_at) VALUES(?,?,?,?,?,?,?,?,?,?)", ("media", "media-key", "hash", "send", "accepted", "completed", '{"result":{"profileProjection":"complete","officialMediaWriteback":"partial"}}', b"sealed", "fixture", "fixture"))]),
        ("available-cache-partial", "incompleteMediaWriteback", [("INSERT INTO media_assets VALUES(?,?,?,?,?,?,?,?)", ("asset", "message", "original", "file", '{"state":"available","officialWriteback":"partial"}', b"sealed-plan", b"sealed-bytes", "fixture"))]),
        ("available-cache-legacy", "incompleteMediaWriteback", [("INSERT INTO media_assets VALUES(?,?,?,?,?,?,?,?)", ("asset", "message", "original", "file", '{"state":"available"}', b"sealed-plan", b"sealed-bytes", "fixture"))]),
        ("partial-voice-cache", "incompleteMediaWriteback", [("INSERT INTO media_jobs(id,idempotency_key,request_hash,kind,state,phase,data_json,payload,created_at,updated_at) VALUES(?,?,?,?,?,?,?,?,?,?)", ("voice", "voice-key", "hash", "send", "failed", "completed", '{"result":{"profileProjection":"complete","officialMediaWriteback":"partial","chunks":[{"state":"accepted"},{"state":"failed"}]}}', b"sealed", "fixture", "fixture"))]),
    ]
    for name, field, statements in cases:
        seed(statements)
        error = export(f"refused-{name}", "--source-stopped", code=9)
        assert error["error"]["code"] == "profile_projection_pending"
        copied = export(f"partial-{name}", "--source-stopped", "--allow-incomplete")
        assert copied[field] and copied["incompleteCopy"]
        if name in ["media-history-complete-cache-partial", "available-cache-partial", "available-cache-legacy", "partial-voice-cache"]:
            assert not copied["pendingMediaProjections"], "cache incompleteness must stay distinct from history projection"
        if name.startswith("cursor") or name == "legacy-unpublished-cursor" or name == "sync":
            assert not copied["syncCheckpointProjected"]

    receipt = [(put, ("received-cursor", b"sealed-received")), (put, ("projected-cursor", b"sealed-projected")), (put, ("projected-cursor-hash", b"a" * 64)), (put, ("projected-received-generation", hashlib.sha256(b"sealed-received").hexdigest().encode()))]
    seed(receipt)
    completed = export("recorded-checkpoint", "--source-stopped")
    assert completed["syncCheckpointProjected"] and completed["syncCheckpointEvidence"] == "durable_projection_receipt"
    contact_receipt = [(put, ("contact-received-cursor", b"sealed-contact")), (put, ("contact-projected-cursor", b"projected-contact")), (put, ("contact-projected-received-generation", hashlib.sha256(b"sealed-contact").hexdigest().encode()))]
    seed(receipt + contact_receipt)
    assert export("recorded-contact-checkpoint", "--source-stopped")["syncCheckpointProjected"]
    for name, extra in [("marker", [recall_marker]), ("uncertain", outgoing_recall("needs_review")), ("accepted", outgoing_recall("accepted"))]:
        seed(receipt + extra)
        export("recall-checkpoint-refused-" + name, "--source-stopped", code=9)
        copied = export("recall-checkpoint-partial-" + name, "--source-stopped", "--allow-incomplete")
        assert not copied["syncCheckpointProjected"] and copied["syncCheckpointEvidence"] == "pending"
    seed(receipt[:-1])
    export("missing-generation", "--source-stopped", code=9)
    seed(receipt[:-1] + [(put, ("projected-received-generation", b"b" * 64))])
    export("wrong-generation", "--source-stopped", code=9)
    mismatched = export("wrong-generation-partial", "--source-stopped", "--allow-incomplete")
    assert not mismatched["syncCheckpointProjected"] and mismatched["pendingCursorProjection"]
    seed(receipt + [(put, ("cursor-projection-needed", b"1"))])
    export("stale-receipt", "--source-stopped", code=9)
    partial = export("stale-receipt-partial", "--source-stopped", "--allow-incomplete")
    assert not partial["syncCheckpointProjected"] and partial["pendingCursorProjection"]
    seed([("INSERT INTO media_assets VALUES(?,?,?,?,?,?,?,?)", ("asset", "message", "original", "image", '{"state":"available","officialWriteback":"complete"}', b"sealed-plan", b"sealed-bytes", "fixture"))])
    complete_cache = export("complete-cache", "--source-stopped")
    assert not complete_cache["incompleteMediaWriteback"]
    seed([("INSERT INTO media_jobs(id,idempotency_key,request_hash,kind,state,phase,data_json,payload,created_at,updated_at) VALUES(?,?,?,?,?,?,?,?,?,?)", ("failed", "failed-key", "hash", "send", "failed", "completed", '{"result":{"profileProjection":"not_applicable"}}', b"sealed", "fixture", "fixture"))])
    unsent = export("no-accepted-media", "--source-stopped")
    assert not unsent["incompleteMediaWriteback"]
    saved_generation = hashlib.sha256(b"sealed-state").hexdigest().encode()
    seed([(put, ("account-state", b"sealed-state")), (put, ("saved-login-accepted-state-generation", saved_generation)), (put, ("saved-login-projected-state-generation", saved_generation))])
    saved = export("saved-login-complete", "--source-stopped")
    assert not saved["pendingSavedLoginProjection"]
    seed([])

    # Another native owner can bind the same external profile through a separate
    # data root; its profile lease must still exclude this copy operation.
    other = root / "other-data"
    other_base = [binary, "--json", "--data-dir", str(other), "--account", "owner"]
    run(other_base + ["store", "attach", "--path", str(source), "--source-stopped"])
    daemon = subprocess.Popen(other_base + ["--listen", "127.0.0.1:0", "serve"], env=env,
                              stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    try:
        ready, _, _ = select.select([daemon.stderr], [], [], 10)
        assert ready and json.loads(daemon.stderr.readline())["state"] == "ready"
        result = subprocess.run(base + ["store", "export", "--destination", str(root / "owned"), "--source-stopped"],
                                env=env, capture_output=True, text=True, timeout=15)
        assert result.returncode != 0 and "already owned" in result.stderr
        assert not (root / "owned").exists() and tree(source) == original
    finally:
        daemon.send_signal(signal.SIGTERM)
        daemon.wait(timeout=10)
        assert daemon.returncode == 0

    print("Native export smoke passed: stopped-source assertion, cursor/sync/read/metadata/accepted-send guards, explicit partial receipts, recorded-checkpoint evidence, cross-root ownership and unchanged opaque copies. No native request.")
