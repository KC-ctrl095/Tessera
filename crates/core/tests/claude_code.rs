//! Chats run by the user's own Claude Code (provider `claude-code`), against
//! a stand-in `claude` that speaks the same stream-json protocol: turns,
//! approvals for its permission prompts, stopping, continuity, usage, and
//! the command sandbox around it.

mod common;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use common::*;
use serde_json::{Value, json};
use tessera_core::config::Paths;
use tessera_core::{Engine, EngineOptions};
use tessera_proto::*;

/// A stand-in for `claude -p --input-format stream-json …`. It logs its
/// arguments and the messages it gets to `calls.jsonl` beside itself.
const FAKE: &str = r#"#!/usr/bin/env python3
import json, os, sys, time
HERE = os.path.dirname(os.path.abspath(__file__))
args = sys.argv[1:]
def log(kind, value):
    with open(os.path.join(HERE, "calls.jsonl"), "a") as f:
        f.write(json.dumps({"kind": kind, "value": value}) + "\n")
def arg(name):
    return args[args.index(name) + 1] if name in args else None
log("started", args)
# Another Claude account: its own configuration folder (CLAUDE_CONFIG_DIR).
ACCOUNT = os.environ.get("CLAUDE_CONFIG_DIR")
log("account", {"dir": ACCOUNT, "args": args})
if args[:1] == ["--version"]:
    print("9.9.9 (Claude Code)"); sys.exit(0)
if args[:2] == ["auth", "status"]:
    if ACCOUNT:
        # Signed in once the user has (the test leaves the mark Claude Code's login would).
        print(json.dumps({"loggedIn": os.path.exists(os.path.join(ACCOUNT, "logged-in")), "authMethod": "claude.ai",
                          "apiProvider": "firstParty", "email": "work@example.com", "subscriptionType": "pro"}))
        sys.exit(0)
    out = os.path.exists(os.path.join(HERE, "signed_out"))
    key = os.path.exists(os.path.join(HERE, "api_key"))
    print(json.dumps({"loggedIn": not out, "authMethod": "api_key" if key else "claude.ai", "apiProvider": "firstParty",
                      "email": "ann@example.com", "subscriptionType": "max"}))
    sys.exit(0)
log("args", args)
# Only synthetic startup state beside this stand-in is ever read: what the
# user's and the project's settings would load at start (hooks, CLAUDE.md,
# plugins, MCP servers), unless the run skips customizations.
fixture = os.path.join(HERE, "privacy-fixture.json")
if os.path.exists(fixture):
    config = json.load(open(fixture))
    isolated = "--safe-mode" in args and arg("--setting-sources") == "" and "--disable-slash-commands" in args
    if not isolated:
        settings = [json.load(open(path)) for path in config["settings"]]
        for setting in settings:
            for path in setting["hooks"]:
                open(path, "w").write("startup ran")
        log("startup-context", [open(path).read() for path in config["memory"]])
        log("startup-integrations", [item for setting in settings for item in setting["plugins"] + setting["mcp"]])
    else:
        log("startup-context", [])
        log("startup-integrations", [])
log("env", sorted(k for k in os.environ if "TOKEN" in k or "KEY" in k or k.startswith("TESSERA") or k.startswith("CLAUDE_CODE")))
if os.path.exists(os.path.join(HERE, "private-old")) and "--safe-mode" in args:
    # A Claude Code from before the private controls: its parser says so and it ends.
    sys.stderr.write("error: unknown option '--safe-mode'\n"); sys.exit(1)
log("auto_memory", os.environ.get("CLAUDE_CODE_DISABLE_AUTO_MEMORY"))
if os.path.exists(os.path.join(HERE, "old")) and "--replay-user-messages" in args:
    # A Claude Code from before a flag Tessera uses: its parser says so and it ends.
    sys.stderr.write("error: unknown option '--replay-user-messages'\n"); sys.exit(1)
if arg("--output-format") == "json":
    # One answer for Tessera's own small requests (titles…).
    prompt = sys.stdin.read()
    system = open(arg("--system-prompt-file")).read()
    log("oneshot", {"system": system, "prompt": prompt, "model": arg("--model"), "cwd": os.getcwd()})
    text = "Stub Title Here" if "name conversations" in system else "stub answer"
    print(json.dumps({"type": "result", "subtype": "success", "is_error": False, "result": text,
                      "usage": {"input_tokens": 12, "output_tokens": 3},
                      "modelUsage": {"claude-haiku-9": {"inputTokens": 12, "outputTokens": 3, "costUSD": 0.0001}},
                      "total_cost_usd": 0.0001}))
    sys.exit(0)
if arg("--append-system-prompt-file"):
    log("context", open(arg("--append-system-prompt-file")).read())
if "--resume" in args and os.path.exists(os.path.join(HERE, "forget")):
    print(json.dumps({"type": "result", "subtype": "error_during_execution", "is_error": True,
                      "result": "No conversation found with session ID: " + args[args.index("--resume") + 1]}), flush=True)
    sys.exit(1)
# A fresh id per conversation, like Claude Code's (a pid repeats inside the sandbox's own pid namespace).
session = args[args.index("--resume") + 1] if "--resume" in args else "sess-" + os.urandom(4).hex()
bridge = "tessera" in json.loads(arg("--mcp-config") or "{}").get("mcpServers", {})
def emit(o):
    sys.stdout.write(json.dumps(o) + "\n"); sys.stdout.flush()
inbox = []
def read():
    if inbox:
        return inbox.pop(0)
    line = sys.stdin.readline()
    if not line:
        sys.exit(0)
    m = json.loads(line)
    log("in", m)
    return m
def wait_for(pred):
    # What doesn't match waits in the inbox (a user message may arrive
    # while a tool call is out).
    for i, m in enumerate(inbox):
        if pred(m):
            return inbox.pop(i)
    while True:
        line = sys.stdin.readline()
        if not line:
            return None
        m = json.loads(line)
        log("in", m)
        if m.get("type") == "control_request" and m["request"]["subtype"] == "set_permission_mode":
            log("permission-mode", m["request"]["mode"])
            emit({"type": "control_response", "response": {"subtype": "success", "request_id": m["request_id"], "response": {}}})
            if pred(m):
                return m
            continue
        if m.get("type") == "control_request" and m["request"]["subtype"] == "interrupt":
            emit({"type": "control_response", "response": {"subtype": "success", "request_id": m["request_id"], "response": {}}})
            return None
        if pred(m):
            return m
        inbox.append(m)
k = [0]
def mcp(method, params=None, server="tessera"):
    k[0] += 1
    rid = "mcp-%d" % k[0]
    msg = {"jsonrpc": "2.0", "id": k[0], "method": method}
    if params is not None:
        msg["params"] = params
    emit({"type": "control_request", "request_id": rid, "request": {"subtype": "mcp_message", "server_name": server, "message": msg}})
    m = wait_for(lambda m: m.get("type") == "control_response" and m["response"]["request_id"] == rid)
    log("mcp", {"method": method, "reply": m and m["response"]})
    if not m or m["response"]["subtype"] != "success":
        return None
    return m["response"]["response"]["mcp_response"]
n = [0]
tally = [0]
def say(text, tools=(), parent=None):
    n[0] += 1
    mid = "msg_%d" % n[0]
    if parent is None:
        emit({"type": "stream_event", "parent_tool_use_id": None, "event": {"type": "message_start", "message": {"id": mid}}})
        emit({"type": "stream_event", "parent_tool_use_id": None, "event": {"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}})
        emit({"type": "stream_event", "parent_tool_use_id": None, "event": {"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": text}}})
    usage = {"input_tokens": 10, "output_tokens": 5, "cache_read_input_tokens": 100, "cache_creation_input_tokens": 0}
    emit({"type": "assistant", "parent_tool_use_id": parent, "message": {"id": mid, "model": "claude-test-9", "content": [{"type": "text", "text": text}], "usage": usage}})
    for i, (tid, name, inp) in enumerate(tools):
        if parent is None:
            emit({"type": "stream_event", "parent_tool_use_id": None, "event": {"type": "content_block_start", "index": i + 1, "content_block": {"type": "tool_use", "id": tid, "name": name, "input": {}}}})
            emit({"type": "stream_event", "parent_tool_use_id": None, "event": {"type": "content_block_delta", "index": i + 1, "delta": {"type": "input_json_delta", "partial_json": json.dumps(inp)}}})
        emit({"type": "assistant", "parent_tool_use_id": parent, "message": {"id": mid, "model": "claude-test-9", "content": [{"type": "tool_use", "id": tid, "name": name, "input": inp}], "usage": usage}})
hooks = {}
def hook(event, inp, tool_use_id=None, tool=None):
    # As Claude Code calls an SDK host's hook functions: a hook_callback per registered id.
    out = {}
    for mt in hooks.get(event, []):
        if tool is not None and "matcher" in mt and tool not in mt["matcher"].split("|"):
            continue
        for cid in mt["hookCallbackIds"]:
            k[0] += 1
            rid = "hook-%d" % k[0]
            req = {"subtype": "hook_callback", "callback_id": cid, "input": dict(inp, hook_event_name=event, session_id=session, cwd=inp.get("cwd", os.getcwd()))}
            if tool_use_id:
                req["tool_use_id"] = tool_use_id
            emit({"type": "control_request", "request_id": rid, "request": req})
            m = wait_for(lambda m: m.get("type") == "control_response" and m["response"]["request_id"] == rid)
            r = (m or {}).get("response", {}).get("response") or {}
            if not m and event == "PreToolUse":
                r = {"hookSpecificOutput": {"hookEventName": event, "permissionDecision": "deny", "permissionDecisionReason": "The CLI was interrupted."}}
            log("hook", {"event": event, "reply": r, "callback": cid, "tool": tool, "call": tool_use_id})
            out.update(r)
            if r.get("hookSpecificOutput", {}).get("permissionDecision") == "deny":
                return out
    return out
crons = []
def result(text, error=False, subtype="success", finish=True):
    if not error and "Stop" in hooks:
        # The Stop hook may ask it to go on (once: the second time stop_hook_active is set).
        r = hook("Stop", {"stop_hook_active": False, "last_assistant_message": text, "background_tasks": [], "session_crons": crons})
        note = (r.get("hookSpecificOutput") or {}).get("additionalContext") or (r.get("reason") if r.get("decision") == "block" else None)
        log("completion-stop", {"phase":text, "nudged":bool(note)})
        if note:
            log("nudged", note)
            if os.path.exists("completion-ack") and "[Completion check]" in note:
                say("✓")
            else:
                check = {"command": "python3 -m py_compile app.py", "description": "Check the code"}
                say("Checking.", [("toolu_chk", "Bash", check)])
                tool_result("toolu_chk", "")
            log("again", hook("Stop", {"stop_hook_active": not os.path.exists("stop-storm"), "last_assistant_message": "Checked.", "background_tasks": [], "session_crons": crons}))
        if os.path.exists("stop-storm"):
            emit({"type":"system","subtype":"task_notification","task_id":"gone","status":"completed","summary":"background completed"})
            log("storm-child", hook("Stop", {"stop_hook_active":False,"agent_id":"child","last_assistant_message":"child done"}))
            log("storm-parent", hook("Stop", {"stop_hook_active":False,"last_assistant_message":"parent done"}))
    if not finish:
        return
    mu = {"inputTokens": 100, "outputTokens": 20, "cacheReadInputTokens": 1000, "cacheCreationInputTokens": 50, "costUSD": 0.0123, "contextWindow": 200000}
    emit({"type": "result", "subtype": subtype, "is_error": error, "result": text, "session_id": session,
          "total_cost_usd": 0.0123,
          "usage": {"input_tokens": 100, "output_tokens": 20, "cache_read_input_tokens": 1000, "cache_creation_input_tokens": 50},
          "modelUsage": {"claude-test-9": mu}})
def tool_result(tid, text, error=False, extra=None, parent=None):
    m = {"type": "user", "parent_tool_use_id": parent, "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": tid, "content": text, "is_error": error}]}}
    if extra:
        m["tool_use_result"] = extra
    emit(m)
def ask(tid, name, inp):
    emit({"type": "control_request", "request_id": "perm-" + tid, "request": {"subtype": "can_use_tool", "tool_name": name, "input": inp, "tool_use_id": tid}})
    m = wait_for(lambda m: m.get("type") == "control_response" and m["response"]["request_id"] == "perm-" + tid)
    return m and m["response"]["response"]["behavior"] == "allow"
def call(name, inp, tid, parent=None):
    # Public CLI mandatory hook precedes the exact MCP call, including Auto.
    say("Using " + name, [(tid, "mcp__tessera__" + name, inp)], parent)
    metadata = {"agent_id": parent, "agent_type": "Explore"} if parent else {}
    decision = hook("PreToolUse", dict(metadata, tool_name="mcp__tessera__" + name, tool_input=inp, tool_use_id=tid), tid, "mcp__tessera__" + name)
    if (decision.get("hookSpecificOutput") or {}).get("permissionDecision") == "deny":
        text = decision["hookSpecificOutput"].get("permissionDecisionReason", "Not allowed")
        tool_result(tid, text, True, parent=parent)
        log("called", {"name": name, "text": text, "error": True})
        return text, True
    r = mcp("tools/call", {"name": name, "arguments": inp, "_meta": {"claudecode/toolUseId": tid}})
    res = (r or {}).get("result", {})
    text = "".join(c.get("text", "") for c in res.get("content", []) if c.get("type") == "text")
    err = res.get("isError", r is None)
    tool_result(tid, text, err, parent=parent)
    log("called", {"name": name, "text": text, "error": err})
    return text, err
def parse_call(text, word):
    rest = text.split(word, 1)[1].strip()
    name, _, js = rest.partition(" ")
    return name, json.loads(js or "{}")
helper = arg("--tools") == ""
started = False
handled = [0]
while True:
    m = read()
    if m.get("type") == "control_request" and m["request"]["subtype"] == "initialize":
        hooks = m["request"].get("hooks") or {}
        log("hooks", sorted(hooks))
        if os.path.exists("reject-init"):
            emit({"type":"control_response", "response":{"subtype":"error", "request_id":m["request_id"], "error":"hooks refused"}})
            continue
        emit({"type": "control_response", "response": {"subtype": "success", "request_id": m["request_id"], "response": {"models": [
            {"value": "default", "displayName": "Default"},
            {"value": "opus", "displayName": "Opus 9", "supportedEffortLevels": ["low", "high"], "supportsFastMode": True},
            {"value": "haiku", "displayName": "Haiku 9"}], "hooks_applied": True if hooks else None}}})
        if bridge:
            # Claude Code connects its MCP servers at start-up.
            mcp("initialize", {"protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": {"name": "claude-code"}})
            mcp("notifications/initialized")
            listed = mcp("tools/list") or {}
            log("tools", [t["name"] for t in listed.get("result", {}).get("tools", [])])
            log("schemas", {t['name']: t['inputSchema'] for t in listed.get('result', {}).get('tools', [])})
        else:
            log("no-bridge-reply", mcp("tools/list"))
        continue
    if m.get("type") == "control_request" and m["request"]["subtype"] == "set_permission_mode":
        log("permission-mode", m["request"]["mode"])
        emit({"type": "control_response", "response": {"subtype": "success", "request_id": m["request_id"], "response": {}}})
        continue
    if m.get("type") == "control_request" and m["request"]["subtype"] == "get_context_usage":
        if os.path.exists("context-count"):
            counts = json.load(open("context-count"))
            emit({"type":"control_response", "response":{"subtype":"success", "request_id":"unrelated", "response":{"totalTokens":999999,"maxTokens":999999}}})
            emit({"type":"control_response", "response":{"subtype":"success", "request_id":m["request_id"], "response":counts}})
        else:
            emit({"type":"control_response", "response":{"subtype":"error", "request_id":m["request_id"], "error":"unsupported"}})
        continue
    if m.get("type") == "user" and m.get("shouldQuery") is False:
        log("held-context", m["message"]["content"])
        if "--replay-user-messages" in args:
            emit(dict(m, isReplay=True))
        continue
    if m.get("type") != "user":
        continue
    full = " ".join(b.get("text", "") for b in m["message"]["content"] if b.get("type") == "text")
    text = full.split("[The new message:]")[-1]
    # Route by the newest actual message, not an appended environment update.
    # Log/replay still retains the complete request so cache tests inspect it.
    last = ([b.get("text", "") for b in m["message"]["content"] if b.get("type") == "text" and not b.get("text", "").startswith("[Environment update]")] or [""])[-1]
    text = last.split("[The new message:]")[-1]
    if "die quietly" in last and handled[0] > 0:
        # A process kept from an earlier turn that is gone by the time the next one comes.
        sys.exit(0)
    handled[0] += 1
    def taken():
        # As Claude Code does with --replay-user-messages: the message, echoed as it is taken up.
        if "--replay-user-messages" in args:
            emit({"type": "user", "isReplay": True, "uuid": m.get("uuid"), "message": m["message"], "session_id": session})
    if "notice first" in text:
        # As Claude Code resuming with something of its own to report (helpers a stopped run left):
        # a turn by itself first, then the message it was sent.
        emit({"type": "system", "subtype": "init", "session_id": session, "model": "claude-test-9", "apiKeySource": "none"})
        say("Your earlier helpers stopped.")
        result("ok")
    fast_on = json.loads(arg("--settings") or "{}").get("fastMode") is True
    fast_off = os.path.exists(os.path.join(HERE, "fast_off"))
    servers = [{"name": "tessera", "status": "failed" if os.path.exists(os.path.join(HERE, "mcp_failed")) else "connected"}] if bridge else []
    emit({"type": "system", "subtype": "init", "session_id": session, "model": "claude-test-9", "apiKeySource": "none",
          "mcp_servers": servers,
          "fast_mode_state": "on" if fast_on and not fast_off else "off",
          "fast_mode_disabled_reason": "extra_usage_disabled" if fast_on and fast_off else None})
    if os.path.exists("ledger-close-initial"):
        os.unlink("ledger-close-initial")
        # The message reached stdin, but no correlated acceptance happened.
        # Late output must not turn its pending ledger/boundary into delivered.
        say("Old output without taking the new input.")
        sys.exit(0)
    taken()
    if text.strip().startswith('native27 limit'):
        mode = text.strip().split()[-1]
        marker = os.path.join(os.getcwd(), 'native27-limit-hit')
        if mode == 'resume' and os.path.exists(marker):
            say('native27 continued after reset')
            result('ok')
            continue
        open(marker, 'w').write('hit')
        if mode != 'result-only':
            emit({'type':'rate_limit_event', 'rate_limit_info':{'status':'allowed_warning','utilization':.8,'rateLimitType':'five_hour'}})
            info = {'status':'rejected','rateLimitType':'five_hour'}
            if mode == 'repeat':
                reset_file = os.path.join(os.getcwd(), 'native27-repeat-reset')
                if not os.path.exists(reset_file): open(reset_file, 'w').write(str(int(time.time()) - 1))
                info['resetsAt'] = int(open(reset_file).read())
            elif mode in ['resume', 'stop']:
                info['resetsAt'] = int(time.time()) + (60 if mode == 'stop' else 1)
            elif mode == 'weekly':
                info.update(resetsAt=1791820800, rateLimitType='seven_day_sonnet')
            elif mode == 'credits':
                info.update(resetsAt=int(time.time())+1, errorCode='credits_required')
            emit({'type':'rate_limit_event','rate_limit_info':info})
        if mode == 'api':
            emit({'type':'assistant','error':'rate_limit','message':{'id':'native27-plan-error','content':[{'type':'text','text':"You've hit your limit · resets 2pm (America/Chicago)"}]}})
        emit({'type':'result','subtype':'error_during_execution','is_error':True, 'result':"You've hit your limit · resets 2pm (America/Chicago)", 'session_id':session,'usage':{'input_tokens':1,'output_tokens':0}})
        continue
    if text.strip() == 'native27 partial blocks':
        def native27_event(e):
            emit({'type':'stream_event','parent_tool_use_id':None,'event':e})
        mid = 'native27-shared-message'
        native27_event({'type':'message_start','message':{'id':mid}})
        for index, typ, chunk in [(0,'thinking','thinking in blocks'), (1,'text','native27 partial blocks finished')]:
            native27_event({'type':'content_block_start','index':index,'content_block':{'type':typ}})
            delta = {'type':'thinking_delta','thinking':chunk} if typ == 'thinking' else {'type':'text_delta','text':chunk}
            native27_event({'type':'content_block_delta','index':index,'delta':delta})
            native27_event({'type':'content_block_stop','index':index})
            block = {'type':typ,typ:chunk}
            emit({'type':'assistant','message':{'id':mid,'model':'fake-haiku','content':[block],'usage':{'input_tokens':11,'output_tokens':7}}})
        native27_event({'type':'message_stop'})
        result('ok')
        continue
    if text.strip() == 'native27 files':
        listed = mcp('tools/list') or {}
        log('native27-inventory', listed.get('result', {}).get('tools', []))
        out, err = call('write', {'path':'native27.txt','content':'alpha\nbeta\n'}, 'native27-write')
        log('native27-tool', {'name':'write','text':out,'error':err})
        out, err = call('read', {'path':'native27.txt','offset':1,'limit':1}, 'native27-read')
        log('native27-tool', {'name':'read','text':out,'error':err})
        out, err = call('edit', {'path':'native27.txt','lines':[{'at':'1','text':'gamma'}]}, 'native27-edit')
        log('native27-tool', {'name':'edit','text':out,'error':err})
        out, err = call('read', {'path':'native27-unseen.txt','offset':1,'limit':1}, 'native27-unseen-read')
        log('native27-tool', {'name':'unseen-read','text':out,'error':err})
        out, err = call('edit', {'path':'native27-unseen.txt','lines':[{'at':'2','text':'unseen'}]}, 'native27-unseen')
        log('native27-tool', {'name':'unseen','text':out,'error':err})
        out, err = call('glob', {'pattern':'native27*.txt'}, 'native27-glob')
        log('native27-tool', {'name':'glob','text':out,'error':err})
        out, err = call('web_fetch', {'url':'invalid-url'}, 'native27-reader')
        log('native27-tool', {'name':'web_fetch','text':out,'error':err})
        say('native27 file tools finished')
        result('ok')
        continue
    if "[Output check]" in text:
        inp = {"file_path": os.path.join(os.getcwd(), "report-extra.txt"), "content": "not needed"}
        say("Finishing the report.", [("toolu_report_write", "Write", inp)])
        r = hook("PreToolUse", {"tool_name": "Write", "tool_input": inp, "tool_use_id": "toolu_report_write"}, "toolu_report_write", "Write")
        ok = r.get("hookSpecificOutput", {}).get("permissionDecision") != "deny"
        log("report-action", {"allowed": ok, "tools": arg("--tools")})
        if ok:
            open(inp["file_path"], "w").write(inp["content"])
        tool_result("toolu_report_write", "done" if ok else "not performed", not ok)
        say("{}")
        result("ok")
        continue
    elif 'You ended without a report' in text:
        log('report-nudge', text)
        if open('empty-report-mode').read() != 'never':
            say('Finished: fixture work is uncommitted.')
        result('')
        continue
    elif 'empty helper report' in text:
        with open('empty-report-mode', 'w') as f: f.write('never' if 'never report' in text else 'recover')
        result('')
        continue
    elif 'helper inventory boundary' in text:
        say('Waiting at an action boundary.')
        log('helper-boundary', 'started')
        wait_for(lambda x: x.get('type') == 'control_request' and x['request']['subtype'] == 'interrupt')
        # The next process continues from its harness note, not this task again.
        sys.exit(0)
    elif 'inspect refreshed bridge' in text:
        listed = mcp('tools/list') or {}
        log('tools', [t['name'] for t in listed.get('result', {}).get('tools', [])])
        say('Inventory refreshed.')
        result('ok')
        continue
    elif text == 'earlier password tool fixture':
        say('Reading fixture output.', [('toolu_password_fixture', 'Bash', {'command':'fixture-command'})])
        tool_result('toolu_password_fixture', 'earlier-plaintext-fixture-897')
        say('Fixture read.')
        result('ok')
        continue
    elif text.strip().startswith('[Helper "'):
        # Told that its helper waits for an approval: look, try to approve, decline.
        name = text.split('"')[1]
        st, _ = call("agents", {"action": "status"}, "toolu_hw1")
        ap, ap_err = call("agents", {"action": "approve", "name": name}, "toolu_hw2")
        de, de_err = call("agents", {"action": "decline", "name": name}, "toolu_hw3")
        log("helper-wait", {"note": text, "status": st, "approve": ap, "approve_error": ap_err, "decline": de, "decline_error": de_err})
        say("Declined the helper's request.")
        result("ok")
        continue
    elif text.strip().startswith('[Report from subagent'):
        say('Helper report received.')
        result('ok')
        continue
    elif "wave2 slow boundary" in text:
        say("Boundary started.")
        log("boundary", "started")
        time.sleep(1)
        say("Boundary finished.")
        result("ok")
        continue
    elif text == "completion native bridged fixture":
        call("task", {"plan":[{"text":"Apply the native helper report", "status":"in_progress"}]}, "native-completion-ledger")
        call("agents", {"action":"start", "wait":False, "tasks":[{"name":"Native checker", "prompt":"COMPLETION_NATIVE_REPORT", "model":"mock/m"}]}, "native-completion-helper")
        result("native helper pending", finish=False)
        report = wait_for(lambda x: x.get("type") == "user")
        log("native-completion-report", report)
        echo = dict(report, isReplay=True, session_id=session)
        for bad in [dict(echo, uuid="foreign"), dict(echo, isSynthetic=True), dict(echo, parent_tool_use_id="child")]:
            emit(bad)
        result("native report unconfirmed", finish=False)
        if os.path.exists("completion-native-close"):
            say("Old output without accepting the report.")
            sys.exit(0)
        emit(echo)
        say("The native report needs action.")
        result("native report accepted", finish=False)
        emit(echo) # Exact duplicate must not renew the allowance again.
        result("native report replay", finish=False)
        result("native report done")
        continue
    elif text == "completion idle bash fixture":
        call("task", {"plan":[{"text":"Inspect the background command result", "status":"in_progress"}]}, "bash-completion-ledger")
        inp = {"command":"sleep 100", "description":"Outstanding build", "run_in_background":True}
        say("Starting a background command.", [("completion-bash", "Bash", inp)])
        emit({"type":"system", "subtype":"task_started", "task_id":"completion-bash", "tool_use_id":"completion-bash", "description":"Outstanding build", "is_backgrounded":True, "task_type":"local_bash"})
        tool_result("completion-bash", "Command running in background")
        say("Waiting on the background command.")
        result("idle command pending")
        log("idle-bash-ready", "outstanding")
        while not os.path.exists("completion-bash-crash"):
            time.sleep(0.01)
        # No completion event, child session or graceful Stop. The roster is lost.
        sys.exit(0)
    elif text == "lite background fixture":
        # Native-work stand-ins keep running until their work ends, stdin
        # closes, or Stop arrives. Agent omits task_started entirely; Task
        # reports it without a background flag. Both must preserve stdin.
        import select, threading
        def lite_read():
            if inbox: return inbox.pop(0)
            line = sys.stdin.readline()
            if not line: return None
            msg = json.loads(line); log("in", msg); return msg
        kind = open("lite-kind").read().strip()
        variant = open("lite-variant").read().strip() if os.path.exists("lite-variant") else ""
        count = int(open("lite-count").read())
        ending = threading.Event()
        rejected = variant in ["denied", "refused", "failed"]
        workers = [threading.Thread(target=lambda: ending.wait(), daemon=True) for _ in range(0 if rejected else count)]
        for worker in workers: worker.start()
        probe = [""]
        def lite_probe():
            if not os.path.exists("lite-probe"): return
            token = open("lite-probe").read()
            if token == probe[0]: return
            hook("Stop", {"stop_hook_active":True, "session_crons":crons})
            probe[0] = token; log("lite-probe", token)
        for i in range(count):
            call_id, task_id = "lite-call-%d" % i, "lite-task-%d" % i
            if kind == "wakeups": continue
            inp = {"run_in_background":variant not in ["foreground-id", "foreground-start"]}
            if kind in ["Agent", "Task"]: inp.update(description="Live helper", subagent_type="Explore", prompt="Fixture")
            else: inp["command"] = "fixture"
            say("Start work.", [(call_id, kind, inp)])
            if rejected:
                tool_result(call_id, variant + " before launch", True)
                continue
            if variant in ["foreground-id", "foreground-start"]:
                if variant == "foreground-start":
                    event = {"type":"system", "subtype":"task_started", "task_id":task_id, "tool_use_id":call_id, "is_backgrounded":False}
                    emit(event); log("lite-foreground-start", event)
                tool_result(call_id, "Foreground work done", extra={"agentId":"lite-agent-%d" % i if variant == "foreground-start" else task_id})
                ending.set(); continue
            if variant == "lost-launch": continue
            if variant in ["completion-first", "completion-first-alias"]:
                # No task_started or tool_use_id arrives before this completion.
                emit({"type":"system", "subtype":"task_notification", "task_id":task_id, "status":"completed"})
                extra = {"backgroundTaskId":task_id}
                if variant.endswith("-alias"): extra["agentId"] = "distinct-agent-%d" % i
                tool_result(call_id, "Late launch result", extra=extra)
                ending.set(); continue
            event = {"type":"system", "subtype":"task_started", "task_id":task_id, "tool_use_id":call_id, "description":"Live work"}
            if kind not in ["Agent", "Task"] and variant not in ["missing-flag", "result-first"]:
                event.update(is_backgrounded=True, task_type="local_bash" if kind=="Bash" else "local_monitor")
            if kind != "Agent" and variant not in ["missing-start", "result-first"]: emit(event)
            if variant == "early-completion":
                emit(event)
                emit({"type":"system", "subtype":"task_notification", "task_id":task_id, "status":"completed"})
                ending.set()
            extra = {"backgroundTaskId":task_id}
            if kind in ["Agent", "Task"]: extra["agentId"] = "lite-agent-%d" % i
            if variant == "agent-id": extra = {"agentId":task_id}
            tool_result(call_id, "Background work started", extra=extra)
            if variant == "result-first":
                event.pop("tool_use_id", None); emit(event)
            if variant == "long-replay":
                emit({"type":"system", "subtype":"task_notification", "task_id":task_id, "status":"completed"})
                for noise_index in range(320):
                    noise = "noise-%d" % noise_index
                    say("Ordinary read.", [(noise, "Read", {"file_path":"fixture.txt"})])
                    tool_result(noise, "Read result")
                emit(event)
                emit({"type":"system", "subtype":"task_started", "task_id":"late-alias", "tool_use_id":call_id, "is_backgrounded":True})
                log("lite-long-replay", True); ending.set()
        if kind == "wakeups":
            crons = [{"id":"wake-%d" % i} for i in range(count)]
        say("Waiting for background work.")
        result("waiting")
        log("lite-ready", count)
        if variant == "child-stop":
            hook("Stop", {"stop_hook_active":True, "agent_id":"child", "session_crons":[]})
            log("lite-child-stop", True)
        while not os.path.exists("lite-release"):
            lite_probe()
            if not inbox and not select.select([sys.stdin], [], [], .01)[0]: continue
            msg = lite_read()
            if msg is None:
                log("lite-early-eof", sum(w.is_alive() for w in workers)); ending.set(); sys.exit(0)
            if msg.get("type") == "control_request" and msg["request"].get("subtype") == "interrupt":
                log("lite-interrupted", True); ending.set()
                emit({"type":"result", "subtype":"success", "is_error":False, "result":"stopped", "session_id":session})
                break
            if msg.get("type") == "user":
                log("lite-new-message", msg["message"]["content"])
                if "--replay-user-messages" in args: emit(dict(msg, isReplay=True, session_id=session))
                say("New message received; work still runs.")
                result("waiting")
            log("lite-live", sum(w.is_alive() for w in workers))
        else:
            log("lite-ended", count); ending.set()
            if kind == "wakeups":
                crons = []
                hook("Stop", {"stop_hook_active":True, "session_crons":[]}, "lite-crons-end")
            else:
                for i in range(count):
                    event = {"type":"system", "subtype":"task_notification", "task_id":"lite-task-%d" % i, "status":"completed"}
                    if variant != "replay" and not variant: event["tool_use_id"] = "lite-call-%d" % i
                    emit(event)
                    if variant == "replay":
                        emit(event)
                        emit({"type":"system", "subtype":"task_started", "task_id":event["task_id"]})
                        emit({"type":"system", "subtype":"task_started", "task_id":"late-%d" % i, "tool_use_id":"lite-call-%d" % i, "is_backgrounded":True})
            if variant == "cycles":
                for i in range(12):
                    cid, tid = "cycle-call-%d" % i, "cycle-task-%d" % i
                    crons = [{"id":"cycle-wake-%d" % i}]
                    say("Cycle helper.", [(cid, "Agent", {"run_in_background":True, "prompt":"Fixture"})])
                    tool_result(cid, "Launched", extra={"agentId":tid})
                    result("cycle waiting")
                    hook("Stop", {"stop_hook_active":True, "agent_id":tid, "session_crons":[]})
                    emit({"type":"system", "subtype":"task_notification", "task_id":tid, "status":"completed"})
                    crons = []
                    hook("Stop", {"stop_hook_active":True, "session_crons":crons})
                    log("lite-cycle", i)
            if os.path.exists("lite-follow-up"):
                say("Background follow-up is a normal turn.")
                result("all done")
        log("lite-live", sum(w.is_alive() for w in workers))
        # No follow-up on purpose in the ordinary-end case. The existing
        # grace gives Claude time to answer, then stdin must close.
        while True:
            lite_probe()
            if not inbox and not select.select([sys.stdin], [], [], .01)[0]: continue
            msg = lite_read()
            if msg is None: log("lite-closed", True); sys.exit(0)
            if msg.get("type") == "control_request":
                emit({"type":"control_response", "response":{"subtype":"success", "request_id":msg["request_id"], "response":{}}})
        continue
    elif text.startswith("claude background command"):
        inp = {"command": "sleep 1", "description": "Watch build", "run_in_background": True}
        say("Running.", [("toolu_bgcmd", "Bash", inp)])
        hook("PreToolUse", {"tool_name": "Bash", "tool_input": inp, "tool_use_id": "toolu_bgcmd"}, "toolu_bgcmd", "Bash")
        emit({"type":"system", "subtype":"task_started", "task_id":"bgcmd", "tool_use_id":"toolu_bgcmd", "description":"Watch build", "is_backgrounded":True, "task_type":"local_bash"})
        tool_result("toolu_bgcmd", "Command running in background", extra={"backgroundTaskId":"bgcmd", "isBackground":True})
        emit({"type":"system", "subtype":"task_notification", "task_id":"bgcmd", "tool_use_id":"toolu_bgcmd", "status":"completed", "summary":"Build done"})
        say("Done.")
        result("ok")
        continue
    elif "helper budget27 " in text and "tessera agents " not in text:
        concurrent = "concurrent" in text
        bridge_case = "bridge" in text
        calls = [("budget27-a", "Glob", {"pattern":"first*"}), ("budget27-b", "Glob", {"pattern":"second*"})]
        if bridge_case:
            for i in range(2):
                call("docs", {"topic":"bots"}, "budget27-bridge-%d" % i)
        else:
            say("Proposed bounded actions.", calls)
            decisions = []
            if concurrent:
                cid = hooks["PreToolUse"][0]["hookCallbackIds"][0]
                for tid, name, inp in calls:
                    req = {"subtype":"hook_callback", "callback_id":cid, "tool_use_id":tid,
                           "input":{"hook_event_name":"PreToolUse", "session_id":session, "cwd":os.getcwd(), "tool_name":name, "tool_input":inp, "tool_use_id":tid}}
                    emit({"type":"control_request", "request_id":"hook-"+tid, "request":req})
                for tid, name, inp in calls:
                    reply = wait_for(lambda m: m.get("type") == "control_response" and m["response"]["request_id"] == "hook-"+tid)
                    r = (reply or {}).get("response", {}).get("response") or {}
                    decisions.append(r)
            for i, (tid, name, inp) in enumerate(calls):
                r = decisions[i] if concurrent else hook("PreToolUse", {"tool_name":name, "tool_input":inp, "tool_use_id":tid}, tid, name)
                ok = r.get("hookSpecificOutput", {}).get("permissionDecision") != "deny"
                # A repeated permission stage must not consume another unit.
                ok = ask(tid, name, inp) and ok
                log("budget27-action", {"id":tid, "allowed":ok, "reply":r})
                if ok:
                    log("executed", {"tool":name, "id":tid})
                tool_result(tid, "done" if ok else r.get("hookSpecificOutput", {}).get("permissionDecisionReason", "Budget exhausted; not executed"), not ok)
        say("Final budget27 report: one action ran.")
        result("Final budget27 report.")
    elif "builtin actions " in text and "tessera agents " not in text:
        actions = json.JSONDecoder().raw_decode(text.split("builtin actions ", 1)[1])[0]
        for i, action in enumerate(actions):
            name, inp = action["tool"], action.get("input", {})
            tid = action.get("id", "toolu_contract_%d" % i)
            say("Using " + name, [(tid, name, inp)])
            if action.get("wait_mode"):
                log("await-mode", tid)
                wait_for(lambda m: m.get("type") == "control_request" and m["request"]["subtype"] == "set_permission_mode")
            hook_input = dict(action.get("metadata", {}), tool_name=name, tool_input=inp, tool_use_id=tid)
            r = hook("PreToolUse", hook_input, tid, name)
            ok = r.get("hookSpecificOutput", {}).get("permissionDecision") != "deny"
            if action.get("prompt_even_if_denied") or (ok and action.get("prompt")):
                ok = ask(tid, name, inp)
            if ok and action.get("cli_refuses"):
                ok = False
            if ok:
                log("executed", {"tool": name, "input": inp, "id": tid})
                if name == "Write":
                    open(inp["file_path"], "w").write(inp["content"])
                if name in ("Write", "Edit", "MultiEdit", "NotebookEdit"):
                    hook("PostToolUse", hook_input, tid, name)
            tool_result(tid, action.get("result", "done" if ok else "not performed"), action.get("is_error", not ok), extra=action.get("extra"))
        say("Actions finished.")
        result("ok")
    elif "ledger change then wait" in text:
        call("task", {"constraints": ["V2 constraint: never deploy"]}, "toolu_ledger")
        log("ledger-ready", "V2 saved while the first input was running")
        extra = wait_for(lambda x: x.get("type") == "user")
        if os.path.exists("ledger-close-queued"):
            os.unlink("ledger-close-queued")
            log("ledger-untaken", extra)
            say("Previous work finished after the queued input, without taking it.")
            sys.exit(0)
        emit({"type": "user", "isReplay": True, "uuid": extra.get("uuid"), "message": extra["message"], "session_id": session})
        log("ledger-accepted", extra.get("uuid"))
        say("Queued input accepted.")
        result("ok")
        continue
    elif "steer me" in text:
        # As Claude Code does: a message sent while a command runs is taken up after it, in this turn.
        say("Working on it.", [("toolu_slow", "Bash", {"command": "sleep 2", "description": "Wait"})])
        extra = wait_for(lambda x: x.get("type") == "user")
        tool_result("toolu_slow", "done")
        if extra is not None:
            emit({"type": "user", "isReplay": True, "uuid": extra.get("uuid"), "message": extra["message"], "session_id": session})
            said = " ".join(b.get("text", "") for b in extra["message"]["content"] if b.get("type") == "text" and not b.get("text", "").startswith("[Tessera work state"))
            say("Steered: " + said.split("[The new message:]")[-1].strip())
        else:
            say("Not steered.")
        result("ok")
        continue
    if "notice first" in text:
        after = {"command": "echo after", "description": "Check the result"}
        say("Now your message.", [("toolu_after2", "Bash", after)])
        ok = ask("toolu_after2", "Bash", after)
        tool_result("toolu_after2", "after" if ok else "declined", not ok)
        say("All done." if ok else "Couldn't check: " + ("no answer" if ok is None else "declined"))
        result("ok")
        continue
    if helper:
        out, err = call("memory", {"target": "memory", "action": "add", "content": "The user likes stub tests."}, "toolu_rev")
        say("Saved." if not err else "Could not save: " + out)
        result("ok")
    elif text.strip().startswith("unknown result"):
        say("Partial answer.")
        emit({"type":"result", "subtype":"future_subtype", "is_error":False, "result":"Everything succeeded", "session_id":session})
    elif text.strip().startswith("late "):
        name, inp = parse_call(text, "late ")
        say("Done early.")
        result("ok")
        r = mcp("tools/call", {"name": name, "arguments": inp, "_meta": {"claudecode/toolUseId": "toolu_late"}})
        log("late", r)
    elif "ask then tessera " in text:
        name, inp = parse_call(text, "ask then tessera ")
        say("Asking.", [("toolu_ask", "mcp__tessera__" + name, inp)])
        log("asked", ask("toolu_ask", "mcp__tessera__" + name, inp))
        r = mcp("tools/call", {"name": name, "arguments": inp, "_meta": {"claudecode/toolUseId": "toolu_ask"}})
        res = (r or {}).get("result", {})
        out = "".join(c.get("text", "") for c in res.get("content", []) if c.get("type") == "text")
        tool_result("toolu_ask", out, res.get("isError", False))
        log("called", {"name": name, "text": out, "error": res.get("isError", False)})
        say("Tool said: " + out[:120])
        result("ok")
    elif "tessera " in text:
        name, inp = parse_call(text, "tessera ")
        out, err = call(name, inp, "toolu_t%d" % n[0])
        say(("Tool failed: " if err else "Tool said: ") + out[:120])
        result("ok")
    elif "make tasks" in text:
        # Newer Claude Code: the task tools (as 2.1 sends them).
        for k, subject in enumerate(["Buy bread", "Pack plates", "Pick a park"], 1):
            say("", [("toolu_tc%d" % k, "TaskCreate", {"subject": subject, "description": subject + " for the picnic"})])
            tool_result("toolu_tc%d" % k, "Task #%d created successfully: %s" % (k, subject), extra={"task": {"id": str(k), "subject": subject}})
        say("", [("toolu_tu1", "TaskUpdate", {"taskId": "1", "status": "in_progress"})])
        tool_result("toolu_tu1", "Updated task #1 status", extra={"success": True, "taskId": "1", "updatedFields": ["status"]})
        say("Tasks made.")
        result("ok")
    elif "finish the first task" in text:
        say("", [("toolu_tu2", "TaskUpdate", {"taskId": "1", "status": "completed"})])
        tool_result("toolu_tu2", "Updated task #1 status", extra={"success": True, "taskId": "1"})
        say("", [("toolu_tu3", "TaskUpdate", {"taskId": "2", "status": "in_progress", "subject": "Pack plates and cups"})])
        tool_result("toolu_tu3", "Updated task #2 status", extra={"success": True, "taskId": "2"})
        say("", [("toolu_tu4", "TaskUpdate", {"taskId": "3", "status": "deleted"})])
        tool_result("toolu_tu4", "Updated task #3 deleted", extra={"success": True, "taskId": "3"})
        say("Moved on.")
        result("ok")
    elif "make a todo list" in text:
        todos = [{"content": "Read the logs", "status": "completed", "activeForm": "Reading the logs"},
                 {"content": "Fix the timeout", "status": "in_progress", "activeForm": "Fixing the timeout"},
                 {"content": "Run the tests", "status": "pending", "activeForm": "Running the tests"}]
        say("Planning.", [("toolu_td1", "TodoWrite", {"todos": todos})])
        tool_result("toolu_td1", "Todos have been modified successfully.")
        say("Planned.")
        result("ok")
    elif "question budget27 " in text and "tessera agents " not in text:
        inp = {"questions":[{"question":"Which one?", "header":"Choice", "multiSelect":False,
               "options":[{"label":"One", "description":""}, {"label":"Two", "description":""}]}]}
        missing = "missing" in text
        ids = ["question-first"] if missing else ["question-first", "question-duplicate"]
        for rid in ids:
            req = {"subtype":"can_use_tool", "tool_name":"AskUserQuestion", "input":inp}
            if not missing:
                req["tool_use_id"] = "question-call"
            emit({"type":"control_request", "request_id":rid, "request":req})
        if "cancel duplicate" in text:
            emit({"type":"control_cancel_request", "request_id":"question-first"})
        for rid in ids:
            reply = wait_for(lambda m: m.get("type") == "control_response" and m["response"]["request_id"] == rid)
            log("question-budget27", {"id":rid, "reply":reply["response"]["response"]})
        if missing:
            tid = "after-missing-question"
            r = hook("PreToolUse", {"tool_name":"Glob", "tool_input":{"pattern":"*"}, "tool_use_id":tid}, tid, "Glob")
            if r.get("hookSpecificOutput", {}).get("permissionDecision") != "deny":
                log("executed", {"tool":"Glob", "id":tid})
            tool_result(tid, "done")
        say("Question budget27 final report.")
        result("ok")
    elif "ask me which" in text:
        inp = {"questions": [
            {"question": "Which database?", "header": "Database", "multiSelect": False,
             "options": [{"label": "Postgres", "description": "The usual"}, {"label": "SQLite", "description": "One file"}]},
            {"question": "Which extras?", "header": "Extras", "multiSelect": True,
             "options": [{"label": "Backups", "description": ""}, {"label": "Metrics", "description": ""}]}]}
        say("Asking.", [("toolu_q1", "AskUserQuestion", inp)])
        emit({"type": "control_request", "request_id": "perm-toolu_q1", "request": {"subtype": "can_use_tool", "tool_name": "AskUserQuestion", "input": inp, "tool_use_id": "toolu_q1"}})
        m = wait_for(lambda m: m.get("type") == "control_response" and m["response"]["request_id"] == "perm-toolu_q1")
        r = m["response"]["response"] if m else {}
        log("answered", {"behavior": r.get("behavior"), "answers": r.get("updatedInput", {}).get("answers"), "message": r.get("message")})
        tool_result("toolu_q1", json.dumps(r.get("updatedInput", {}).get("answers")) if r.get("behavior") == "allow" else r.get("message", ""), r.get("behavior") != "allow")
        say("Got it.")
        result("ok")
    elif "completion dependency fixture" in text:
        # Two own helpers: only one finishes before the parent must act.
        call("task", {"plan":[{"text":"slow helper work", "status":"in_progress"}, {"text":"fast helper report needs action", "status":"in_progress"}]}, "completion-ledger")
        for name in ["fast", "slow"]:
            cid = "completion-" + name
            inp = {"description": name, "prompt": name, "subagent_type": "general-purpose", "run_in_background": True}
            say("Starting " + name, [(cid, "Agent", inp)])
            emit({"type":"system", "subtype":"task_started", "task_id":cid, "tool_use_id":cid, "is_backgrounded":True})
            tool_result(cid, "Async agent launched.")
        say("Waiting, but independent work remains.")
        result("helpers waiting")
        time.sleep(0.2)
        event = {"type":"system", "subtype":"task_notification", "task_id":"completion-fast", "tool_use_id":"completion-fast", "status":"completed", "summary":"Report needs action"}
        emit(event)
        say("The fast helper finished; its report needs action.")
        result("report needs action")
        time.sleep(0.2)
        emit(event) # Replay must not renew the allowance or invalidate again.
        say("Duplicate report only.")
        result("duplicate notification")
        emit({"type":"system", "subtype":"task_notification", "task_id":"completion-slow", "tool_use_id":"completion-slow", "status":"completed", "summary":"done"})
        say("Helpers finished.")
        result("finished helpers")
    elif "delegate in the background" in text:
        inp = {"description": "Watch the logs", "prompt": "watch them", "subagent_type": "general-purpose", "run_in_background": True}
        say("Starting a helper.", [("toolu_bg", "Agent", inp)])
        emit({"type": "system", "subtype": "task_started", "task_id": "task2", "tool_use_id": "toolu_bg", "description": "Watch the logs", "subagent_type": "general-purpose", "is_backgrounded": True})
        tool_result("toolu_bg", "Async agent launched successfully.")
        say("It runs in the background.")
        say("Watching.", [("toolu_bg1", "Bash", {"command": "tail -n 5 log"})], parent="toolu_bg")
        tool_result("toolu_bg1", "all quiet", parent="toolu_bg")
        say("All quiet in the logs.", parent="toolu_bg")
        emit({"type": "system", "subtype": "task_notification", "task_id": "task2", "tool_use_id": "toolu_bg", "status": "completed", "summary": "quiet", "usage": {"total_tokens": 500, "tool_uses": 1, "duration_ms": 900}})
        result("ok")
    elif "run it in the background" in text:
        inp = {"command": "sleep 30", "description": "Wait a while", "run_in_background": True}
        say("Starting it.", [("toolu_bash_bg", "Bash", inp)])
        emit({"type": "system", "subtype": "background_tasks_changed", "tasks": [{"task_id": "b1", "task_type": "local_bash", "description": "Wait a while"}]})
        emit({"type": "system", "subtype": "task_started", "task_id": "b1", "tool_use_id": "toolu_bash_bg", "description": "Wait a while", "is_backgrounded": True, "task_type": "local_bash"})
        tool_result("toolu_bash_bg", "Command running in background with ID: b1")
        say("It is running.")
        result("ok")
        # As Claude Code does: when the command is done it takes the turn up again by itself.
        time.sleep(1.5 if "slowly" in text else 0.4)
        emit({"type": "system", "subtype": "task_notification", "task_id": "b1", "tool_use_id": "toolu_bash_bg", "status": "completed", "summary": "done"})
        if "then check" in text:
            after = {"command": "echo after", "description": "Check the result"}
            say("It finished; checking.", [("toolu_after", "Bash", after)])
            ok = ask("toolu_after", "Bash", after)
            tool_result("toolu_after", "after" if ok else "declined", not ok)
            say("All done." if ok else "Couldn't check: " + ("no answer" if ok is None else "declined"))
        else:
            say("It finished.")
        result("ok")
    elif "delegate" in text:
        inp = {"description": "Count the files", "prompt": "count them", "subagent_type": "Explore"}
        say("Delegating.", [("toolu_ag", "Agent", inp)])
        emit({"type": "system", "subtype": "task_started", "task_id": "task1", "tool_use_id": "toolu_ag", "description": "Count the files", "subagent_type": "Explore"})
        say("Looking.", [("toolu_sub1", "Glob", {"pattern": "**/*"})], parent="toolu_ag")
        tool_result("toolu_sub1", "a.txt", parent="toolu_ag")
        if bridge:
            call("docs", {"topic": "bots"}, "toolu_sub2", parent="toolu_ag")
            if "nested" in text:
                call("agents", {"action": "start", "wait": True, "tasks": [{"name": "Grandchild", "prompt": "Check", "model": "mock/m"}]}, "toolu_sub3", parent="toolu_ag")
            if "screen" in text:
                call("desktop", {"action": "screenshot"}, "toolu_sub4", parent="toolu_ag")
        emit({"type": "system", "subtype": "task_progress", "task_id": "task1", "tool_use_id": "toolu_ag", "description": "Count the files", "summary": "Found one file", "usage": {"total_tokens": 900, "tool_uses": 2, "duration_ms": 1500}})
        emit({"type": "system", "subtype": "task_notification", "task_id": "task1", "tool_use_id": "toolu_ag", "status": "completed", "output_file": "", "summary": "done", "usage": {"total_tokens": 1000, "tool_uses": 2, "duration_ms": 1600}})
        tool_result("toolu_ag", "There is 1 file.")
        say("One file.")
        result("ok")
    elif "tally" in text:
        # Running totals, as Claude Code reports them, with a second (small) model in them.
        tally[0] += 1
        k = tally[0]
        time.sleep(0.6)
        say("Tally %d." % k)
        emit({"type": "result", "subtype": "success", "is_error": False, "result": "ok", "session_id": session,
              "usage": {"input_tokens": 100 * k, "output_tokens": 20 * k},
              "modelUsage": {"claude-haiku-8": {"inputTokens": 7 * k, "outputTokens": 1 * k, "costUSD": 0.001 * k, "contextWindow": 200000},
                             "claude-test-9": {"inputTokens": 100 * k, "outputTokens": 20 * k, "costUSD": 0.01 * k, "contextWindow": 1000000}}})
    elif text.strip().startswith("/compact"):
        emit({"type":"system", "subtype":"status", "status":"compacting"})
        emit({"type": "system", "subtype": "compact_boundary", "compact_metadata": {"trigger": "manual", "pre_tokens": 52000, "post_tokens": 4000}})
        result("")
    elif "check my limits" in text:
        # As Claude Code 2.1 reports them: every window it tracks, and the one limiting now.
        t = int(time.time())
        windows = {"five_hour": {"utilization": 0.42, "resetsAt": t + 7200}, "seven_day": {"utilization": 0.18, "resetsAt": t + 400000}}
        emit({"type": "rate_limit_event", "uuid": "rl1", "session_id": session, "rate_limit_info": {"status": "allowed", "resetsAt": t + 7200,
              "rateLimitType": "five_hour", "utilization": 0.42, "isUsingOverage": False, "unifiedWindows": windows}})
        emit({"type": "rate_limit_event", "uuid": "rl2", "session_id": session, "rate_limit_info": {"status": "rejected", "resetsAt": t + 86400,
              "rateLimitType": "seven_day_opus", "unifiedWindows": windows}})
        say("Checked.")
        result("ok")
    elif "use up the plan" in text:
        if not ACCOUNT:
            # The usual account's plan is used up for hours.
            emit({"type": "rate_limit_event", "rate_limit_info": {"status": "rejected", "resetsAt": int(time.time()) + 3 * 3600, "rateLimitType": "five_hour"}})
            result("You've hit your limit · resets in 3 hours", True)
        else:
            say("Answered on " + os.path.basename(ACCOUNT) + (" saw-digest" if "[Earlier in this chat" in full else ""))
            result("ok")
    elif "hit the limit" in text:
        mark = os.path.join(HERE, "limited")
        if not os.path.exists(mark):
            open(mark, "w").close()
            emit({"type": "rate_limit_event", "rate_limit_info": {"status": "rejected", "resetsAt": int(time.time()) + 1, "rateLimitType": "five_hour"}})
            result("You've hit your limit · resets soon", True)
        else:
            emit({"type": "rate_limit_event", "rate_limit_info": {"status": "allowed_warning", "utilization": 0.91, "resetsAt": int(time.time()) + 3600, "rateLimitType": "seven_day"}})
            say("Back after the reset.")
            result("ok")
    elif "permission dismissed" in text:
        inp = {"command": "echo hi", "description": "Say hi"}
        say("Asking, then closing the question.", [("toolu_dismiss", "Bash", inp)])
        emit({"type": "control_request", "request_id": "perm-dismiss", "request": {"subtype": "can_use_tool", "tool_name": "Bash", "input": inp, "tool_use_id": "toolu_dismiss"}})
        time.sleep(0.1)
        emit({"type": "control_cancel_request", "request_id": "perm-dismiss"})
        m = wait_for(lambda m: m.get("type") == "control_response" and m["response"]["request_id"] == "perm-dismiss")
        if m is not None:
            tool_result("toolu_dismiss", m["response"]["response"]["message"], True)
            say("Carried on after no answer.")
            result("ok")
    elif "run a command" in text:
        inp = {"command": "echo hi", "description": "Say hi"}
        say("Running it.", [("toolu_1", "Bash", inp)])
        ok = ask("toolu_1", "Bash", inp)
        if ok is None:
            result("", True, "error_during_execution"); continue
        tool_result("toolu_1", "hi" if ok else "The user declined", not ok)
        say("Done: " + ("allowed" if ok else "denied"))
        result("Done")
    elif "write to " in text:
        path = text.split("write to ", 1)[1].split()[0]
        inp = {"file_path": path, "content": "x"}
        say("Writing.", [("toolu_3", "Write", inp)])
        ok = ask("toolu_3", "Write", inp)
        tool_result("toolu_3", "written" if ok else "denied", not ok)
        say("Wrote." if ok else "Not written.")
        result("ok")
    elif "change code" in text:
        # As Claude Code 2.1.286 with Tessera's hooks: PreToolUse before each change, also where
        # no permission prompt comes (acceptEdits); the change is real.
        cwd = os.getcwd()
        app, util = os.path.join(cwd, "app.py"), os.path.join(cwd, "util.py")
        edit = {"file_path": app, "old_string": "print(1)", "new_string": "print(2)"}
        say("Changing.", [("toolu_e1", "Edit", edit)])
        r = hook("PreToolUse", {"tool_name": "Edit", "tool_input": edit, "tool_use_id": "toolu_e1"}, "toolu_e1", "Edit")
        ok = r.get("hookSpecificOutput", {}).get("permissionDecision") != "deny"
        if ok and arg("--permission-mode") == "default":
            ok = ask("toolu_e1", "Edit", edit)
        if ok:
            src = open(app).read()
            open(app, "w").write(src.replace("print(1)", "print(2)"))
            tool_result("toolu_e1", "The file app.py has been updated.", False, {"filePath": app, "structuredPatch": [
                {"oldStart": 1, "oldLines": 1, "newStart": 1, "newLines": 1, "lines": ["-print(1)", "+print(2)"]}]})
        else:
            tool_result("toolu_e1", "The user declined", True)
        write = {"file_path": util, "content": "x = 1\n"}
        say("Adding a file.", [("toolu_e2", "Write", write)])
        hook("PreToolUse", {"tool_name": "Write", "tool_input": write, "tool_use_id": "toolu_e2"}, "toolu_e2", "Write")
        open(util, "w").write("x = 1\n")
        tool_result("toolu_e2", "File created successfully at: " + util, False,
                    {"type": "create", "filePath": util, "content": "x = 1\n", "structuredPatch": []})
        if "and test" in text:
            t = {"command": "python3 -m pytest -q", "description": "Run the tests"}
            say("Testing.", [("toolu_t1", "Bash", t)])
            tool_result("toolu_t1", "1 passed")
        say("Changed.")
        result("ok")
    elif "hand off a change" in text:
        inp = {"description": "Fix the bug", "prompt": "fix it", "subagent_type": "general-purpose"}
        say("Delegating.", [("toolu_agc", "Agent", inp)])
        emit({"type": "system", "subtype": "task_started", "task_id": "task3", "tool_use_id": "toolu_agc", "description": "Fix the bug",
              "subagent_type": "general-purpose", "is_backgrounded": False, "task_type": "local_agent"})
        app = os.path.join(os.getcwd(), "app.py")
        edit = {"file_path": app, "old_string": "print(1)", "new_string": "print(3)"}
        say("Fixing.", [("toolu_sube", "Edit", edit)], parent="toolu_agc")
        hook("PreToolUse", {"tool_name": "Edit", "tool_input": edit, "tool_use_id": "toolu_sube", "agent_id": "a1", "agent_type": "general-purpose"}, "toolu_sube", "Edit")
        src = open(app).read()
        open(app, "w").write(src.replace("print(1)", "print(3)"))
        tool_result("toolu_sube", "The file app.py has been updated.", False, {"filePath": app, "structuredPatch": []}, parent="toolu_agc")
        emit({"type": "system", "subtype": "task_notification", "task_id": "task3", "tool_use_id": "toolu_agc", "status": "completed", "summary": "fixed"})
        tool_result("toolu_agc", "Fixed it.")
        say("Fixed.")
        result("ok")
    elif "slow helper" in text:
        bg = "in the background" in text
        inp = {"description": "Watch it", "prompt": "watch", "subagent_type": "general-purpose", "run_in_background": bg}
        say("Starting a helper.", [("toolu_slow_ag", "Agent", inp)])
        emit({"type": "system", "subtype": "task_started", "task_id": "task_s", "tool_use_id": "toolu_slow_ag", "description": "Watch it",
              "subagent_type": "general-purpose", "is_backgrounded": bg, "task_type": "local_agent"})
        if bg:
            tool_result("toolu_slow_ag", "Async agent launched successfully.")
        say("Watching.", [("toolu_w1", "Bash", {"command": "sleep 100"})], parent="toolu_slow_ag")
        m = wait_for(lambda m: m.get("type") == "control_request" and m["request"]["subtype"] == "stop_task")
        if m is None:
            result("", True, "error_during_execution")
            continue
        log("stop_task", m["request"])
        emit({"type": "control_response", "response": {"subtype": "success", "request_id": m["request_id"], "response": {}}})
        emit({"type": "system", "subtype": "task_updated", "task_id": "task_s", "patch": {"status": "killed"}})
        tool_result("toolu_w1", "Interrupted", True, parent="toolu_slow_ag")
        emit({"type": "system", "subtype": "task_notification", "task_id": "task_s", "tool_use_id": "toolu_slow_ag", "status": "completed" if "late completion" in text else "stopped", "summary": "Watch it"})
        if not bg:
            tool_result("toolu_slow_ag", "[Request interrupted by user]", True)
        say("Went on without it.")
        result("ok")
    elif "move it to the background" in text:
        # A foreground command past its time limit goes on in the background (2.1.2xx).
        inp = {"command": "make build", "description": "Build"}
        say("Building.", [("toolu_mv", "Bash", inp)])
        emit({"type": "system", "subtype": "task_started", "task_id": "bmv", "tool_use_id": "toolu_mv", "description": "Build",
              "is_backgrounded": False, "task_type": "local_bash"})
        emit({"type": "system", "subtype": "task_updated", "task_id": "bmv", "patch": {"is_backgrounded": True}})
        tool_result("toolu_mv", "Command did not complete within its 120s timeout and was moved to the background with ID: bmv.")
        say("It goes on in the background.")
        result("ok")
        time.sleep(0.5)
        emit({"type": "system", "subtype": "task_notification", "task_id": "bmv", "tool_use_id": "toolu_mv", "status": "completed", "summary": "Build"})
        say("The build finished.")
        result("ok")
    elif "schedule a wakeup" in text:
        crons.append({"id": "c1", "schedule": "*/5 * * * *", "recurring": True, "prompt": "check the build"})
        say("Scheduled.")
        result("ok")
    elif "carry on after parking" in text:
        say("Started it; I'll look when it's done.")
        result("ok")
        # Background work Tessera didn't see ends: Claude Code takes the turn up by
        # itself, in the process kept between turns.
        time.sleep(2.5 if "slowly" in text else 1.0)
        emit({"type": "system", "subtype": "init", "session_id": session, "model": "claude-test-9", "apiKeySource": "none"})
        out = os.path.join(os.getcwd(), "bg-output.txt")
        read_inp = {"file_path": out}
        say("The background output is ready; reading it.", [("toolu_bg_read", "Read", read_inp)])
        r = hook("PreToolUse", {"tool_name": "Read", "tool_input": read_inp, "tool_use_id": "toolu_bg_read"}, "toolu_bg_read", "Read")
        ok = (r.get("hookSpecificOutput") or {}).get("permissionDecision") != "deny"
        log("self-read", {"allowed": ok, "why": (r.get("hookSpecificOutput") or {}).get("permissionDecisionReason")})
        tool_result("toolu_bg_read", open(out).read() if ok else "not read", not ok)
        risky = {"command": "rm -rf build", "description": "Clean up"}
        say("Cleaning up.", [("toolu_bg_rm", "Bash", risky)])
        r = hook("PreToolUse", {"tool_name": "Bash", "tool_input": risky, "tool_use_id": "toolu_bg_rm"}, "toolu_bg_rm", "Bash")
        hook_ok = (r.get("hookSpecificOutput") or {}).get("permissionDecision") != "deny"
        answer = {}
        if hook_ok:
            emit({"type": "control_request", "request_id": "perm-toolu_bg_rm", "request": {"subtype": "can_use_tool", "tool_name": "Bash", "input": risky, "tool_use_id": "toolu_bg_rm"}})
            perm = wait_for(lambda m: m.get("type") == "control_response" and m["response"]["request_id"] == "perm-toolu_bg_rm")
            answer = (perm or {}).get("response", {}).get("response") or {}
        ok = hook_ok and answer.get("behavior") == "allow"
        log("self-risky", {"allowed": ok, "hook": r, "answer": answer})
        tool_result("toolu_bg_rm", "removed" if ok else "not run", not ok)
        say("Done after carrying on." if ok else "Not cleaned up.")
        result("ok")
    elif "risky helper command" in text:
        # A helper on Claude Code: a command its mode asks about (PreToolUse),
        # then one Claude Code itself asks about in every mode.
        risky = {"command": "git clean -nf", "description": "Clean"}
        say("Cleaning.", [("toolu_hr", "Bash", risky)])
        r = hook("PreToolUse", {"tool_name": "Bash", "tool_input": risky, "tool_use_id": "toolu_hr"}, "toolu_hr", "Bash")
        ok = (r.get("hookSpecificOutput") or {}).get("permissionDecision") != "deny"
        log("helper-risky", {"allowed": ok, "why": (r.get("hookSpecificOutput") or {}).get("permissionDecisionReason")})
        tool_result("toolu_hr", "cleaned" if ok else "not run", not ok)
        if "classified" in text:
            inp = {"command": "echo classified", "description": "Echo"}
            say("Checking.", [("toolu_hc", "Bash", inp)])
            emit({"type": "control_request", "request_id": "perm-toolu_hc", "request": {"subtype": "can_use_tool", "tool_name": "Bash", "input": inp,
                  "tool_use_id": "toolu_hc", "classifier_approvable": False, "decision_reason": "Claude Code asks before this command."}})
            m = wait_for(lambda m: m.get("type") == "control_response" and m["response"]["request_id"] == "perm-toolu_hc")
            answer = (m or {}).get("response", {}).get("response") or {}
            log("helper-classified", {"allowed": answer.get("behavior") == "allow", "message": answer.get("message")})
            tool_result("toolu_hc", "ok" if answer.get("behavior") == "allow" else "not run", answer.get("behavior") != "allow")
        say("Helper done.")
        result("ok")
    elif "two helpers then carry on" in text:
        call("agents", {"action": "start", "wait": False, "tasks": [
            {"name": "Quick", "prompt": "QUICK-REPORT now", "model": "mock/m"},
            {"name": "Slow", "prompt": "SLOW-REPORT later", "model": "mock/m"}]}, "toolu_two")
        say("Two helpers started.")
        result("ok")
        # The quick one reports meanwhile (no turn: its batch isn't done), then
        # Claude Code takes a turn up by itself in the kept process.
        time.sleep(3.0)
        emit({"type": "system", "subtype": "init", "session_id": session, "model": "claude-test-9", "apiKeySource": "none"})
        say("Carrying on by myself.")
        result("ok")
    elif "wake up by yourself" in text:
        say("Later.")
        result("ok")
        # As if a wake-up fired between turns.
        time.sleep(0.5)
        emit({"type": "system", "subtype": "init", "session_id": session, "model": "claude-test-9", "apiKeySource": "none"})
        say("I woke up by myself.")
        result("ok")
    elif "native edit context fixture" in text:
        value = open('notes.txt').read().splitlines()[0]
        say('Editing.', [('toolu_context', 'Edit', {'file_path':os.path.join(os.getcwd(), 'notes.txt'), 'old_string':'old', 'new_string':'new'})])
        tool_result('toolu_context', 'Updated.', False, {'filePath':'notes.txt', 'structuredPatch':[{'oldStart':1,'oldLines':2,'newStart':1,'newLines':2,'lines':[' ' + value, '-old', '+new']}]})
        result('done')
    elif "edit a file" in text:
        cwd = os.getcwd()
        inp = {"file_path": os.path.join(cwd, "a.txt"), "old_string": "old", "new_string": "new"}
        say("Editing.", [("toolu_2", "Edit", inp)])
        tool_result("toolu_2", "The file a.txt has been updated.", False, {"filePath": inp["file_path"], "structuredPatch": [
            {"oldStart": 1, "oldLines": 1, "newStart": 1, "newLines": 1, "lines": ["-old", "+new"]}]})
        say("Edited.")
        result("Edited.")
    elif "read " in text:
        path = text.split("read ", 1)[1].split()[0]
        try:
            open(path).read(); seen = "visible"
        except OSError:
            seen = "hidden"
        say("The file is " + seen + ".")
        result(seen)
    elif "count processes" in text:
        procs = sum(1 for d in os.listdir("/proc") if d.isdigit())
        say("%d processes" % procs)
        result("ok")
    elif "hang" in text.split():
        # Match the fixture's command word, never the substring in "unchanged".
        say("Working on it")
        wait_for(lambda m: False)
        result("", True, "error_during_execution")
    elif "trail off" in text:
        say("First, the plan.")
        n[0] += 1
        mid = "msg_%d" % n[0]
        emit({"type": "stream_event", "parent_tool_use_id": None, "event": {"type": "message_start", "message": {"id": mid}}})
        emit({"type": "stream_event", "parent_tool_use_id": None, "event": {"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}})
        emit({"type": "stream_event", "parent_tool_use_id": None, "event": {"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "Halfway through a sent"}}})
        wait_for(lambda m: False)
        result("", True, "error_during_execution")
    elif "fail login" in text:
        result("Invalid API key · Please run /login", True)
    elif "crash midway" in last:
        say("Starting the build.", [("toolu_crash", "Bash", {"command": "make", "description": "Build"})])
        os.kill(os.getpid(), 9)
    elif "print plainly" in last:
        sys.stdout.write("Fatal: the plain words of a failure\n"); sys.stdout.flush()
        sys.exit(3)
    elif "garble" in last:
        # Lines it doesn't know or that aren't JSON at all, retries and a model fallback: none of it stops the turn.
        sys.stdout.write("not json at all\n{broken json\n"); sys.stdout.flush()
        emit({"type": "mystery", "x": 1})
        emit({"type": "system", "subtype": "brand_new_thing", "x": 2})
        emit({"type": "control_response", "response": {"subtype": "success", "request_id": "nobody", "response": {}}})
        for a in (1, 2, 3):
            emit({"type": "system", "subtype": "api_retry", "attempt": a, "max_retries": 10, "retry_delay_ms": 500,
                  "error_status": 529, "error": "overloaded", "session_id": session})
        emit({"type": "system", "subtype": "model_fallback", "trigger": "overloaded", "original_model": "claude-test-9",
              "fallback_model": "claude-test-8", "content": "Opus is busy, so this turn uses Sonnet.", "session_id": session})
        say("Recovered fine.")
        result("Recovered fine.")
    elif "quiet api error" in last:
        # Its request failed, said as a message of its own, and the result doesn't say so.
        emit({"type": "assistant", "parent_tool_use_id": None, "error": "server_error",
              "message": {"id": "msg_err", "model": "<synthetic>", "content": [{"type": "text", "text": "API Error: 500 Internal server error"}]}})
        result("")
    elif "say nothing" in last:
        result("")
    elif "show the cost" in last:
        result("Total cost: $0.12")
    elif "odd requests" in last:
        # Requests Tessera doesn't serve still get an answer: it would wait forever otherwise.
        emit({"type": "control_request", "request_id": "hook-odd", "request": {"subtype": "hook_callback", "callback_id": "nope", "input": {"hook_event_name": "Notification"}}})
        h = wait_for(lambda m: m.get("type") == "control_response" and m["response"]["request_id"] == "hook-odd")
        other = mcp("tools/list", server="elsewhere")
        emit({"type": "control_request", "request_id": "odd-1", "request": {"subtype": "brand_new_request"}})
        o = wait_for(lambda m: m.get("type") == "control_response" and m["response"]["request_id"] == "odd-1")
        say("Answered: hook %s, other server %s, new request %s." % (h and h["response"]["subtype"], "refused" if other is None else "served", o and o["response"]["subtype"]))
        result("ok")
    else:
        say("Echo: " + text.strip()[-60:] + (" saw-digest" if "[Earlier in this chat" in full else ""))
        result("ok")
"#;

struct Setup {
    engine: Engine,
    _mock: Mock,
    home: tempfile::TempDir,
    fake: tempfile::TempDir,
    work: PathBuf,
    _work_dir: tempfile::TempDir,
}

async fn setup() -> Setup {
    setup_with("").await
}

async fn setup_with(extra: &str) -> Setup {
    let home = tempfile::tempdir().unwrap();
    let fake = tempfile::tempdir().unwrap();
    setup_in(home, fake, extra).await
}

async fn setup_in(home: tempfile::TempDir, fake: tempfile::TempDir, extra: &str) -> Setup {
    let mock = Mock::start(vec![Reply::Text("hello from the other model".into())]).await;
    let claude = fake.path().join("claude");
    std::fs::write(&claude, FAKE).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let work_dir = tempfile::tempdir().unwrap();
    let work = work_dir.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    // Tests that set `[agent]` themselves say how often memory is reviewed.
    // Each turn starts its own `claude` unless a test keeps processes open.
    let keep = if extra.contains("claude_keep_open_minutes") {
        ""
    } else {
        "claude_keep_open_minutes = 0\n"
    };
    let (agent, extra) = if extra.contains("[agent]\n") {
        (
            String::new(),
            extra.replacen("[agent]\n", &format!("[agent]\n{keep}"), 1),
        )
    } else {
        (
            format!("[agent]\nreview_interval = 0\n{keep}"),
            extra.to_string(),
        )
    };
    let config = format!(
        r#"default_model = "claude-code/opus"
{agent}[tuning."*"]
small = "off"
check = false
[[provider]]
id = "mock"
name = "Mock"
kind = "openai_chat"
base_url = "{}"
discover = false
[[provider.models]]
id = "m"
context_window = 100000
[[provider]]
id = "claude-code"
name = "Claude (your subscription)"
kind = "claude_code"
base_url = "{}"
[[provider.models]]
id = "opus"
{extra}"#,
        mock.url,
        claude.display()
    );
    std::fs::write(home.path().join("config.toml"), config).unwrap();
    let engine = Engine::open(Paths::at(home.path()), EngineOptions { scheduler: false })
        .await
        .unwrap();
    Setup {
        engine,
        _mock: mock,
        home,
        fake,
        work,
        _work_dir: work_dir,
    }
}

impl Setup {
    async fn call<T: serde::de::DeserializeOwned>(&self, req: Request) -> T {
        serde_json::from_value(self.engine.handle(req, None).await.unwrap()).unwrap()
    }

    async fn chat(&self, permission: PermissionMode) -> SessionInfo {
        let s: SessionInfo = self
            .call(Request::CreateSession {
                cwd: Some(self.work.display().to_string()),
                model: Some(ModelRef {
                    provider: "claude-code".into(),
                    model: "opus".into(),
                }),
                title: None,
                bot_id: None,
                private: false,
                lean: false,
            })
            .await;
        let mut u = update(&s.id);
        if let Request::UpdateSession { permission: p, .. } = &mut u {
            *p = Some(permission);
        }
        self.call(u).await
    }

    async fn send(&self, id: &str, text: &str) {
        self.engine
            .handle(
                Request::Send {
                    session_id: id.into(),
                    content: vec![ContentPart::text(text)],
                    delivery: Delivery::Auto,
                    attachments: Vec::new(),
                },
                None,
            )
            .await
            .unwrap();
    }

    async fn settle(&self, id: &str) {
        tokio::time::sleep(Duration::from_millis(50)).await;
        if tokio::time::timeout(Duration::from_secs(20), self.engine.wait_idle(id))
            .await
            .is_err()
        {
            let snap = self.snapshot(id).await;
            panic!(
                "the chat did not finish: {:?}\n{:#?}\n{}",
                snap.info.status,
                self.runs(),
                serde_json::to_string(&snap.entries).unwrap()
            );
        }
    }

    async fn snapshot(&self, id: &str) -> SessionSnapshot {
        self.call(Request::GetSession {
            session_id: id.into(),
            since_seq: None,
        })
        .await
    }

    /// How often `claude` was started at all.
    fn started(&self) -> usize {
        self.calls()
            .iter()
            .filter(|c| c["kind"] == "started")
            .count()
    }

    /// What the stand-in was started with, and the messages it got.
    fn calls(&self) -> Vec<Value> {
        std::fs::read_to_string(self.fake.path().join("calls.jsonl"))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect()
    }

    fn turn_args(&self) -> Vec<Value> {
        self.logged("args")
            .into_iter()
            .filter(|a| {
                a.as_array()
                    .is_some_and(|a| a.iter().any(|x| x == "stream-json"))
            })
            .collect()
    }
    /// The turns it ran (not Tessera's one-shot requests, e.g. titles).
    fn runs(&self) -> Vec<String> {
        self.all_runs()
            .into_iter()
            .filter(|r| !r.contains("--output-format json"))
            .collect()
    }

    fn all_runs(&self) -> Vec<String> {
        self.calls()
            .iter()
            .filter(|c| c["kind"] == "args")
            .map(|c| {
                c["value"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|a| a.as_str().unwrap().to_string())
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect()
    }

    fn prompts(&self) -> Vec<String> {
        self.calls()
            .iter()
            .filter(|c| c["kind"] == "in" && c["value"]["type"] == "user")
            .map(|c| c["value"]["message"]["content"].to_string())
            .collect()
    }
}

fn update(session_id: &str) -> Request {
    Request::UpdateSession {
        session_id: session_id.into(),
        title: None,
        model: None,
        thinking: None,
        permission: None,
        pinned: None,
        archived: None,
        cwd: None,
        members: None,
        fast: None,
        subagents: None,
    }
}

fn texts(entries: &[Entry]) -> Vec<String> {
    entries
        .iter()
        .filter_map(|e| match &e.body {
            EntryBody::Assistant { content, .. } => Some(
                content
                    .iter()
                    .filter_map(|p| match p {
                        AssistantPart::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<String>(),
            ),
            _ => None,
        })
        .filter(|t| !t.is_empty())
        .collect()
}

async fn approval(rx: &mut tokio::sync::broadcast::Receiver<Event>) -> ApprovalRequest {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if let Ok(Event::ApprovalRequested { request, .. }) = rx.recv().await {
                return request;
            }
        }
    })
    .await
    .expect("no approval was requested")
}

#[tokio::test]
async fn it_is_found_and_listed_as_a_subscription() {
    let s = setup().await;
    let info: ClaudeCodeInfo = s
        .call(Request::ClaudeCodeStatus {
            refresh: true,
            provider: None,
        })
        .await;
    assert!(info.installed && info.signed_in && !info.api_key);
    assert_eq!(info.version.as_deref(), Some("9.9.9"));
    assert_eq!(info.plan.as_deref(), Some("max"));
    assert!(info.hint.starts_with("Ready"), "{}", info.hint);
    let providers: Vec<ProviderInfo> = s.call(Request::ListProviders { refresh: true }).await;
    let p = providers.iter().find(|p| p.id == "claude-code").unwrap();
    assert!(p.authenticated);
    assert_eq!(p.kind, ProviderKind::ClaudeCode);
    assert_eq!(p.auth, AuthMethod::Oauth);
    assert_eq!(p.account.as_deref(), Some("ann@example.com"));
    assert!(p.models.iter().any(|m| m.id == "opus"));
    // Signing in is Claude Code's business: Tessera only points the way.
    let e = s
        .engine
        .handle(
            Request::StartOauth {
                provider: "claude-code".into(),
            },
            None,
        )
        .await
        .unwrap_err();
    assert!(e.message.contains("Ready") || e.message.contains("claude"));
    // Signed out: still listed, not usable, and the hint says what to do.
    std::fs::write(s.fake.path().join("signed_out"), "").unwrap();
    let info: ClaudeCodeInfo = s
        .call(Request::ClaudeCodeStatus {
            refresh: true,
            provider: None,
        })
        .await;
    assert!(info.installed && !info.signed_in);
    assert!(info.hint.contains("`claude`"), "{}", info.hint);
    let providers: Vec<ProviderInfo> = s.call(Request::ListProviders { refresh: false }).await;
    assert!(
        !providers
            .iter()
            .find(|p| p.id == "claude-code")
            .unwrap()
            .authenticated
    );
}

#[tokio::test]
async fn its_permission_prompts_are_tessera_approvals() {
    let s = setup().await;
    let chat = s.chat(PermissionMode::Ask).await;
    let mut rx = s.engine.subscribe();
    s.send(&chat.id, "please run a command").await;
    let req = approval(&mut rx).await;
    assert_eq!(req.tool, "Bash");
    assert_eq!(req.call_id, "toolu_1");
    assert_eq!(req.summary, "echo hi");
    // The tool card it belongs to is already in the transcript.
    let snap = s.snapshot(&chat.id).await;
    assert_eq!(snap.info.status, SessionStatus::AwaitingApproval);
    assert!(snap.entries.iter().any(|e| matches!(&e.body,
        EntryBody::Assistant { content, .. } if content.iter().any(|p| matches!(p, AssistantPart::ToolCall { id, .. } if id == "toolu_1")))));
    s.engine
        .handle(
            Request::RespondApproval {
                session_id: chat.id.clone(),
                request_id: req.id,
                decision: ApprovalDecision::Allow,
            },
            None,
        )
        .await
        .unwrap();
    s.settle(&chat.id).await;
    let snap = s.snapshot(&chat.id).await;
    assert_eq!(snap.info.status, SessionStatus::Idle);
    assert_eq!(texts(&snap.entries), vec!["Running it.", "Done: allowed"]);
    let results = tool_results(&snap.entries);
    assert_eq!(results, vec![("Bash".to_string(), false, "hi".to_string())]);
    // The command line: its own permission checks on, prompts over stdio.
    let runs = s.runs();
    assert_eq!(runs.len(), 1);
    assert!(
        runs[0].contains("--permission-prompt-tool stdio --permission-mode default --model opus")
    );
    assert!(!runs[0].contains("--resume") && !runs[0].contains("dangerously"));
    // Usage: the session's tokens, and the API-equivalent cost it reported
    // under the concrete model, billed as a subscription.
    assert_eq!(snap.info.usage.input, 100);
    assert_eq!(snap.info.usage.cache_read, 1000);
    assert_eq!(snap.info.usage.cost_usd, None);
    let report: UsageReport = s
        .call(Request::GetUsage {
            from: None,
            to: None,
            utc_offsets: Vec::new(),
        })
        .await;
    let m = report
        .by_model
        .iter()
        .find(|m| m.provider == "claude-code")
        .expect("no claude-code usage");
    assert_eq!(m.model, "claude-test-9");
    assert_eq!(m.billing, Some(Billing::Subscription));
    assert!((m.totals.api_cost_usd.unwrap() - 0.0123).abs() < 1e-9);
    assert_eq!(m.totals.cost_usd, None);
    // Its own model list replaces the aliases.
    let providers: Vec<ProviderInfo> = s.call(Request::ListProviders { refresh: false }).await;
    let p = providers.iter().find(|p| p.id == "claude-code").unwrap();
    let opus = p.models.iter().find(|m| m.id == "opus").unwrap();
    assert_eq!(opus.name, "Opus 9");
    assert!(p.models.iter().all(|m| m.id != "default"));

    // The next message continues the same Claude Code conversation.
    s.send(&chat.id, "thanks").await;
    s.settle(&chat.id).await;
    let runs = s.runs();
    assert!(runs[1].contains("--resume sess-"), "{}", runs[1]);
    assert!(!s.prompts()[1].contains("Earlier in this chat"));

    // Claude Code lost that conversation: a new one starts, told what
    // happened so far.
    std::fs::write(s.fake.path().join("forget"), "").unwrap();
    s.send(&chat.id, "still there?").await;
    s.settle(&chat.id).await;
    let runs = s.runs();
    assert_eq!(runs.len(), 4);
    assert!(runs[2].contains("--resume") && !runs[3].contains("--resume"));
    let prompt = s.prompts().last().unwrap().clone();
    assert!(
        prompt.contains("Earlier in this chat") && prompt.contains("Done: allowed"),
        "{prompt}"
    );
    let snap = s.snapshot(&chat.id).await;
    assert_eq!(snap.info.status, SessionStatus::Idle);
    assert!(texts(&snap.entries).last().unwrap().ends_with("saw-digest"));
}

#[tokio::test]
async fn usage_counts_once_and_the_window_is_the_chat_models() {
    let s = setup().await;
    let chat = s.chat(PermissionMode::Ask).await;
    s.send(&chat.id, "tally one").await;
    tokio::time::sleep(Duration::from_millis(250)).await;
    // Queued while it works: the same run answers it, with running totals.
    s.send(&chat.id, "tally two").await;
    s.settle(&chat.id).await;
    assert_eq!(s.runs().len(), 1, "one run answered both");
    let snap = s.snapshot(&chat.id).await;
    assert_eq!(texts(&snap.entries), vec!["Tally 1.", "Tally 2."]);
    // Every model's tokens once: 2 × (100 + 7) in, 2 × (20 + 1) out.
    assert_eq!((snap.info.usage.input, snap.info.usage.output), (214, 42));
    // The window is the chat's model's (1M), not the small model's (200K, first in the map).
    assert_eq!(snap.info.context.window, 1_000_000);
    let report: UsageReport = s
        .call(Request::GetUsage {
            from: None,
            to: None,
            utc_offsets: Vec::new(),
        })
        .await;
    let by = |id: &str| {
        report
            .by_model
            .iter()
            .find(|m| m.provider == "claude-code" && m.model == id)
            .unwrap_or_else(|| panic!("no usage for {id}: {:?}", report.by_model))
            .totals
            .clone()
    };
    assert_eq!(
        (by("claude-test-9").input, by("claude-test-9").output),
        (200, 40)
    );
    assert_eq!(
        (by("claude-haiku-8").input, by("claude-haiku-8").output),
        (14, 2)
    );
    assert!((by("claude-test-9").api_cost_usd.unwrap() - 0.02).abs() < 1e-9);
}

#[tokio::test]
async fn signed_in_with_an_api_key_it_is_billed_per_token() {
    let s = setup().await;
    std::fs::write(s.fake.path().join("api_key"), "").unwrap();
    let info: ClaudeCodeInfo = s
        .call(Request::ClaudeCodeStatus {
            refresh: true,
            provider: None,
        })
        .await;
    assert!(info.signed_in && info.api_key);
    let chat = s.chat(PermissionMode::Ask).await;
    s.send(&chat.id, "hello").await;
    s.settle(&chat.id).await;
    let report: UsageReport = s
        .call(Request::GetUsage {
            from: None,
            to: None,
            utc_offsets: Vec::new(),
        })
        .await;
    let m = report
        .by_model
        .iter()
        .find(|m| m.provider == "claude-code" && m.model == "claude-test-9")
        .expect("no claude-code usage");
    // Not "in plan": per token, at the cost Claude Code reported.
    assert_eq!(m.billing, Some(Billing::Api));
    assert!((m.totals.api_cost_usd.unwrap() - 0.0123).abs() < 1e-9);
}

#[tokio::test]
async fn declined_yolo_and_read_only() {
    let s = setup().await;
    let chat = s.chat(PermissionMode::Ask).await;
    let mut rx = s.engine.subscribe();
    s.send(&chat.id, "run a command").await;
    let req = approval(&mut rx).await;
    s.engine
        .handle(
            Request::RespondApproval {
                session_id: chat.id.clone(),
                request_id: req.id,
                decision: ApprovalDecision::Deny,
            },
            None,
        )
        .await
        .unwrap();
    s.settle(&chat.id).await;
    let snap = s.snapshot(&chat.id).await;
    assert!(!snap.entries.iter().any(|e| matches!(&e.body,
        EntryBody::Notice { text, .. } if text == "Stopped.")));
    assert_eq!(texts(&snap.entries).last().unwrap(), "Done: denied");
    assert!(
        !s.calls()
            .iter()
            .any(|c| c["value"]["request"]["subtype"] == "interrupt")
    );
    let answer = s
        .calls()
        .into_iter()
        .find(|c| {
            c["value"]["type"] == "control_response"
                && c["value"]["response"]["request_id"]
                    .as_str()
                    .is_some_and(|r| r.starts_with("perm-"))
        })
        .unwrap();
    assert_eq!(answer["value"]["response"]["response"]["behavior"], "deny");
    assert_eq!(answer["value"]["response"]["response"]["interrupt"], false);
    assert!(
        answer["value"]["response"]["response"]["message"]
            .as_str()
            .unwrap()
            .contains("declined this Bash action")
    );
    // YOLO: nothing asks.
    let yolo = s.chat(PermissionMode::Auto).await;
    s.send(&yolo.id, "run a command").await;
    s.settle(&yolo.id).await;
    let snap = s.snapshot(&yolo.id).await;
    assert_eq!(texts(&snap.entries).last().unwrap(), "Done: allowed");
    assert!(
        s.runs()
            .last()
            .unwrap()
            .contains("--permission-mode acceptEdits")
    );

    // Read-only: it plans, and anything it asks to do is refused.
    let ro = s.chat(PermissionMode::ReadOnly).await;
    s.send(&ro.id, "run a command").await;
    s.settle(&ro.id).await;
    let snap = s.snapshot(&ro.id).await;
    assert_eq!(texts(&snap.entries).last().unwrap(), "Done: denied");
    assert!(s.runs().last().unwrap().contains("--permission-mode plan"));
    while let Ok(ev) = rx.try_recv() {
        if let Event::ApprovalRequested { session_id, .. } = ev {
            assert_eq!(session_id, chat.id, "only the ask-mode chat asks");
        }
    }
}

#[tokio::test]
async fn stop_while_approval_is_open_interrupts_the_cli() {
    let s = setup().await;
    let chat = s.chat(PermissionMode::Ask).await;
    let mut rx = s.engine.subscribe();
    s.send(&chat.id, "run a command").await;
    let _ = approval(&mut rx).await;
    s.call::<Value>(Request::Abort {
        session_id: chat.id.clone(),
    })
    .await;
    s.settle(&chat.id).await;
    assert!(
        s.calls()
            .iter()
            .any(|c| c["value"]["request"]["subtype"] == "interrupt")
    );
}

#[tokio::test]
async fn own_read_chains_ask_and_secret_and_data_guards_hold() {
    let s = setup_with("[security]\nsandbox = 'off'\n").await;
    let chat = s.chat(PermissionMode::Ask).await;
    let mut rx = s.engine.subscribe();
    s.send(&chat.id, &builtin_actions(json!([
        {"tool":"Bash", "input":{"command":"cat own-file | head; tail own-log | grep done | wc -l; ls && sha256sum own-file"}},
        {"tool":"Bash", "input":{"command":format!("cat '{}/config.toml' | head", s.home.path().display())}},
        {"tool":"Bash", "input":{"command":"cat .env.production | head"}},
        {"tool":"Bash", "input":{"command":"cat .credentials.json"}}
    ]))).await;
    decide(
        &s,
        &chat.id,
        approval(&mut rx).await,
        ApprovalDecision::Deny,
    )
    .await;
    s.settle(&chat.id).await;
    no_extra_approvals(&mut rx);
    assert!(s.logged("executed").is_empty());
    assert_eq!(
        tool_results(&s.snapshot(&chat.id).await.entries)
            .iter()
            .filter(|r| r.1)
            .count(),
        4
    );
}
#[tokio::test]
async fn stop_interrupts_it_and_failures_are_explained() {
    let s = setup().await;
    let chat = s.chat(PermissionMode::Default).await;
    let mut rx = s.engine.subscribe();
    s.send(&chat.id, "hang for a while").await;
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if let Ok(Event::StreamDelta { .. }) = rx.recv().await {
                return;
            }
        }
    })
    .await
    .expect("it never started");
    s.call::<Value>(Request::Abort {
        session_id: chat.id.clone(),
    })
    .await;
    s.settle(&chat.id).await;
    let snap = s.snapshot(&chat.id).await;
    assert_eq!(snap.info.status, SessionStatus::Idle);
    assert!(matches!(&snap.entries.last().unwrap().body,
        EntryBody::Notice { text, .. } if text == "Stopped."));
    assert!(
        s.calls()
            .iter()
            .any(|c| c["value"]["request"]["subtype"] == "interrupt")
    );

    s.send(&chat.id, "fail login").await;
    s.settle(&chat.id).await;
    let snap = s.snapshot(&chat.id).await;
    assert_eq!(snap.info.status, SessionStatus::Error);
    let EntryBody::Notice { text, .. } = &snap.entries.last().unwrap().body else {
        panic!("no error notice");
    };
    assert!(
        text.contains("/login") && text.contains("sign in"),
        "{text}"
    );
}

#[tokio::test]
async fn stopping_mid_sentence_keeps_what_was_said() {
    let s = setup().await;
    let chat = s.chat(PermissionMode::Default).await;
    let mut rx = s.engine.subscribe();
    s.send(&chat.id, "trail off please").await;
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if let Ok(Event::StreamDelta { delta, .. }) = rx.recv().await
                && format!("{delta:?}").contains("Halfway")
            {
                return;
            }
        }
    })
    .await
    .expect("it never got halfway");
    s.call::<Value>(Request::Abort {
        session_id: chat.id.clone(),
    })
    .await;
    s.settle(&chat.id).await;
    let said: Vec<(String, StopReason)> = s
        .snapshot(&chat.id)
        .await
        .entries
        .into_iter()
        .filter_map(|e| match e.body {
            EntryBody::Assistant { content, stop, .. } => Some((
                content
                    .iter()
                    .filter_map(|p| match p {
                        AssistantPart::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect(),
                stop,
            )),
            _ => None,
        })
        .collect();
    assert_eq!(
        said,
        vec![
            ("First, the plan.".to_string(), StopReason::Stop),
            ("Halfway through a sent".to_string(), StopReason::Aborted),
        ]
    );
}

#[tokio::test]
async fn edits_show_diffs_and_another_models_turns_come_along() {
    let s = setup().await;
    let chat = s.chat(PermissionMode::Default).await;
    s.send(&chat.id, "edit a file").await;
    s.settle(&chat.id).await;
    let snap = s.snapshot(&chat.id).await;
    let details = snap
        .entries
        .iter()
        .find_map(|e| match &e.body {
            EntryBody::ToolResult { details, .. } => details.clone(),
            _ => None,
        })
        .unwrap();
    assert_eq!(details["path"], "a.txt");
    assert!(details["diff"].as_str().unwrap().contains("-old\n+new"));

    // Another model answers a turn; back on Claude Code, it is told.
    let mut u = update(&chat.id);
    if let Request::UpdateSession { model, .. } = &mut u {
        *model = Some(ModelRef {
            provider: "mock".into(),
            model: "m".into(),
        });
    }
    s.call::<SessionInfo>(u).await;
    s.send(&chat.id, "a question for the other model").await;
    s.settle(&chat.id).await;
    let mut u = update(&chat.id);
    if let Request::UpdateSession { model, .. } = &mut u {
        *model = Some(ModelRef {
            provider: "claude-code".into(),
            model: "opus".into(),
        });
    }
    s.call::<SessionInfo>(u).await;
    s.send(&chat.id, "and back").await;
    s.settle(&chat.id).await;
    let last = s.prompts().last().unwrap().clone();
    assert!(last.contains("hello from the other model"), "{last}");
    assert!(
        !last.contains("edit a file"),
        "it saw that turn itself: {last}"
    );
    let snap = s.snapshot(&chat.id).await;
    assert!(texts(&snap.entries).last().unwrap().ends_with("saw-digest"));
    assert!(s.runs().last().unwrap().contains("--resume"));
}

#[tokio::test]
async fn it_runs_in_the_command_sandbox() {
    if !tessera_core::sandbox::support().available {
        return;
    }
    let s = setup().await;
    let secret: &Path = &s.home.path().join("auth.json");
    if !secret.exists() {
        std::fs::write(secret, "{}").unwrap();
    }
    let chat = s.chat(PermissionMode::Default).await;
    s.send(&chat.id, &format!("read {}", secret.display()))
        .await;
    s.settle(&chat.id).await;
    let snap = s.snapshot(&chat.id).await;
    assert_eq!(
        texts(&snap.entries).last().unwrap(),
        "The file is hidden.",
        "Claude Code could read Tessera's credentials"
    );
    // Files in the chat's own folder are as usual.
    std::fs::write(s.work.join("notes.txt"), "x").unwrap();
    s.send(
        &chat.id,
        &format!("read {}", s.work.join("notes.txt").display()),
    )
    .await;
    s.settle(&chat.id).await;
    let snap = s.snapshot(&chat.id).await;
    assert_eq!(texts(&snap.entries).last().unwrap(), "The file is visible.");
    // Without Landlock 2 (`TESSERA_TEST_LANDLOCK_ABI=1`) it shares Tessera's
    // private process namespace: the user's other processes aren't there.
    if tessera_core::sandbox::describe().contains("private process view") {
        s.send(&chat.id, "count processes").await;
        s.settle(&chat.id).await;
        let snap = s.snapshot(&chat.id).await;
        let last = texts(&snap.entries).pop().unwrap();
        let n: usize = last.split(' ').next().unwrap().parse().unwrap();
        assert!(n < 30, "it sees the host's processes: {last}");
    }
}

#[tokio::test]
async fn private_chats_keep_no_history_and_only_browse() {
    let s = setup().await;
    let chat: SessionInfo = s
        .call(Request::CreateSession {
            cwd: None,
            model: Some(ModelRef {
                provider: "claude-code".into(),
                model: "opus".into(),
            }),
            title: None,
            bot_id: None,
            private: true,
            lean: false,
        })
        .await;
    s.send(&chat.id, "hello there").await;
    s.settle(&chat.id).await;
    s.send(&chat.id, "and again").await;
    s.settle(&chat.id).await;
    let runs = s.runs();
    assert_eq!(runs.len(), 2);
    for r in &runs {
        assert!(
            r.contains("--no-session-persistence --tools WebSearch,WebFetch --strict-mcp-config"),
            "{r}"
        );
        assert!(!r.contains("--resume"), "{r}");
    }
    // Each turn brings the conversation so far along instead.
    let last = s.prompts().last().unwrap().clone();
    assert!(
        last.contains("hello there") && last.contains("Echo: hello there"),
        "{last}"
    );
    let snap = s.snapshot(&chat.id).await;
    assert!(texts(&snap.entries).last().unwrap().ends_with("saw-digest"));
    // Nothing of it is left in the usage stats.
    let report: UsageReport = s
        .call(Request::GetUsage {
            from: None,
            to: None,
            utc_offsets: Vec::new(),
        })
        .await;
    assert!(report.by_model.is_empty());
}

#[tokio::test]
async fn edits_elsewhere_follow_the_folder_settings() {
    let open = tempfile::tempdir().unwrap();
    let locked = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    let s = setup_with(&format!(
        "[security]\nfolders = [{{ path = \"{}\", write = true }}, {{ path = \"{}\", write = false }}]\n",
        open.path().display(),
        locked.path().display()
    ))
    .await;
    let chat = s.chat(PermissionMode::Default).await;
    let mut rx = s.engine.subscribe();
    s.send(
        &chat.id,
        &format!("write to {}/a.txt", open.path().display()),
    )
    .await;
    s.settle(&chat.id).await;
    s.send(
        &chat.id,
        &format!("write to {}/a.txt", locked.path().display()),
    )
    .await;
    s.settle(&chat.id).await;
    let snap = s.snapshot(&chat.id).await;
    let t = texts(&snap.entries);
    assert_eq!(t[t.len() - 3..], ["Wrote.", "Writing.", "Not written."]);
    while let Ok(ev) = rx.try_recv() {
        assert!(
            !matches!(ev, Event::ApprovalRequested { .. }),
            "nothing asked"
        );
    }
    // Anywhere else asks first.
    s.send(
        &chat.id,
        &format!("write to {}/a.txt", other.path().display()),
    )
    .await;
    let req = approval(&mut rx).await;
    assert_eq!(req.tool, "Write");
    s.engine
        .handle(
            Request::RespondApproval {
                session_id: chat.id.clone(),
                request_id: req.id,
                decision: ApprovalDecision::Deny,
            },
            None,
        )
        .await
        .unwrap();
    s.settle(&chat.id).await;
    // Claude Code's own settings (hooks, permission rules) ask even in a
    // folder agents may change: they decide what runs without asking.
    s.send(
        &chat.id,
        &format!("write to {}/.claude/settings.json", open.path().display()),
    )
    .await;
    let req = approval(&mut rx).await;
    assert!(
        req.reason.contains("sensitive location") && req.reason.contains(".claude/settings.json"),
        "{req:?}"
    );
    s.engine
        .handle(
            Request::RespondApproval {
                session_id: chat.id.clone(),
                request_id: req.id,
                decision: ApprovalDecision::Deny,
            },
            None,
        )
        .await
        .unwrap();
    s.settle(&chat.id).await;
}

#[tokio::test]
async fn claude_runs_only_when_something_asks() {
    let s = setup().await;
    // Start-up and a plain model list leave it alone; it isn't listed yet.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let providers: Vec<ProviderInfo> = s.call(Request::ListProviders { refresh: false }).await;
    assert!(providers.iter().all(|p| p.id != "claude-code"));
    assert_eq!(s.started(), 0, "claude ran without being asked");
    // Asking about it runs it once; the answer is kept.
    let info: ClaudeCodeInfo = s
        .call(Request::ClaudeCodeStatus {
            refresh: false,
            provider: None,
        })
        .await;
    assert!(info.signed_in);
    let ran = s.started();
    assert_eq!(ran, 2, "--version and auth status");
    let _: ClaudeCodeInfo = s
        .call(Request::ClaudeCodeStatus {
            refresh: false,
            provider: None,
        })
        .await;
    let providers: Vec<ProviderInfo> = s.call(Request::ListProviders { refresh: false }).await;
    assert!(
        providers
            .iter()
            .any(|p| p.id == "claude-code" && p.authenticated)
    );
    assert_eq!(s.started(), ran);
    // After a restart it is still known, and still nothing runs `claude`.
    let Setup {
        engine, home, fake, ..
    } = s;
    drop(engine);
    let s = setup_in(home, fake, "").await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let providers: Vec<ProviderInfo> = s.call(Request::ListProviders { refresh: false }).await;
    assert!(
        providers
            .iter()
            .any(|p| p.id == "claude-code" && p.authenticated)
    );
    assert_eq!(s.started(), ran);
    // A refreshed list asks again.
    let _: Vec<ProviderInfo> = s.call(Request::ListProviders { refresh: true }).await;
    assert_eq!(s.started(), ran + 2);
}

// ---- Tessera's tools in Claude Code turns (the bridge) ----------------------------

impl Setup {
    /// What the stand-in logged under `kind`, in order.
    fn logged(&self, kind: &str) -> Vec<Value> {
        self.calls()
            .into_iter()
            .filter(|c| c["kind"] == kind)
            .map(|c| c["value"].clone())
            .collect()
    }

    /// The value after `flag` in the last turn's arguments.
    fn last_arg(&self, flag: &str) -> Option<String> {
        let a: Vec<String> = self
            .logged("args")
            .into_iter()
            .filter_map(|v| serde_json::from_value::<Vec<String>>(v).ok())
            .rfind(|a| {
                !a.windows(2)
                    .any(|w| w[0] == "--output-format" && w[1] == "json")
            })?;
        a.iter()
            .position(|x| x == flag)
            .and_then(|i| a.get(i + 1).cloned())
    }

    async fn session_on(&self, model: &str, bot: Option<&str>, private: bool) -> SessionInfo {
        self.call(Request::CreateSession {
            cwd: (!private).then(|| self.work.display().to_string()),
            model: Some(model.parse().unwrap()),
            title: None,
            bot_id: bot.map(str::to_string),
            private,
            lean: false,
        })
        .await
    }

    async fn save_bot(&self, name: &str, model: &str, tools: ToolPreset) -> Bot {
        self.call(Request::SaveBot {
            bot: Bot {
                id: String::new(),
                name: name.into(),
                handle: String::new(),
                avatar: String::new(),
                color: String::new(),
                role: format!("{name}'s job"),
                personality: format!("{name} speaks in short sentences."),
                instructions: format!("{name} keeps the lab tidy."),
                model: Some(model.parse().unwrap()),
                thinking: None,
                permission: PermissionMode::Default,
                tools,
                allowed_tools: vec![],
                cwd: Some(self.work.display().to_string()),
                own_memory: true,
                share_user_profile: true,
                can_message: vec![],
                askable: None,
                knowledge: Vec::new(),
                starters: Vec::new(),
                machines: None,
                secrets: None,
                created_at: 0,
                updated_at: 0,
            },
        })
        .await
    }
}

/// The tool calls in the transcript, by name.
fn tool_calls(entries: &[Entry]) -> Vec<String> {
    entries
        .iter()
        .filter_map(|e| match &e.body {
            EntryBody::Assistant { content, .. } => Some(content),
            _ => None,
        })
        .flatten()
        .filter_map(|p| match p {
            AssistantPart::ToolCall { name, .. } => Some(name.clone()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn tessera_tools_work_in_claude_code_turns() {
    let s = setup().await;
    let chat = s.chat(PermissionMode::Default).await;
    s.send(
        &chat.id,
        r#"tessera note {"action":"create","title":"Groceries","content":"milk and eggs"}"#,
    )
    .await;
    s.settle(&chat.id).await;
    // Tessera's tools come over the turn's own pipe (an in-process MCP
    // server), allowed in Claude Code so that Tessera alone asks; only
    // Tessera's MCP is available, and its context comes in a file of its own.
    let cfg: Value = serde_json::from_str(&s.last_arg("--mcp-config").unwrap()).unwrap();
    assert_eq!(cfg["mcpServers"]["tessera"]["type"], "sdk");
    assert_eq!(
        s.last_arg("--allowedTools").as_deref(),
        Some("mcp__tessera,Grep,Glob")
    );
    let run = s.runs().pop().unwrap();
    assert!(!run.contains("--strict-mcp-config"), "{run}");
    let ctx_file = s.last_arg("--append-system-prompt-file").unwrap();
    assert!(
        !Path::new(&ctx_file).exists(),
        "the context file outlived its turn"
    );
    let ctx = s.logged("context").pop().unwrap();
    let ctx = ctx.as_str().unwrap();
    assert!(
        ctx.starts_with("# Tessera") && ctx.contains("mcp__tessera__"),
        "{ctx}"
    );
    assert!(
        ctx.contains("descriptions give actions") && ctx.contains("never put credentials"),
        "{ctx}"
    );
    // Native counterparts share tagged files and the reader; conditional
    // search/LSP stay out unless configured.
    let tools: Vec<String> = serde_json::from_value(s.logged("tools").pop().unwrap()).unwrap();
    for t in [
        "note",
        "whiteboard",
        "memory",
        "notify",
        "share_file",
        "docs",
        "recall",
        "skill",
        "agent",
        "agents",
        "save_secret",
        "read",
        "grep",
        "bash",
        "job",
        "write",
        "edit",
        "glob",
        "web_fetch",
    ] {
        assert!(tools.contains(&t.to_string()), "{t} missing from {tools:?}");
    }
    for t in ["web_search", "lsp"] {
        assert!(!tools.contains(&t.to_string()), "{t} offered: {tools:?}");
    }
    // Nothing of Tessera's own settings or keys in its environment.
    let env: Vec<String> = serde_json::from_value(s.logged("env").pop().unwrap()).unwrap();
    assert!(!env.iter().any(|k| k == "TESSERA_HOME"), "{env:?}");
    // The note was made, as by Tessera's own agent.
    let notes: Vec<NoteInfo> = s.call(Request::ListNotes).await;
    assert!(notes.iter().any(|n| n.title == "Groceries"), "{notes:?}");
    let called = s.logged("called").pop().unwrap();
    assert_eq!(called["error"], false, "{called}");
    // The chat shows it as Tessera's own note tool, not `mcp__…`.
    let snap = s.snapshot(&chat.id).await;
    assert_eq!(tool_calls(&snap.entries), vec!["note"]);
    let results = tool_results(&snap.entries);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].0, "note");
    assert!(!results[0].1);
    assert!(
        texts(&snap.entries)
            .last()
            .unwrap()
            .starts_with("Tool said:")
    );
    assert_eq!(snap.info.status, SessionStatus::Idle);

    // A whiteboard's card carries the board, like any other chat's.
    s.send(
        &chat.id,
        r##"tessera whiteboard {"action":"show","title":"Plan","markdown":"# Steps"}"##,
    )
    .await;
    s.settle(&chat.id).await;
    let snap = s.snapshot(&chat.id).await;
    let details = snap
        .entries
        .iter()
        .rev()
        .find_map(|e| match &e.body {
            EntryBody::ToolResult { tool, details, .. } if tool == "whiteboard" => details.clone(),
            _ => None,
        })
        .expect("no whiteboard result");
    assert_eq!(details["board"]["title"], "Plan");
    // And a notification goes out through Tessera.
    s.send(
        &chat.id,
        r#"tessera notify {"title":"Done","body":"All set"}"#,
    )
    .await;
    s.settle(&chat.id).await;
    let called = s.logged("called").pop().unwrap();
    assert_eq!(called["name"], "notify");
    assert_eq!(called["error"], false, "{called}");
}

#[tokio::test]
async fn a_bridged_call_is_approved_once_in_tessera() {
    let s = setup().await;
    // Commands are offered without a vault; stored credentials still use
    // exactly the same native approval and injection path.
    s.call::<Value>(Request::SetSecret {
        name: "LAB_TOKEN".into(),
        value: "s3cret-value-123456".into(),
    })
    .await;
    let chat = s.chat(PermissionMode::Ask).await;
    let mut rx = s.engine.subscribe();
    s.send(
        &chat.id,
        r#"ask then tessera bash {"command":"echo hello"}"#,
    )
    .await;
    // Claude Code's own permission prompt for it is answered by Tessera at
    // once: the approval that counts is Tessera's, when the call runs.
    let req = approval(&mut rx).await;
    assert_eq!(req.tool, "bash");
    assert_eq!(req.call_id, "toolu_ask");
    assert_eq!(s.logged("asked"), vec![Value::Bool(true)]);
    s.engine
        .handle(
            Request::RespondApproval {
                session_id: chat.id.clone(),
                request_id: req.id,
                decision: ApprovalDecision::Allow,
            },
            None,
        )
        .await
        .unwrap();
    s.settle(&chat.id).await;
    let mut asked = 0;
    while let Ok(ev) = rx.try_recv() {
        if matches!(ev, Event::ApprovalRequested { .. }) {
            asked += 1;
        }
    }
    assert_eq!(asked, 0, "asked again");
    let called = s.logged("called").pop().unwrap();
    assert_eq!(called["error"], false, "{called}");
    assert!(
        called["text"].as_str().unwrap().contains("hello"),
        "{called}"
    );
    let snap = s.snapshot(&chat.id).await;
    assert_eq!(tool_results(&snap.entries)[0].0, "bash");
    assert!(texts(&snap.entries).last().unwrap().contains("hello"));
}

#[tokio::test]
async fn credentials_work_through_tessera_commands_but_never_show() {
    let s = setup().await;
    s.call::<Value>(Request::SetSecret {
        name: "LAB_TOKEN".into(),
        value: "s3cret-value-123456".into(),
    })
    .await;
    let chat = s.chat(PermissionMode::Auto).await;
    s.send(
        &chat.id,
        r#"tessera bash {"command":"test \"$LAB_TOKEN\" = s3cret-value-123456 && echo matched; echo $LAB_TOKEN"}"#,
    )
    .await;
    s.settle(&chat.id).await;
    let tools: Vec<String> = serde_json::from_value(s.logged("tools").pop().unwrap()).unwrap();
    assert!(tools.contains(&"bash".to_string()), "{tools:?}");
    // Its context lists the variable, never the value.
    let ctx = s
        .logged("context")
        .pop()
        .unwrap()
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        ctx.contains("$LAB_TOKEN") && !ctx.contains("s3cret"),
        "{ctx}"
    );
    let called = s.logged("called").pop().unwrap();
    let out = called["text"].as_str().unwrap();
    assert!(out.contains("matched"), "the command didn't get it: {out}");
    assert!(
        !out.contains("s3cret-value"),
        "the value reached Claude Code: {out}"
    );
    // Nor anything Tessera keeps.
    let snap = s.snapshot(&chat.id).await;
    assert!(
        !serde_json::to_string(&snap.entries)
            .unwrap()
            .contains("s3cret-value")
    );
}

#[tokio::test]
async fn bridged_calls_are_refused_as_tessera_refuses_them() {
    let s = setup().await;
    // The same command through Tessera's own agent (another model)…
    s._mock.route(
        "cat-native",
        Reply::Tools(vec![(
            "bash".into(),
            serde_json::json!({"command": "cat $TESSERA_HOME/auth.json"}),
        )]),
    );
    s._mock.route("cat-native", Reply::Text("ok".into()));
    s.call::<Value>(Request::SetSecret {
        name: "LAB_TOKEN".into(),
        value: "s3cret-value-123456".into(),
    })
    .await;
    let native: SessionInfo = s.session_on("mock/m", None, false).await;
    let mut u = update(&native.id);
    if let Request::UpdateSession { permission: p, .. } = &mut u {
        *p = Some(PermissionMode::Auto);
    }
    s.call::<SessionInfo>(u).await;
    s.send(&native.id, "cat-native please").await;
    s.settle(&native.id).await;
    let native_refusal = tool_results(&s.snapshot(&native.id).await.entries)[0].clone();
    assert!(native_refusal.1, "{native_refusal:?}");
    // …and through Claude Code: refused the same way.
    let chat = s.chat(PermissionMode::Auto).await;
    s.send(
        &chat.id,
        r#"tessera bash {"command":"cat $TESSERA_HOME/auth.json"}"#,
    )
    .await;
    s.settle(&chat.id).await;
    let called = s.logged("called").pop().unwrap();
    assert_eq!(called["error"], true);
    assert_eq!(called["text"].as_str().unwrap(), native_refusal.2);
    // A read-only chat isn't offered tools that change things, and a call
    // to one anyway is refused.
    let ro = s.chat(PermissionMode::ReadOnly).await;
    s.send(&ro.id, r#"tessera bash {"command":"touch x"}"#)
        .await;
    s.settle(&ro.id).await;
    let tools: Vec<String> = serde_json::from_value(s.logged("tools").pop().unwrap()).unwrap();
    assert!(!tools.contains(&"bash".to_string()), "{tools:?}");
    let called = s.logged("called").pop().unwrap();
    assert_eq!(called["error"], true);
    assert_eq!(
        called["text"].as_str().unwrap(),
        "Not performed: This helper is read-only; the action was not performed. Carry on with other allowed work, or ask in the chat what needs to change."
    );
    assert!(!s.work.join("x").exists());
}

#[tokio::test]
async fn the_bridge_belongs_to_its_chat_and_turn() {
    let s = setup().await;
    let ada = s
        .save_bot("Ada", "claude-code/opus", ToolPreset::Full)
        .await;
    let bob = s
        .save_bot("Bob", "claude-code/opus", ToolPreset::Research)
        .await;
    // Ada keeps a note of her own (her own memory).
    let a = s.session_on("claude-code/opus", Some(&ada.id), false).await;
    s.send(
        &a.id,
        r#"tessera note {"action":"create","title":"Ada's secret plan","content":"x"}"#,
    )
    .await;
    s.settle(&a.id).await;
    let notes: Vec<NoteInfo> = s.call(Request::ListNotes).await;
    let note = notes
        .iter()
        .find(|n| n.title == "Ada's secret plan")
        .unwrap();
    // The user keeps it from agents: Bob's chat acts as Bob, and it isn't his
    // to read, as in Tessera's own agent. (The id goes to the stand-in
    // escaped: written out in the user's own message it would bring the note
    // into Bob's chat.)
    let _: Note = s
        .call(Request::SetNoteAgents {
            id: note.id.clone(),
            keep_from_agents: true,
        })
        .await;
    let b = s.session_on("claude-code/opus", Some(&bob.id), false).await;
    s.send(
        &b.id,
        &format!(
            r#"tessera note {{"action":"read","id":"{}"}}"#,
            note.id.replace('_', "\\u005f")
        ),
    )
    .await;
    s.settle(&b.id).await;
    let called = s.logged("called").pop().unwrap();
    assert_eq!(called["error"], true, "{called}");
    // His preset decides what he gets, of Tessera's tools and Claude Code's.
    let tools: Vec<String> = serde_json::from_value(s.logged("tools").pop().unwrap()).unwrap();
    assert!(!tools.contains(&"schedule".to_string()), "{tools:?}");
    let deny = s.last_arg("--disallowedTools").unwrap();
    assert!(deny.contains("Bash") && deny.contains("Edit"), "{deny}");
    assert!(s.runs().last().unwrap().contains("--strict-mcp-config"));
    // His context is his persona.
    let ctx = s
        .logged("context")
        .pop()
        .unwrap()
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        ctx.contains("You are Bob (@bob)") && ctx.contains("keeps the lab tidy"),
        "{ctx}"
    );
    // A tool it wasn't offered is refused.
    s.send(
        &b.id,
        r#"tessera write {"path":"unavailable.txt","content":"notes"}"#,
    )
    .await;
    s.settle(&b.id).await;
    let called = s.logged("called").pop().unwrap();
    assert_eq!(called["error"], true);
    assert_eq!(
        called["text"].as_str().unwrap(),
        "Not performed: This tool is not available to this actor now. Carry on with other allowed work, or ask in the chat what needs to change."
    );
    assert!(!s.work.join("unavailable.txt").exists());
    // A call after its turn ended never runs.
    let before = s.call::<Vec<NoteInfo>>(Request::ListNotes).await.len();
    s.send(
        &a.id,
        r#"late note {"action":"create","title":"Too late","content":"x"}"#,
    )
    .await;
    s.settle(&a.id).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    // Refused (in Tessera's words, when its input was still open).
    let late = s.logged("late");
    assert_eq!(late.len(), 1, "{late:?}");
    assert!(
        late[0].is_null()
            || (late[0]["result"]["isError"] == true
                && late[0]["result"]["content"][0]["text"]
                    .as_str()
                    .is_some_and(|t| t.contains("had already finished"))),
        "{late:?}"
    );
    let after: Vec<NoteInfo> = s.call(Request::ListNotes).await;
    assert_eq!(after.len(), before, "a call after the turn ran");
}

#[tokio::test]
async fn private_chats_get_no_bridge_and_no_context() {
    let s = setup().await;
    let chat = s.session_on("claude-code/opus", None, true).await;
    s.send(&chat.id, "hello").await;
    s.settle(&chat.id).await;
    let run = s.runs().pop().unwrap();
    assert!(!run.contains("--mcp-config"), "{run}");
    assert!(!run.contains("--append-system-prompt"), "{run}");
    assert!(s.logged("context").is_empty());
    // Asking for Tessera's server anyway gets a refusal.
    let reply = s.logged("no-bridge-reply").pop().unwrap();
    assert_eq!(reply, Value::Null);
    let refused = s
        .logged("mcp")
        .into_iter()
        .rev()
        .find(|m| m["method"] == "tools/list")
        .unwrap();
    assert_eq!(refused["reply"]["subtype"], "error");
}

/// A private chat's `claude` starts without the user's and the project's
/// customizations (hooks, CLAUDE.md and memory, plugins, MCP servers); an
/// ordinary chat keeps them. Only synthetic files beside the stand-in are used.
#[tokio::test]
async fn private_chats_start_without_the_users_customizations() {
    let s = setup().await;
    let synthetic = tempfile::tempdir().unwrap();
    let user_hook = synthetic.path().join("home/.claude/startup-marker");
    let project_hook = synthetic.path().join("project/.claude/startup-marker");
    for path in [&user_hook, &project_hook] {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    }
    let settings = [
        synthetic.path().join("home/.claude/settings.json"),
        synthetic.path().join("project/.claude/settings.json"),
    ];
    for (path, hook) in settings.iter().zip([&user_hook, &project_hook]) {
        std::fs::write(
            path,
            serde_json::to_vec(&json!({"hooks": [hook],
                "plugins": ["synthetic installed plugin"], "mcp": ["synthetic external MCP"]}))
            .unwrap(),
        )
        .unwrap();
    }
    let memory = [
        synthetic.path().join("home/.claude/CLAUDE.md"),
        synthetic.path().join("project/CLAUDE.md"),
        synthetic
            .path()
            .join("home/.claude/projects/fixture/memory/MEMORY.md"),
    ];
    for path in &memory {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "synthetic memory fixture").unwrap();
    }
    std::fs::write(
        s.fake.path().join("privacy-fixture.json"),
        serde_json::to_vec(&json!({"settings": settings, "memory": memory})).unwrap(),
    )
    .unwrap();
    let private = s.session_on("claude-code/opus", None, true).await;
    s.send(&private.id, "hello privately").await;
    s.settle(&private.id).await;
    let run = s.runs().pop().unwrap();
    assert!(
        run.contains(
            "--safe-mode --setting-sources  --disable-slash-commands --no-session-persistence"
        ),
        "{run}"
    );
    assert!(!user_hook.exists() && !project_hook.exists());
    assert_eq!(s.logged("startup-context"), [json!([])]);
    assert_eq!(s.logged("startup-integrations"), [json!([])]);
    let env = s.logged("env").pop().unwrap();
    for name in [
        "CLAUDE_CODE_SAFE_MODE",
        "CLAUDE_CODE_DISABLE_CLAUDE_MDS",
        "CLAUDE_CODE_DISABLE_AUTO_MEMORY",
    ] {
        assert!(env.as_array().unwrap().contains(&json!(name)), "{env}");
    }
    // An ordinary chat is unchanged: the user's own setup applies there.
    let ordinary = s.chat(PermissionMode::Auto).await;
    s.send(&ordinary.id, "hello normally").await;
    s.settle(&ordinary.id).await;
    let run = s.runs().pop().unwrap();
    assert!(!run.contains("--safe-mode"), "{run}");
    assert!(user_hook.exists() && project_hook.exists());
    assert_eq!(s.logged("startup-context")[1].as_array().unwrap().len(), 3);
    assert_eq!(
        s.logged("startup-integrations")[1]
            .as_array()
            .unwrap()
            .len(),
        4
    );
    let env = s.logged("env").pop().unwrap();
    assert!(
        !env.as_array()
            .unwrap()
            .contains(&json!("CLAUDE_CODE_SAFE_MODE"))
    );
    s.engine.shutdown().await;
}

/// A `claude` too old for the private controls ends at start: the chat
/// says why, and nothing of the private chat was sent to it.
#[tokio::test]
async fn a_cli_without_the_private_controls_gets_nothing_of_the_chat() {
    let s = setup().await;
    std::fs::write(s.fake.path().join("private-old"), "").unwrap();
    let private = s.session_on("claude-code/opus", None, true).await;
    s.send(&private.id, "private fixture text must not reach the CLI")
        .await;
    s.settle(&private.id).await;
    assert!(s.prompts().is_empty());
    let snap = s.snapshot(&private.id).await;
    assert_eq!(snap.info.status, SessionStatus::Error);
    assert!(
        notices(&snap.entries)
            .iter()
            .any(|(_, text)| text.contains("--safe-mode")),
        "{:?}",
        notices(&snap.entries)
    );
    s.engine.shutdown().await;
}

#[tokio::test]
async fn adding_claude_code_sets_everything_up() {
    let s = setup().await;
    // An API key is never taken for it.
    let e = s
        .engine
        .handle(
            Request::SaveProvider {
                spec: ProviderSpec {
                    id: "my-claude".into(),
                    name: "My Claude".into(),
                    kind: ProviderKind::ClaudeCode,
                    base_url: s.fake.path().join("claude").display().to_string(),
                    api_key: Some("sk-ant-xxx".into()),
                    discover: true,
                    models: vec![],
                },
            },
            None,
        )
        .await
        .unwrap_err();
    assert!(
        e.message.contains("never takes Claude credentials"),
        "{}",
        e.message
    );
    let p: ProviderInfo = s
        .call(Request::SaveProvider {
            spec: ProviderSpec {
                id: "my-claude".into(),
                name: "My Claude".into(),
                kind: ProviderKind::ClaudeCode,
                base_url: s.fake.path().join("claude").display().to_string(),
                api_key: None,
                discover: true,
                models: vec![],
            },
        })
        .await;
    assert!(p.authenticated, "{p:?}");
    assert!(p.models.iter().any(|m| m.id == "sonnet"), "{:?}", p.models);
    // Its chats get Tessera's tools and context, with nothing more to do.
    let chat = s.session_on("my-claude/sonnet", None, false).await;
    s.send(&chat.id, r#"tessera docs {"topic":"bots"}"#).await;
    s.settle(&chat.id).await;
    assert!(s.last_arg("--mcp-config").is_some());
    assert!(s.last_arg("--append-system-prompt-file").is_some());
    assert_eq!(s.logged("called").pop().unwrap()["error"], false);
    // And with an empty command it finds `claude` itself, where installed;
    // elsewhere it says so instead of saving a provider nobody sees.
    let installed = tessera_core::claude_code::locate("").is_some();
    let e = s
        .engine
        .handle(
            Request::SaveProvider {
                spec: ProviderSpec {
                    id: "found-claude".into(),
                    name: "Claude".into(),
                    kind: ProviderKind::ClaudeCode,
                    base_url: String::new(),
                    api_key: None,
                    discover: true,
                    models: vec![],
                },
            },
            None,
        )
        .await;
    if installed {
        assert!(e.is_ok(), "{e:?}");
    } else {
        let e = e.unwrap_err();
        assert!(e.message.contains("isn't installed"), "{}", e.message);
    }
}

#[tokio::test]
async fn with_only_claude_code_titles_and_memory_still_work() {
    let home = tempfile::tempdir().unwrap();
    let fake = tempfile::tempdir().unwrap();
    let s = setup_in(
        home,
        fake,
        "[memory]\nenabled = true\n[agent]\nreview_interval = 1\n",
    )
    .await;
    // (The mock model is there, but no role points at it.)
    let chat = s.chat(PermissionMode::Default).await;
    s.send(&chat.id, "plan a trip to the lakes").await;
    s.settle(&chat.id).await;
    // The title: one small answer from Claude Code's smallest model, with
    // no tools and nothing kept.
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let snap = s.snapshot(&chat.id).await;
            if snap.info.title == "Stub Title Here" {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("no title from Claude Code");
    let one = s.logged("oneshot").pop().unwrap();
    assert_eq!(one["model"], "haiku");
    assert!(one["prompt"].as_str().unwrap().contains("plan a trip"));
    let shot = s
        .all_runs()
        .into_iter()
        .find(|r| r.contains("--output-format json"))
        .unwrap();
    assert!(
        shot.contains("--tools  --strict-mcp-config --no-session-persistence"),
        "{shot}"
    );
    // The memory review runs on it too: its own tools off, Tessera's
    // memory tool on, nothing kept.
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let m: MemoryState = s.call(Request::GetMemory).await;
            if m.memory.contains("likes stub tests") {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("the memory review didn't save anything");
    let review = s
        .runs()
        .into_iter()
        .find(|r| r.contains("--tools  --no-session-persistence"))
        .unwrap();
    assert!(
        review.contains("--model haiku") && review.contains("--strict-mcp-config"),
        "{review}"
    );
    // Posted once the review is over (it saved the note on the way).
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let snap = s.snapshot(&chat.id).await;
            if snap.entries.iter().any(|e| matches!(&e.body,
                EntryBody::Notice { text, .. } if text.starts_with("Learned from this conversation")))
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("no learned notice");
}

#[tokio::test]
async fn bots_on_claude_code_talk_in_channels() {
    let s = setup().await;
    let ada = s
        .save_bot("Ada", "claude-code/opus", ToolPreset::Full)
        .await;
    let bob = s
        .save_bot("Bob", "claude-code/haiku", ToolPreset::Full)
        .await;
    let room: SessionInfo = s
        .call(Request::CreateChannel {
            title: "Lab".into(),
            members: vec![ada.id.clone(), bob.id.clone()],
            cwd: Some(s.work.display().to_string()),
        })
        .await;
    s.send(&room.id, "hello both").await;
    s.settle(&room.id).await;
    let snap = s.snapshot(&room.id).await;
    let authors: Vec<Option<String>> = snap
        .entries
        .iter()
        .filter_map(|e| match &e.body {
            EntryBody::Assistant { author, .. } => Some(author.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(authors, vec![Some(ada.id.clone()), Some(bob.id.clone())]);
    // Each bot has its own conversation, persona and model; Bob hears Ada.
    let runs = s.runs();
    assert!(runs[0].contains("--model opus") && runs[1].contains("--model haiku"));
    let ctx = s.logged("context");
    assert!(ctx[0].as_str().unwrap().contains("You are Ada"));
    assert!(ctx[1].as_str().unwrap().contains("You are Bob"));
    let prompts = s.prompts();
    assert!(prompts[1].contains("[@ada]: Echo"), "{}", prompts[1]);
    // Next round: each continues its own.
    s.send(&room.id, "again").await;
    s.settle(&room.id).await;
    let runs = s.runs();
    let resumed: Vec<&String> = runs[2..]
        .iter()
        .filter(|r| r.contains("--resume"))
        .collect();
    assert_eq!(resumed.len(), 2, "{runs:#?}");
    assert_ne!(
        runs[2].split("--resume ").nth(1).unwrap().split(' ').next(),
        runs[3].split("--resume ").nth(1).unwrap().split(' ').next()
    );
}

#[tokio::test]
async fn compaction_subagents_and_limits_parity() {
    let s = setup().await;
    let chat = s.chat(PermissionMode::Default).await;
    // Compact asks Claude Code to compact its own conversation.
    s.send(&chat.id, "hi").await;
    s.settle(&chat.id).await;
    s.call::<Value>(Request::Compact {
        session_id: chat.id.clone(),
        instructions: None,
    })
    .await;
    s.settle(&chat.id).await;
    let snap = s.snapshot(&chat.id).await;
    assert!(
        snap.entries.iter().any(|e| matches!(&e.body,
            EntryBody::Compaction { external: true, summary, tokens_before: 52000, tokens_after: 4000, .. } if summary.is_empty())),
        "no truthful compaction boundary"
    );
    assert_eq!(snap.info.context.tokens, 4000);
    // Its own subagents: one card with what they did, Tessera's tools too.
    s.send(&chat.id, "delegate this").await;
    s.settle(&chat.id).await;
    let snap = s.snapshot(&chat.id).await;
    let details = snap
        .entries
        .iter()
        .find_map(|e| match &e.body {
            EntryBody::ToolResult { tool, details, .. } if tool == "Agent" => details.clone(),
            _ => None,
        })
        .expect("no subagent card");
    assert_eq!(details["subagent"]["type"], "Explore");
    assert_eq!(details["subagent"]["description"], "Count the files");
    assert_eq!(details["subagent"]["tool_uses"], 2);
    assert_eq!(details["subagent"]["status"], "completed");
    let docs = s.logged("called").pop().unwrap();
    assert_eq!(docs["name"], "docs");
    assert_eq!(docs["error"], false, "{docs}");
    // Only the conversation's own calls are cards.
    assert_eq!(tool_calls(&snap.entries), vec!["Agent"]);
    // The plan's limit: waited out, then it carries on by itself.
    let mut u = update(&chat.id);
    if let Request::UpdateSession { permission, .. } = &mut u {
        *permission = Some(PermissionMode::Auto);
    }
    s.call::<SessionInfo>(u).await;
    s.send(&chat.id, "hit the limit please").await;
    tokio::time::timeout(Duration::from_secs(30), s.engine.wait_idle(&chat.id))
        .await
        .expect("it didn't carry on after the limit");
    let snap = s.snapshot(&chat.id).await;
    assert_eq!(snap.info.status, SessionStatus::Idle);
    let notices: Vec<String> = snap
        .entries
        .iter()
        .filter_map(|e| match &e.body {
            EntryBody::Notice { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect();
    assert!(
        notices
            .iter()
            .any(|n| n.contains("5-hour limit is reached")),
        "{notices:#?}"
    );
    assert!(
        notices.iter().any(|n| n.contains("Stop cancels the wait")),
        "{notices:#?}"
    );
    assert!(
        notices
            .iter()
            .any(|n| n.contains("weekly limit is nearly reached. You've used 91%")),
        "{notices:#?}"
    );
    assert_eq!(
        texts(&snap.entries).last().unwrap(),
        "Back after the reset."
    );
}

/// The plan's limits as Claude Code reports them along with a turn: on the
/// usage page with how much is used, when each resets and how fresh that is,
/// and still there after a restart.
#[tokio::test]
async fn its_plan_limits_show_on_the_usage_page() {
    let s = setup().await;
    let chat = s.chat(PermissionMode::Default).await;
    let before = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    s.send(&chat.id, "check my limits").await;
    s.settle(&chat.id).await;
    assert_eq!(
        texts(&s.snapshot(&chat.id).await.entries).last().unwrap(),
        "Checked."
    );
    let check = |r: UsageReport| {
        let sub = r
            .subscriptions
            .iter()
            .find(|x| x.provider == "claude-code")
            .expect("Claude isn't listed");
        let l = &sub.limits;
        let windows: Vec<&str> = l.iter().map(|l| l.window.as_str()).collect();
        assert_eq!(windows, ["five_hour", "weekly", "weekly_opus"], "{l:#?}");
        assert_eq!(
            l.iter().map(|l| l.label.as_str()).collect::<Vec<_>>(),
            ["5-hour limit", "Weekly limit", "Weekly limit, Opus"]
        );
        let used: Vec<Option<u64>> = l
            .iter()
            .map(|l| l.used_percent.map(|p| p.round() as u64))
            .collect();
        assert_eq!(used, [Some(42), Some(18), None]);
        assert_eq!(
            l.iter().map(|l| l.status).collect::<Vec<_>>(),
            [Some(LimitStatus::Ok), None, Some(LimitStatus::Reached)]
        );
        let in_2h = l[0].resets_at.unwrap().abs_diff(before + 7_200_000);
        assert!(in_2h < 10_000, "{:?}", l[0].resets_at);
        assert!(
            l.iter()
                .all(|l| l.updated_at >= before && l.updated_at < before + 60_000)
        );
    };
    check(
        s.call(Request::GetUsage {
            from: None,
            to: None,
            utc_offsets: Vec::new(),
        })
        .await,
    );
    // After a restart: the same, as of the same time.
    let Setup {
        engine, home, fake, ..
    } = s;
    drop(engine);
    let s = setup_in(home, fake, "").await;
    check(
        s.call(Request::GetUsage {
            from: None,
            to: None,
            utc_offsets: Vec::new(),
        })
        .await,
    );
}

/// Tessera's context is part of Claude Code's system prompt: it stays the
/// same for the whole conversation (a change would be a full prompt-cache
/// miss on every turn after it), and is brought up to date only when
/// Claude Code compacts or a setting it reflects changes.
#[tokio::test]
async fn its_context_stays_put_until_it_compacts_or_settings_change() {
    let s = setup().await;
    let chat = s.chat(PermissionMode::Default).await;
    let contexts = || -> Vec<String> {
        s.logged("context")
            .into_iter()
            .map(|c| c.as_str().unwrap().to_string())
            .collect()
    };
    s.send(&chat.id, "hello").await;
    s.settle(&chat.id).await;
    // A memory saved mid-conversation…
    s.send(
        &chat.id,
        r#"tessera memory {"target":"memory","action":"add","content":"The build runs on my-server."}"#,
    )
    .await;
    s.settle(&chat.id).await;
    assert_eq!(s.logged("called").pop().unwrap()["error"], false);
    s.send(&chat.id, "and again").await;
    s.settle(&chat.id).await;
    // …doesn't change what the conversation was given.
    let ctx = contexts();
    assert_eq!(ctx.len(), 3, "{ctx:#?}");
    assert!(ctx.iter().all(|c| c == &ctx[0]));
    assert!(!ctx[2].contains("runs on my-server"));
    assert!(s.runs()[2].contains("--resume"));
    // Once Claude Code has compacted, the next turn brings it up to date.
    s.call::<Value>(Request::Compact {
        session_id: chat.id.clone(),
        instructions: None,
    })
    .await;
    s.settle(&chat.id).await;
    s.send(&chat.id, "after compacting").await;
    s.settle(&chat.id).await;
    let ctx = contexts();
    assert_eq!(ctx[3], ctx[0], "the compacting run itself still has it");
    assert!(ctx[4].contains("runs on my-server"), "{}", ctx[4]);
    // Frozen again from there…
    s.send(
        &chat.id,
        r#"tessera memory {"target":"memory","action":"add","content":"The tests run nightly."}"#,
    )
    .await;
    s.settle(&chat.id).await;
    s.send(&chat.id, "once more").await;
    s.settle(&chat.id).await;
    let ctx = contexts();
    assert_eq!(ctx[6], ctx[4]);
    // …until a setting changes (the same moment Tessera's own prompts are renewed).
    let unchanged: Request =
        serde_json::from_value(json!({"method": "set_security", "params": {}})).unwrap();
    s.call::<Value>(unchanged).await;
    s.send(&chat.id, "after the settings").await;
    s.settle(&chat.id).await;
    let ctx = contexts();
    assert!(ctx[7].contains("run nightly"), "{}", ctx[7]);
    // The same conversation throughout.
    assert!(s.runs()[7].contains("--resume"));
}

#[tokio::test]
async fn background_work_keeps_the_run_open_for_approvals() {
    let s = setup().await;
    let chat = s.chat(PermissionMode::Ask).await;
    let mut rx = s.engine.subscribe();
    // Its reply ends with a command still running; when the command is done
    // Claude Code carries on by itself, and that part can still ask.
    s.send(&chat.id, "run it in the background then check")
        .await;
    let req = approval(&mut rx).await;
    assert_eq!(req.call_id, "toolu_after");
    s.engine
        .handle(
            Request::RespondApproval {
                session_id: chat.id.clone(),
                request_id: req.id,
                decision: ApprovalDecision::Allow,
            },
            None,
        )
        .await
        .unwrap();
    s.settle(&chat.id).await;
    let snap = s.snapshot(&chat.id).await;
    assert_eq!(
        texts(&snap.entries),
        vec![
            "Starting it.",
            "It is running.",
            "It finished; checking.",
            "All done."
        ]
    );
    assert_eq!(s.runs().len(), 1, "one run did it all");
    assert_eq!(snap.info.status, SessionStatus::Idle);
}

/// Claude Code sometimes runs a turn of its own before the message it was
/// sent (resuming after a stopped run, with something to report). That
/// turn's end isn't the end: the message is still to come, and its approvals
/// must still reach the user (they failed with "Stream closed" before).
#[tokio::test]
async fn a_turn_of_its_own_first_keeps_the_run_open_for_the_message() {
    let s = setup().await;
    let chat = s.chat(PermissionMode::Ask).await;
    let mut rx = s.engine.subscribe();
    s.send(&chat.id, "notice first, then check").await;
    let req = approval(&mut rx).await;
    assert_eq!(req.call_id, "toolu_after2");
    s.engine
        .handle(
            Request::RespondApproval {
                session_id: chat.id.clone(),
                request_id: req.id,
                decision: ApprovalDecision::Allow,
            },
            None,
        )
        .await
        .unwrap();
    s.settle(&chat.id).await;
    let snap = s.snapshot(&chat.id).await;
    assert_eq!(
        texts(&snap.entries),
        vec![
            "Your earlier helpers stopped.",
            "Now your message.",
            "All done."
        ]
    );
    assert_eq!(s.runs().len(), 1, "one run did it all");
    assert_eq!(snap.info.status, SessionStatus::Idle);
}

/// Steering a Claude Code chat reaches it in the same turn (Claude Code
/// takes a message sent mid-turn up at its next step), not after the turn
/// as a follow-up.
#[tokio::test]
async fn steering_reaches_claude_code_in_the_same_turn() {
    let s = setup().await;
    let chat = s.chat(PermissionMode::Auto).await;
    s.send(&chat.id, "steer me").await;
    // Its command is running: steer now.
    for _ in 0..100 {
        if texts(&s.snapshot(&chat.id).await.entries).contains(&"Working on it.".to_string()) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    }
    s.engine
        .handle(
            Request::Send {
                session_id: chat.id.clone(),
                content: vec![ContentPart::text("use the staging server")],
                delivery: Delivery::Steer,
                attachments: Vec::new(),
            },
            None,
        )
        .await
        .unwrap();
    s.settle(&chat.id).await;
    let snap = s.snapshot(&chat.id).await;
    assert_eq!(
        texts(&snap.entries),
        vec!["Working on it.", "Steered: use the staging server"]
    );
    assert_eq!(s.runs().len(), 1, "one run, one turn");
}

/// While Claude Code's background work runs after its answer, the chat is
/// idle (the work can take hours: an upgrade, a build); when the work ends
/// and it carries on by itself, the chat is busy again and its approvals
/// still reach the user.
#[tokio::test]
async fn a_chat_is_idle_while_background_work_runs_and_busy_when_it_carries_on() {
    let s = setup().await;
    let chat = s.chat(PermissionMode::Ask).await;
    let mut rx = s.engine.subscribe();
    s.send(&chat.id, "run it in the background slowly then check")
        .await;
    let mut idle = false;
    for _ in 0..100 {
        let snap = s.snapshot(&chat.id).await;
        if texts(&snap.entries).contains(&"It is running.".to_string())
            && snap.info.status == SessionStatus::Idle
        {
            idle = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(idle, "idle while the command runs in the background");
    let req = approval(&mut rx).await;
    assert_eq!(req.call_id, "toolu_after");
    s.engine
        .handle(
            Request::RespondApproval {
                session_id: chat.id.clone(),
                request_id: req.id,
                decision: ApprovalDecision::Allow,
            },
            None,
        )
        .await
        .unwrap();
    s.settle(&chat.id).await;
    let snap = s.snapshot(&chat.id).await;
    assert_eq!(texts(&snap.entries).last().unwrap(), "All done.");
    assert_eq!(snap.info.status, SessionStatus::Idle);
    assert_eq!(s.runs().len(), 1, "one run did it all");
}

/// While it waits for its own background work it isn't finished: no
/// "Finished" then (the chat shows idle), one when all of it has ended.
#[tokio::test]
async fn a_chat_waiting_on_its_background_work_is_finished_once_at_the_end() {
    let s = setup_with("[agent]\nreview_interval = 0\nnotify_after_minutes = 0.0000001\n").await;
    let chat = s.chat(PermissionMode::Auto).await;
    let mut events = s.engine.subscribe();
    let finished = |events: &mut tokio::sync::broadcast::Receiver<Event>| {
        let mut out = Vec::new();
        while let Ok(e) = events.try_recv() {
            if let Event::Notification { notification } = e {
                out.push(notification);
            }
        }
        out
    };
    s.send(&chat.id, "run it in the background slowly then check")
        .await;
    let mut idle = false;
    for _ in 0..100 {
        let snap = s.snapshot(&chat.id).await;
        if texts(&snap.entries).contains(&"It is running.".to_string())
            && snap.info.status == SessionStatus::Idle
        {
            idle = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(idle, "idle while the command runs in the background");
    assert!(
        finished(&mut events).is_empty(),
        "not finished while it waits"
    );
    s.settle(&chat.id).await;
    assert_eq!(
        texts(&s.snapshot(&chat.id).await.entries).last().unwrap(),
        "All done."
    );
    let n = finished(&mut events);
    assert_eq!(n.len(), 1, "{n:?}");
    assert_eq!(n[0].title, "Finished");
    assert!(n[0].body.ends_with("All done."), "{n:?}");
}

/// The session a Claude Code subagent call's card points to.
fn child_of(entries: &[Entry], call: &str) -> (Value, String) {
    let details = entries
        .iter()
        .find_map(|e| match &e.body {
            EntryBody::ToolResult {
                call_id, details, ..
            } if call_id == call => details.clone(),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no card details for {call}"));
    let id = details["subagents"][0]["session_id"]
        .as_str()
        .expect("no session for the subagent")
        .to_string();
    (details, id)
}

#[tokio::test]
async fn cli_tasks_update_the_authoritative_plan() {
    let s = setup().await;
    let chat = s.chat(PermissionMode::Default).await;
    s.send(&chat.id, "make tasks").await;
    s.settle(&chat.id).await;
    let ledger = s.snapshot(&chat.id).await.info.task.unwrap();
    assert_eq!(
        ledger
            .plan
            .iter()
            .map(|p| p.text.as_str())
            .collect::<Vec<_>>(),
        ["Buy bread", "Pack plates", "Pick a park"]
    );
    assert_eq!(ledger.plan[0].status, PlanStatus::InProgress);
    s.send(&chat.id, "finish the first task").await;
    s.settle(&chat.id).await;
    let ledger = s.snapshot(&chat.id).await.info.task.unwrap();
    assert_eq!(ledger.plan.len(), 2);
    assert_eq!(ledger.plan[0].status, PlanStatus::Done);
    assert_eq!(ledger.plan[1].text, "Pack plates and cups");
    assert_eq!(ledger.plan[1].status, PlanStatus::InProgress);
}

#[tokio::test]
async fn cli_and_bridged_tasks_share_one_ledger_without_duplicates() {
    let s = setup().await;
    let chat = s.chat(PermissionMode::Default).await;
    s.send(&chat.id, r#"tessera task {"goal":"Keep this","constraints":["No production"],"done_criteria":["Tests pass"],"plan":[{"text":"Test","status":"pending"},{"text":"Buy bread","status":"pending"}]}"#).await;
    s.settle(&chat.id).await;
    s.send(&chat.id, "make tasks").await;
    s.settle(&chat.id).await;
    let ledger = s.snapshot(&chat.id).await.info.task.unwrap();
    assert_eq!(
        ledger.plan.len(),
        4,
        "matching bridge step adopted, other step kept"
    );
    assert_eq!(ledger.goal, "Keep this");
    assert_eq!(ledger.constraints, ["No production"]);
    assert_eq!(ledger.done_criteria, ["Tests pass"]);
    // A full bridge plan replacement can reorder steps; CLI ids follow text,
    // not ordinal, and status updates from either interface use this one plan.
    s.send(&chat.id, r#"tessera task {"plan":[{"text":"Pack plates","status":"pending"},{"text":"Test","status":"done"},{"text":"Buy bread","status":"in_progress"},{"text":"Pick a park","status":"pending"}]}"#).await;
    s.settle(&chat.id).await;
    s.send(&chat.id, "finish the first task").await;
    s.settle(&chat.id).await;
    let ledger = s.snapshot(&chat.id).await.info.task.unwrap();
    assert_eq!(ledger.plan.len(), 3);
    assert_eq!(ledger.plan[0].text, "Pack plates and cups");
    assert_eq!(ledger.plan[0].status, PlanStatus::InProgress);
    assert_eq!(ledger.plan[1].text, "Test");
    assert_eq!(ledger.plan[1].status, PlanStatus::Done);
    assert_eq!(ledger.plan[2].text, "Buy bread");
    assert_eq!(ledger.plan[2].status, PlanStatus::Done);
    s.send(&chat.id, r#"tessera task {"update":[{"index":1,"status":"done"}],"verify":[{"index":1,"status":"met","evidence":"Tests passed"}]}"#).await;
    s.settle(&chat.id).await;
    let ledger = s.snapshot(&chat.id).await.info.task.unwrap();
    assert_eq!(ledger.plan[0].status, PlanStatus::Done);
    assert_eq!(ledger.criteria[0].evidence, "Tests passed");
    assert_eq!(ledger.goal, "Keep this");
}

#[tokio::test]
async fn cli_task_ids_come_from_successful_results_not_ordinals() {
    let s = setup().await;
    let chat = s.chat(PermissionMode::Default).await;
    s.send(&chat.id, &builtin_actions(json!([
        {"tool":"TaskCreate", "input":{"subject":"First"}, "extra":{"task":{"id":"41"}}},
        {"tool":"TaskCreate", "input":{"subject":"Second"}, "extra":{"task":{"id":7}}},
        {"tool":"TaskCreate", "input":{"subject":"Failed"}, "is_error":true, "extra":{"task":{"id":"99"}}},
        {"tool":"TaskCreate", "input":{"subject":"No id"}},
        {"tool":"TaskUpdate", "input":{"taskId":"1", "status":"completed"}},
        {"tool":"TaskUpdate", "input":{"taskId":"41", "status":"completed"}, "is_error":true},
        {"tool":"TaskUpdate", "input":{"taskId":"41", "status":"completed"}, "extra":{"success":false}},
        {"tool":"TaskUpdate", "input":{"taskId":7, "status":"in_progress", "subject":"Second renamed"}},
        {"tool":"TaskCreate", "input":{"subject":"First"}, "extra":{"task":{"id":"41"}}}
    ]))).await;
    s.settle(&chat.id).await;
    let ledger = s.snapshot(&chat.id).await.info.task.unwrap();
    assert_eq!(
        ledger.plan.len(),
        2,
        "failed/missing-id/replayed creates add no steps"
    );
    assert_eq!(ledger.plan[0].status, PlanStatus::Pending);
    assert_eq!(ledger.plan[1].text, "Second renamed");
    assert_eq!(ledger.plan[1].status, PlanStatus::InProgress);
    s.send(
        &chat.id,
        &builtin_actions(json!([
            {"tool":"TaskUpdate", "input":{"taskId":"41", "status":"deleted"}},
            {"tool":"TaskUpdate", "input":{"taskId":"7", "status":"completed"}}
        ])),
    )
    .await;
    s.settle(&chat.id).await;
    let ledger = s.snapshot(&chat.id).await.info.task.unwrap();
    assert_eq!(ledger.plan.len(), 1);
    assert_eq!(ledger.plan[0].text, "Second renamed");
    assert_eq!(ledger.plan[0].status, PlanStatus::Done);
}

#[tokio::test]
async fn older_cli_todos_replace_only_the_ledger_plan() {
    let s = setup().await;
    let chat = s.chat(PermissionMode::Default).await;
    s.send(&chat.id, r#"tessera task {"goal":"Keep this","constraints":["No production"],"done_criteria":["Tests pass"]}"#).await;
    s.settle(&chat.id).await;
    s.send(&chat.id, "make a todo list").await;
    s.settle(&chat.id).await;
    let ledger = s.snapshot(&chat.id).await.info.task.unwrap();
    assert_eq!(ledger.plan.len(), 3);
    assert_eq!(ledger.plan[0].status, PlanStatus::Done);
    assert_eq!(ledger.plan[1].status, PlanStatus::InProgress);
    assert_eq!(ledger.plan[2].status, PlanStatus::Pending);
    assert_eq!(ledger.goal, "Keep this");
    assert_eq!(ledger.constraints, ["No production"]);
    assert_eq!(ledger.done_criteria, ["Tests pass"]);
}

#[tokio::test]
async fn cli_plan_and_result_ids_survive_compaction_and_restart_resume() {
    let s = setup().await;
    let chat = s.chat(PermissionMode::Default).await;
    s.call::<Value>(Request::SetGoal {
        session_id: chat.id.clone(),
        goal: "Picnic".into(),
    })
    .await;
    s.send(&chat.id, "make tasks").await;
    s.settle(&chat.id).await;
    let before = s.snapshot(&chat.id).await.info.task.unwrap();
    s.call::<Value>(Request::Compact {
        session_id: chat.id.clone(),
        instructions: None,
    })
    .await;
    s.settle(&chat.id).await;
    s.send(&chat.id, "after compact").await;
    s.settle(&chat.id).await;
    assert_eq!(s.snapshot(&chat.id).await.info.task.unwrap(), before);
    let held = serde_json::to_string(&s.logged("held-context")).unwrap();
    assert!(
        held.contains("Picnic") && held.contains("Pack plates"),
        "{held}"
    );
    let Setup {
        engine,
        home,
        fake,
        _work_dir: _keep_old_folder,
        ..
    } = s;
    drop(engine);
    let s = setup_in(home, fake, "").await;
    assert_eq!(s.snapshot(&chat.id).await.info.task.unwrap(), before);
    s.send(&chat.id, "finish the first task").await;
    s.settle(&chat.id).await;
    assert!(s.runs().last().unwrap().contains("--resume"));
    let after = s.snapshot(&chat.id).await.info.task.unwrap();
    assert_eq!(after.goal, "Picnic");
    assert_eq!(after.plan.len(), 2);
    assert_eq!(after.plan[0].status, PlanStatus::Done);
    assert_eq!(after.plan[1].text, "Pack plates and cups");
}

#[tokio::test]
async fn cli_task_policy_is_low_risk_but_keeps_the_task_ceiling() {
    let s = setup().await;
    let actions = builtin_actions(json!([
        {"tool":"TodoWrite", "input":{"todos":[]}, "prompt":true},
        {"tool":"TaskCreate", "input":{"subject":"Check"}, "extra":{"task":{"id":"73"}}, "prompt":true},
        {"tool":"TaskUpdate", "input":{"taskId":"73", "status":"completed"}, "prompt":true},
        {"tool":"TaskGet", "input":{"taskId":"73"}, "prompt":true},
        {"tool":"TaskList", "input":{}, "prompt":true}
    ]));
    for mode in [
        PermissionMode::Default,
        PermissionMode::Ask,
        PermissionMode::Auto,
        PermissionMode::ReadOnly,
    ] {
        let chat = s.chat(mode).await;
        let mut rx = s.engine.subscribe();
        let before = s.logged("executed").len();
        s.send(&chat.id, &actions).await;
        s.settle(&chat.id).await;
        assert_eq!(s.logged("executed").len() - before, 5);
        assert_eq!(
            s.snapshot(&chat.id).await.info.task.unwrap().plan[0].status,
            PlanStatus::Done
        );
        let tools = s.last_arg("--tools").unwrap();
        for tool in [
            "TodoWrite",
            "TaskCreate",
            "TaskUpdate",
            "TaskGet",
            "TaskList",
        ] {
            assert!(tools.split(',').any(|t| t == tool), "missing {tool}");
        }
        no_extra_approvals(&mut rx);
    }
    let mut bot = s
        .save_bot("Lookup", "claude-code/opus", ToolPreset::Custom)
        .await;
    bot.allowed_tools = vec!["read".into()];
    let bot: Bot = s.call(Request::SaveBot { bot }).await;
    let chat = s.session_on("claude-code/opus", Some(&bot.id), false).await;
    let before = s.logged("executed").len();
    s.send(&chat.id, &actions).await;
    s.settle(&chat.id).await;
    assert_eq!(s.logged("executed").len(), before);
    assert!(
        tool_results(&s.snapshot(&chat.id).await.entries)
            .iter()
            .all(|r| r.1)
    );
    assert!(
        !s.last_arg("--tools")
            .unwrap()
            .split(',')
            .any(|t| t == "TaskCreate")
    );
}

#[tokio::test]
async fn native_ledger_versions_survive_resume_compact_and_goal_clear() {
    let s = setup().await;
    let chat = s.chat(PermissionMode::Default).await;
    s.call::<Value>(Request::SetGoal {
        session_id: chat.id.clone(),
        goal: "User goal".into(),
    })
    .await;
    s.send(&chat.id, r#"tessera task {"constraints":["Do not deploy"],"done_criteria":["Both files pass"],"decisions":["Use tests"],"plan":[{"text":"Check","status":"blocked"}],"verify":[{"index":1,"status":"unmet","evidence":"Not checked yet"}]}"#).await;
    s.settle(&chat.id).await;
    assert!(s.prompts()[0].contains("User goal"));
    let ledger = s.snapshot(&chat.id).await.info.task.unwrap();
    assert_eq!(ledger.constraints, ["Do not deploy"]);
    s.send(&chat.id, "hello").await;
    s.settle(&chat.id).await;
    let p = s.prompts().last().unwrap().clone();
    assert!(
        p.contains("version ") && p.contains("Both files pass") && p.contains("Do not deploy"),
        "{p}"
    );
    s.call::<Value>(Request::Compact {
        session_id: chat.id.clone(),
        instructions: None,
    })
    .await;
    s.settle(&chat.id).await;
    s.send(&chat.id, "after compact").await;
    s.settle(&chat.id).await;
    assert_eq!(s.snapshot(&chat.id).await.info.task.unwrap(), ledger);
    let held = serde_json::to_string(&s.logged("held-context")).unwrap();
    assert!(
        held.contains("User goal") && held.contains("Both files pass"),
        "{held}"
    );
    assert!(
        s.logged("nudged").iter().any(|n| n
            .as_str()
            .is_some_and(|s| s.contains("Both files pass [unmet]"))),
        "the blocked plan does not hide an independent unmet criterion"
    );
    s.call::<Value>(Request::SetGoal {
        session_id: chat.id.clone(),
        goal: "".into(),
    })
    .await;
    s.send(&chat.id, "goal cleared").await;
    s.settle(&chat.id).await;
    let p = s.prompts().last().unwrap().clone();
    assert!(
        p.contains("including any cleared goal") && !p.contains("## Goal\nUser goal"),
        "{p}"
    );
    // Frozen system context does not move as ledger versions change.
    let contexts = s.logged("context");
    assert!(contexts.iter().all(|c| c == &contexts[0]));
    let saved = s.snapshot(&chat.id).await.info.task.unwrap();
    let Setup {
        engine,
        home,
        fake,
        _work_dir: _keep_old_folder,
        ..
    } = s;
    drop(engine);
    let s = setup_in(home, fake, "").await;
    assert_eq!(s.snapshot(&chat.id).await.info.task.unwrap(), saved);
    s.call::<Value>(Request::SetGoal {
        session_id: chat.id.clone(),
        goal: "Goal after restart".into(),
    })
    .await;
    s.send(&chat.id, "resumed after restart").await;
    s.settle(&chat.id).await;
    assert!(s.runs().last().unwrap().contains("--resume"));
    let p = s.prompts().last().unwrap().clone();
    assert!(
        p.contains("Goal after restart") && p.contains("Do not deploy"),
        "{p}"
    );
    std::fs::write(s.fake.path().join("forget"), "").unwrap();
    s.send(&chat.id, "new context after lost resume").await;
    s.settle(&chat.id).await;
    assert!(!s.runs().last().unwrap().contains("--resume"));
    let p = s.prompts().last().unwrap().clone();
    assert!(
        p.contains("Goal after restart")
            && p.contains("Do not deploy")
            && p.contains("Both files pass"),
        "{p}"
    );
}

#[tokio::test]
async fn its_questions_are_tesseras_question_cards() {
    let s = setup().await;
    // Even with nothing else asking: a question is not a permission.
    let chat = s.chat(PermissionMode::Auto).await;
    s.send(&chat.id, "ask me which setup to use").await;
    // Each question in turn, on one whiteboard in the chat.
    let next_ask = || async {
        for _ in 0..200 {
            let boards: Vec<Whiteboard> = s
                .call(Request::ListBoards {
                    session_id: Some(chat.id.clone()),
                })
                .await;
            if let Some(b) = boards.into_iter().find(|b| b.ask.is_some()) {
                return b;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("no question was asked");
    };
    let answer = |b: &Whiteboard, a: AskAnswer| Request::AnswerBoard {
        id: b.id.clone(),
        ask_id: b.ask.as_ref().unwrap().id.clone(),
        answer: a,
    };
    let b = next_ask().await;
    assert_eq!(b.title, "Database");
    let ask = b.ask.clone().unwrap();
    assert_eq!(ask.prompt, "Which database?");
    assert_eq!(
        ask.input,
        AskInput::Choice {
            options: vec!["Postgres".into(), "SQLite".into(), "Something else…".into()],
            multi: false,
        }
    );
    match &b.content {
        BoardContent::Markdown { markdown } => {
            assert!(
                markdown.contains("**SQLite**: One file") && markdown.contains("Which extras?"),
                "{markdown}"
            )
        }
        other => panic!("{other:?}"),
    }
    s.call::<Value>(answer(&b, AskAnswer::Choice { index: 1 }))
        .await;
    // The second lets several be picked; "something else" asks for the words.
    let mut b = next_ask().await;
    for _ in 0..100 {
        if b.ask.as_ref().unwrap().prompt == "Which extras?" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
        b = next_ask().await;
    }
    assert!(matches!(
        &b.ask.as_ref().unwrap().input,
        AskInput::Choice { multi: true, .. }
    ));
    s.call::<Value>(answer(
        &b,
        AskAnswer::Choices {
            indexes: vec![0, 2],
        },
    ))
    .await;
    let mut b = next_ask().await;
    for _ in 0..100 {
        if matches!(b.ask.as_ref().unwrap().input, AskInput::Text { .. }) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
        b = next_ask().await;
    }
    assert_eq!(b.ask.as_ref().unwrap().prompt, "Your answer: Which extras?");
    s.call::<Value>(answer(
        &b,
        AskAnswer::Text {
            text: "Alerts".into(),
        },
    ))
    .await;
    s.settle(&chat.id).await;
    let got = s.logged("answered").pop().unwrap();
    assert_eq!(got["behavior"], "allow");
    assert_eq!(
        got["answers"],
        json!({"Which database?": "SQLite", "Which extras?": "Backups, Alerts"})
    );
    // The board keeps the answers.
    let boards: Vec<Whiteboard> = s
        .call(Request::ListBoards {
            session_id: Some(chat.id.clone()),
        })
        .await;
    let BoardContent::Markdown { markdown } = &boards[0].content else {
        panic!()
    };
    assert!(
        markdown.contains("→ SQLite") && markdown.contains("→ Backups, Alerts"),
        "{markdown}"
    );

    // Declined: Claude Code is told, and carries on.
    s.send(&chat.id, "ask me which one again").await;
    let b = next_ask().await;
    s.call::<Value>(answer(&b, AskAnswer::Cancel)).await;
    s.settle(&chat.id).await;
    let got = s.logged("answered").pop().unwrap();
    assert_eq!(got["behavior"], "deny");
    assert_eq!(got["message"], "The user chose not to answer.");
}

#[tokio::test]
async fn claude_subagents_are_sessions_like_tesseras() {
    let s = setup().await;
    let chat = s.chat(PermissionMode::Default).await;
    s.send(&chat.id, "delegate this").await;
    s.settle(&chat.id).await;
    let snap = s.snapshot(&chat.id).await;
    // Its card names its session, as Tessera's own `agent` calls do.
    let (details, child) = child_of(&snap.entries, "toolu_ag");
    assert_eq!(details["subagents"][0]["name"], "Count the files");
    assert_eq!(details["subagents"][0]["background"], false);
    assert_eq!(details["subagent"]["type"], "Explore");
    // The session: a subagent of the chat, started by that call, with what it did.
    let c = s.snapshot(&child).await;
    assert_eq!(c.info.kind, SessionKind::Subagent);
    assert_eq!(c.info.parent_id.as_deref(), Some(chat.id.as_str()));
    assert_eq!(c.info.parent_call.as_deref(), Some("toolu_ag"));
    // Its role is Claude Code's subagent type, as the Subagents bar shows it.
    assert_eq!(c.info.role.as_deref(), Some("Explore"));
    assert_eq!(c.info.title, "↳ Count the files");
    assert_eq!(c.info.status, SessionStatus::Idle);
    assert_eq!(c.info.steps, 2);
    assert!(matches!(&c.entries[0].body,
        EntryBody::User { content, .. } if plain_text(content) == "count them"));
    // Tessera's tools by their own names, as in any chat.
    assert_eq!(tool_calls(&c.entries), vec!["Glob", "docs"]);
    let results = tool_results(&c.entries);
    assert_eq!(results[0], ("Glob".to_string(), false, "a.txt".to_string()));
    assert_eq!(results[1].0, "docs");
    assert!(texts(&c.entries).contains(&"Looking.".to_string()));
    // The main chat's own transcript keeps only its own calls.
    assert_eq!(tool_calls(&snap.entries), vec!["Agent"]);
    // It takes no messages, and stopping it alone is the chat's turn to stop.
    let send = s
        .engine
        .handle(
            Request::Send {
                session_id: child.clone(),
                content: vec![ContentPart::text("hello")],
                delivery: Delivery::Auto,
                attachments: Vec::new(),
            },
            None,
        )
        .await
        .unwrap_err();
    assert!(
        send.message.contains("Write in the main chat"),
        "{}",
        send.message
    );
    // Stop on one that is done changes nothing (a running one stops alone, see
    // `one_of_its_subagents_stops_alone`).
    s.engine
        .handle(
            Request::Abort {
                session_id: child.clone(),
            },
            None,
        )
        .await
        .unwrap();
    assert_eq!(s.snapshot(&child).await.info.status, SessionStatus::Idle);

    // One in the background: its call returns at once, it goes on, its notification ends it.
    s.send(&chat.id, "delegate in the background please").await;
    s.settle(&chat.id).await;
    let snap = s.snapshot(&chat.id).await;
    let (details, bg) = child_of(&snap.entries, "toolu_bg");
    assert_eq!(details["subagents"][0]["background"], true);
    let c = s.snapshot(&bg).await;
    assert_eq!(c.info.status, SessionStatus::Idle);
    assert!(
        c.info.role.is_some(),
        "every Claude Code subagent has a role"
    );
    assert_eq!(tool_calls(&c.entries), vec!["Bash"]);
    assert_eq!(texts(&c.entries).last().unwrap(), "All quiet in the logs.");

    // A command in the background is a command, not a subagent.
    let mut rx = s.engine.subscribe();
    s.send(&chat.id, "run it in the background now").await;
    s.settle(&chat.id).await;
    let snap = s.snapshot(&chat.id).await;
    let bash = snap
        .entries
        .iter()
        .find_map(|e| match &e.body {
            EntryBody::ToolResult {
                call_id, details, ..
            } if call_id == "toolu_bash_bg" => Some(details.clone()),
            _ => None,
        })
        .expect("no result for the command");
    assert!(
        bash.as_ref()
            .is_some_and(|d| d.get("duration_ms").is_some()),
        "{bash:?}"
    );
    assert!(bash.as_ref().unwrap().get("subagents").is_none());
    let mut started = Vec::new();
    while let Ok(e) = rx.try_recv() {
        if let Event::SessionUpdated { session } = e
            && session.kind == SessionKind::Subagent
            && !session.internal
        {
            started.push(session.id);
        }
    }
    assert!(started.is_empty(), "{started:?}");
}

#[tokio::test]
async fn a_credential_command_asks_once_and_runs() {
    let s = setup().await;
    s.call::<Value>(Request::SetSecret {
        name: "LAB_TOKEN".into(),
        value: "s3cret-value-123456".into(),
    })
    .await;
    let chat = s.chat(PermissionMode::Ask).await;
    let mut rx = s.engine.subscribe();
    s.send(
        &chat.id,
        r#"tessera bash {"command":"printf %s \"$LAB_TOKEN\" | wc -c"}"#,
    )
    .await;
    let req = approval(&mut rx).await;
    assert_eq!(req.tool, "bash");
    assert!(req.reason.contains("LAB_TOKEN"), "{}", req.reason);
    s.engine
        .handle(
            Request::RespondApproval {
                session_id: chat.id.clone(),
                request_id: req.id,
                decision: ApprovalDecision::Allow,
            },
            None,
        )
        .await
        .unwrap();
    s.settle(&chat.id).await;
    let called = s.logged("called").pop().unwrap();
    assert_eq!(called["error"], false, "{called}");
    assert!(called["text"].as_str().unwrap().contains("19"), "{called}");
}

#[tokio::test]
async fn a_bots_scheduled_duty_runs_on_claude_code_as_the_bot() {
    let s = setup().await;
    let bot = s
        .save_bot("Sentry", "claude-code/haiku", ToolPreset::Full)
        .await;
    let job: Job = s
        .call(Request::SaveJob {
            job: Job {
                id: String::new(),
                name: "Nightly sweep".into(),
                schedule: "0 3 * * *".into(),
                prompt: r#"tessera notify {"title":"Sweep done","body":"All quiet."}"#.into(),
                cwd: s.work.display().to_string(),
                model: Some("claude-code/haiku".parse().unwrap()),
                enabled: true,
                permission: PermissionMode::Auto,
                bot_id: Some(bot.id.clone()),
                last_run: None,
                next_run: None,
                last_session: None,
                last_status: None,
                ..Default::default()
            },
        })
        .await;
    let info: SessionInfo = s.call(Request::RunJob { id: job.id }).await;
    s.settle(&info.id).await;
    let ctx = s
        .logged("context")
        .pop()
        .unwrap()
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        ctx.contains("You are Sentry") && ctx.contains("# Scheduled run"),
        "{ctx}"
    );
    let called = s.logged("called").pop().unwrap();
    assert_eq!(called["name"], "notify");
    assert_eq!(called["error"], false, "{called}");
    assert!(s.runs().last().unwrap().contains("--model haiku"));
}

/// The board and note tiles in the shared layout.
async fn shown_tiles(s: &Setup) -> usize {
    let l: MosaicLayout = s.call(Request::GetMosaics).await;
    l.mosaics
        .iter()
        .flat_map(|m| &m.tiles)
        .filter(|t| t.board_id.is_some() || t.note_id.is_some())
        .count()
}

/// AGENT-TILES: a Claude Code turn's whiteboards, notes and browsing are
/// cards in the chat, as with Tessera's own agent (the bridge runs the same
/// tool code); only an explicit ask puts a tile on screen.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bridged_tools_put_up_no_tiles_unless_asked() {
    let s = setup().await;
    let chat = s.chat(PermissionMode::Auto).await;
    for msg in [
        r##"tessera whiteboard {"action":"show","title":"Plan","markdown":"# Steps"}"##,
        r#"tessera note {"action":"create","title":"Findings","content":"- WAN up"}"#,
    ] {
        s.send(&chat.id, msg).await;
        s.settle(&chat.id).await;
        let called = s.logged("called").pop().unwrap();
        assert_eq!(called["error"], false, "{called}");
    }
    assert_eq!(shown_tiles(&s).await, 0, "a bridged call put a tile up");
    let snap = s.snapshot(&chat.id).await;
    let details: Vec<Value> = snap
        .entries
        .iter()
        .filter_map(|e| match &e.body {
            EntryBody::ToolResult { details, .. } => details.clone(),
            _ => None,
        })
        .collect();
    assert_eq!(details[0]["board"]["title"], "Plan");
    assert_eq!(details[1]["note"]["title"], "Findings");
    // Asked to show it: it goes up.
    s.send(
        &chat.id,
        r##"tessera whiteboard {"action":"show","title":"Shown","markdown":"# Yes","open":true}"##,
    )
    .await;
    s.settle(&chat.id).await;
    assert_eq!(shown_tiles(&s).await, 1);

    // Browsing through the bridge stays in the background too.
    if !tessera_core::browser::installed(None) {
        eprintln!("no Chromium-family browser installed; skipping the browser part");
        return;
    }
    let app = axum::Router::new().route(
        "/",
        axum::routing::get(|| async {
            axum::response::Html("<!doctype html><title>Router</title><h1>Router admin</h1>")
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    s.send(
        &chat.id,
        &format!(r#"tessera browser {{"action":"open","url":"http://{addr}/"}}"#),
    )
    .await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    tokio::time::timeout(Duration::from_secs(60), s.engine.wait_idle(&chat.id))
        .await
        .unwrap();
    let called = s.logged("called").pop().unwrap();
    assert_eq!(called["name"], "browser");
    assert_eq!(called["error"], false, "{called}");
    assert!(
        called["text"]
            .as_str()
            .unwrap()
            .contains("Opened background tab"),
        "{called}"
    );
    assert_eq!(shown_tiles(&s).await, 1, "browsing put a tile up");
}

// ---- batch 14: task list on every model, rewind of its edits, the verification
// note, one process across turns, fast mode, stopping one subagent ----------------

/// A chat on Claude Code with `app.py` in its folder.
async fn code_chat(s: &Setup) -> SessionInfo {
    std::fs::write(s.work.join("app.py"), "print(1)\n").unwrap();
    s.chat(PermissionMode::Default).await
}

fn user_seq(entries: &[Entry], text: &str) -> u64 {
    entries
        .iter()
        .find(|e| matches!(&e.body, EntryBody::User { content, .. } if plain_text(content) == text))
        .map(|e| e.seq)
        .unwrap_or_else(|| panic!("no message {text:?}"))
}

#[tokio::test]
async fn its_task_list_is_on_for_every_model() {
    // Claude Code 2.1.268+ leaves its task tools out on models newer than Opus 4.7 /
    // Sonnet 4.6 unless asked: Tessera asks, so the chat's plan fills on Opus 5.x too.
    let s = setup().await;
    let chat = s.chat(PermissionMode::Default).await;
    s.send(&chat.id, "make tasks").await;
    s.settle(&chat.id).await;
    let env: Vec<String> = serde_json::from_value(s.logged("env").pop().unwrap()).unwrap();
    assert!(env.iter().any(|k| k == "CLAUDE_CODE_ENABLE_TODO_TOOLS"));
    let context = s.logged("context").pop().unwrap();
    assert!(
        context
            .as_str()
            .unwrap()
            .contains("Use task for the durable goal/constraints/plan/evidence"),
        "{context}"
    );
    let snap = s.snapshot(&chat.id).await;
    assert_eq!(snap.info.task.unwrap().plan.len(), 3);
}

#[tokio::test]
async fn rewind_puts_back_what_claude_changed() {
    let s = setup().await;
    let chat = code_chat(&s).await;
    // Its hooks come with `initialize`: copies before changes, the note before it stops.
    s.send(&chat.id, "change code").await;
    s.settle(&chat.id).await;
    let hooks: Vec<String> = serde_json::from_value(s.logged("hooks").pop().unwrap()).unwrap();
    assert_eq!(hooks, ["PostToolUse", "PreToolUse", "Stop"]);
    assert_eq!(
        std::fs::read_to_string(s.work.join("app.py")).unwrap(),
        "print(2)\n"
    );
    assert!(s.work.join("util.py").exists());
    // No permission prompt came (acceptEdits), and still each change had its copy taken.
    let pre: Vec<Value> = s
        .logged("hook")
        .into_iter()
        .filter(|h| h["event"] == "PreToolUse" && h["callback"] == "tessera_policy")
        .collect();
    assert_eq!(pre.len(), 2, "{pre:?}");
    assert!(
        pre.iter().all(|h| h["reply"] == json!({})),
        "nothing decided: {pre:?}"
    );
    let snap = s.snapshot(&chat.id).await;
    let at = user_seq(&snap.entries, "change code") - 1;
    let r: RewindResult = s
        .call(Request::Rewind {
            session_id: chat.id.clone(),
            to_seq: at,
            files: true,
            conversation: false,
            dry_run: false,
        })
        .await;
    assert_eq!(r.restored, ["app.py"], "{r:?}");
    assert_eq!(r.deleted, ["util.py"], "{r:?}");
    assert_eq!(
        std::fs::read_to_string(s.work.join("app.py")).unwrap(),
        "print(1)\n"
    );
    assert!(!s.work.join("util.py").exists());
    // Its commands are listed as not undone.
    assert!(
        r.not_undone.iter().any(|c| c.contains("py_compile")),
        "{:?}",
        r.not_undone
    );
}

#[tokio::test]
async fn a_subagents_changes_are_rewound_too() {
    let s = setup().await;
    let chat = code_chat(&s).await;
    s.send(&chat.id, "hand off a change").await;
    s.settle(&chat.id).await;
    assert_eq!(
        std::fs::read_to_string(s.work.join("app.py")).unwrap(),
        "print(3)\n"
    );
    let snap = s.snapshot(&chat.id).await;
    let at = user_seq(&snap.entries, "hand off a change") - 1;
    let r: RewindResult = s
        .call(Request::Rewind {
            session_id: chat.id.clone(),
            to_seq: at,
            files: true,
            conversation: true,
            dry_run: false,
        })
        .await;
    assert_eq!(r.restored, ["app.py"], "{r:?}");
    assert_eq!(
        std::fs::read_to_string(s.work.join("app.py")).unwrap(),
        "print(1)\n"
    );
}

#[tokio::test]
async fn unchecked_code_changes_get_one_note() {
    let s = setup().await;
    let chat = code_chat(&s).await;
    s.send(&chat.id, "change code").await;
    s.settle(&chat.id).await;
    // Once, naming the code it changed; it ran a check and then stopped as usual.
    let nudged = s.logged("nudged");
    assert_eq!(nudged.len(), 1, "{nudged:?}");
    let note = nudged[0].as_str().unwrap();
    assert!(
        note.starts_with("[Check] You changed app.py, util.py since the last check."),
        "{note}"
    );
    assert_eq!(s.logged("again").pop().unwrap(), json!({}));
    // Tested after the change: nothing to say. Text files aren't code.
    s.send(&chat.id, "change code and test").await;
    s.settle(&chat.id).await;
    s.send(&chat.id, "edit a file").await;
    s.settle(&chat.id).await;
    assert_eq!(s.logged("nudged").len(), 1);
    // Switched off: never.
    let off = setup_with("[agent]\nverify_check = \"off\"\n").await;
    let chat = code_chat(&off).await;
    off.send(&chat.id, "change code").await;
    off.settle(&chat.id).await;
    assert!(off.logged("nudged").is_empty());
}

/// Wait for the session of one of Claude Code's subagents (its `Agent` call).
async fn subagent_session(
    rx: &mut tokio::sync::broadcast::Receiver<Event>,
    call: &str,
) -> SessionInfo {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if let Ok(Event::SessionUpdated { session }) = rx.recv().await
                && session.parent_call.as_deref() == Some(call)
                && session.status == SessionStatus::Running
            {
                return session;
            }
        }
    })
    .await
    .expect("no session for the subagent")
}

#[tokio::test]
async fn one_of_its_subagents_stops_alone() {
    for bg in [false, true] {
        let s = setup().await;
        let chat = s.chat(PermissionMode::Default).await;
        let mut rx = s.engine.subscribe();
        let text = if bg {
            "slow helper in the background"
        } else {
            "slow helper"
        };
        s.send(&chat.id, text).await;
        let child = subagent_session(&mut rx, "toolu_slow_ag").await;
        s.call::<Value>(Request::Abort {
            session_id: child.id.clone(),
        })
        .await;
        s.settle(&chat.id).await;
        // Claude Code was asked to stop that task; the chat's turn went on.
        let stop = s.logged("stop_task").pop().expect("no stop_task");
        assert_eq!(stop["task_id"], "task_s");
        let snap = s.snapshot(&chat.id).await;
        assert!(
            texts(&snap.entries)
                .iter()
                .any(|t| t == "Went on without it."),
            "{:?}",
            texts(&snap.entries)
        );
        assert!(
            !snap.entries.iter().any(|e| matches!(&e.body,
                EntryBody::Notice { text, .. } if text == "Stopped.")),
            "the turn itself wasn't stopped"
        );
        let kid = s.snapshot(&child.id).await;
        assert_eq!(kid.info.status, SessionStatus::Interrupted, "bg {bg}");
        assert!(
            kid.entries.iter().any(|e| matches!(&e.body,
                EntryBody::Notice { text, .. } if text.starts_with("Stopped by the user"))),
            "bg {bg}"
        );
    }
}

#[tokio::test]
async fn stopping_the_chat_names_claude_helpers_and_ends_them() {
    for bg in [false, true] {
        let s = setup().await;
        let chat = s.chat(PermissionMode::Default).await;
        let mut rx = s.engine.subscribe();
        s.send(
            &chat.id,
            if bg {
                "slow helper in the background"
            } else {
                "slow helper"
            },
        )
        .await;
        let child = subagent_session(&mut rx, "toolu_slow_ag").await;
        s.call::<Value>(Request::Abort {
            session_id: chat.id.clone(),
        })
        .await;
        // The notice is recorded before waiting for Claude's interruption.
        let snap = s.snapshot(&chat.id).await;
        assert!(snap.entries.iter().any(|e| matches!(&e.body,
            EntryBody::Notice { text, .. } if text == "Stopping this chat, including 1 helper: Watch it."
        )), "bg {bg}: {:?}", snap.entries);
        s.settle(&chat.id).await;
        assert_eq!(
            s.snapshot(&child.id).await.info.status,
            SessionStatus::Interrupted
        );
    }
}

#[tokio::test]
async fn stopping_a_private_claude_chat_names_its_helpers_without_saving_them() {
    let s = setup().await;
    let chat = s.session_on("claude-code/opus", None, true).await;
    s.send(&chat.id, "slow helper in the background").await;
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if tool_calls(&s.snapshot(&chat.id).await.entries).contains(&"Agent".to_string()) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    s.call::<Value>(Request::Abort {
        session_id: chat.id.clone(),
    })
    .await;
    s.settle(&chat.id).await;
    let snap = s.snapshot(&chat.id).await;
    assert!(snap.children.is_empty());
    assert!(snap.entries.iter().any(|e| matches!(&e.body,
        EntryBody::Notice { text, .. } if text == "Stopping this chat, including 1 helper: Watch it."
    )));
}

#[tokio::test]
async fn a_command_moved_to_the_background_keeps_the_run_open() {
    let s = setup().await;
    let chat = s.chat(PermissionMode::Default).await;
    let mut rx = s.engine.subscribe();
    s.send(&chat.id, "move it to the background").await;
    let mut said = String::new();
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            match rx.recv().await {
                Ok(Event::ToolOutput { call_id, chunk, .. }) if call_id == "toolu_mv" => {
                    said.push_str(&chunk)
                }
                Ok(Event::SessionUpdated { session })
                    if session.id == chat.id
                        && session.status == SessionStatus::Idle
                        && said.contains("Moved") =>
                {
                    return;
                }
                _ => {}
            }
        }
    })
    .await
    .expect("no note on the command's card");
    assert!(
        said.contains("Moved to the background after its time limit"),
        "{said}"
    );
    assert!(!said.contains("Runs in the background"), "{said}");
    // It went on with its input open: the follow-up came in the same run.
    tokio::time::sleep(Duration::from_millis(1500)).await;
    s.settle(&chat.id).await;
    let snap = s.snapshot(&chat.id).await;
    assert!(
        texts(&snap.entries)
            .iter()
            .any(|t| t == "The build finished."),
        "{:?}",
        texts(&snap.entries)
    );
    assert!(!snap.entries.iter().any(|e| matches!(&e.body,
        EntryBody::Notice { text, .. } if text.contains("carried on by itself"))));
    assert_eq!(s.runs().len(), 1);
}

#[tokio::test]
async fn one_process_serves_the_next_turns() {
    let s = setup_with("[agent]\nreview_interval = 0\nclaude_keep_open_minutes = 10\n").await;
    let chat = s.chat(PermissionMode::Ask).await;
    s.send(&chat.id, "first").await;
    s.settle(&chat.id).await;
    s.send(&chat.id, "second").await;
    s.settle(&chat.id).await;
    // One `claude` for both, each message sent to it as it came.
    assert_eq!(s.runs().len(), 1, "{:#?}", s.runs());
    let snap = s.snapshot(&chat.id).await;
    assert_eq!(
        texts(&snap.entries),
        ["Echo: first", "Echo: second"],
        "{:?}",
        texts(&snap.entries)
    );
    // Approvals work in it as in a new one.
    let mut rx = s.engine.subscribe();
    s.send(&chat.id, "run a command").await;
    let a = approval(&mut rx).await;
    s.call::<Value>(Request::RespondApproval {
        session_id: chat.id.clone(),
        request_id: a.id,
        decision: ApprovalDecision::Allow,
    })
    .await;
    s.settle(&chat.id).await;
    assert_eq!(s.runs().len(), 1);
    assert!(texts(&s.snapshot(&chat.id).await.entries).contains(&"Done: allowed".to_string()));
    // A changed setting (here the thinking level) starts a new one, with it.
    let mut u = update(&chat.id);
    if let Request::UpdateSession { thinking, .. } = &mut u {
        *thinking = Some(ThinkingLevel::High);
    }
    s.call::<SessionInfo>(u).await;
    s.send(&chat.id, "third").await;
    s.settle(&chat.id).await;
    assert_eq!(s.runs().len(), 2);
    assert!(
        s.runs()[1].contains("--effort high --resume sess-"),
        "{}",
        s.runs()[1]
    );
    // Stopped mid-turn: that process is done with.
    s.send(&chat.id, "hang").await;
    tokio::time::sleep(Duration::from_millis(600)).await;
    s.call::<Value>(Request::Abort {
        session_id: chat.id.clone(),
    })
    .await;
    s.settle(&chat.id).await;
    s.send(&chat.id, "fourth").await;
    s.settle(&chat.id).await;
    assert_eq!(s.runs().len(), 3);
}

#[tokio::test]
async fn a_kept_process_that_wakes_up_by_itself_goes_on_as_a_turn() {
    let s = setup_with("[agent]\nreview_interval = 0\nclaude_keep_open_minutes = 10\n").await;
    let chat = s.chat(PermissionMode::Default).await;
    // A scheduled wake-up now keeps stdin open until stopped or reported
    // ended; the self-start-as-normal-turn assertions below are unchanged.
    s.send(&chat.id, "schedule a wakeup").await;
    tokio::time::timeout(Duration::from_secs(5), async {
        while !notices(&s.snapshot(&chat.id).await.entries)
            .iter()
            .any(|(_, text)| text.contains("background work (1 running)"))
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    s.call::<Value>(Request::Abort {
        session_id: chat.id.clone(),
    })
    .await;
    s.settle(&chat.id).await;
    s.send(&chat.id, "next").await;
    s.settle(&chat.id).await;
    assert_eq!(s.runs().len(), 2);
    // One kept that starts something by itself goes on as a turn of the chat,
    // in the same process; the next message goes to it as usual.
    s.send(&chat.id, "wake up by yourself").await;
    s.settle(&chat.id).await;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    s.settle(&chat.id).await;
    s.send(&chat.id, "after").await;
    s.settle(&chat.id).await;
    assert_eq!(s.runs().len(), 2, "{:#?}", s.runs());
    let snap = s.snapshot(&chat.id).await;
    assert_eq!(
        texts(&snap.entries)[2..],
        ["Later.", "I woke up by myself.", "Echo: after"],
        "{:?}",
        texts(&snap.entries)
    );
    // Nothing was sent to it for the turn it took up itself, and that turn
    // isn't sent back to it with the next message.
    let last = s.prompts().last().unwrap().clone();
    assert!(!last.contains("woke up"), "{last}");
    // A rewound conversation closes it: the next turn starts afresh.
    let at = user_seq(&snap.entries, "after") - 1;
    s.call::<RewindResult>(Request::Rewind {
        session_id: chat.id.clone(),
        to_seq: at,
        files: false,
        conversation: true,
        dry_run: false,
    })
    .await;
    s.send(&chat.id, "again").await;
    s.settle(&chat.id).await;
    assert_eq!(s.runs().len(), 3);
}

/// Claude Code takes a turn up by itself in the process kept between turns
/// (background work Tessera didn't see ended): that is a turn of the chat
/// like any other. A plain read runs as usual, a risky command asks as usual
/// (nothing is allowed that would otherwise ask), and nothing is sent to it.
#[tokio::test]
async fn work_it_takes_up_by_itself_between_turns_is_a_normal_turn() {
    let s = setup_with("[agent]\nreview_interval = 0\nclaude_keep_open_minutes = 10\n").await;
    std::fs::write(s.work.join("bg-output.txt"), "build ok").unwrap();
    let chat = s.chat(PermissionMode::Ask).await;
    let mut rx = s.engine.subscribe();
    s.send(&chat.id, "carry on after parking").await;
    s.settle(&chat.id).await;
    // The risky command asks the user, as in any turn.
    let req = approval(&mut rx).await;
    assert_eq!(req.call_id, "toolu_bg_rm");
    assert_eq!(req.tool, "Bash");
    let snap = s.snapshot(&chat.id).await;
    assert_eq!(snap.info.status, SessionStatus::AwaitingApproval);
    s.call::<Value>(Request::RespondApproval {
        session_id: chat.id.clone(),
        request_id: req.id,
        decision: ApprovalDecision::Allow,
    })
    .await;
    s.settle(&chat.id).await;
    let snap = s.snapshot(&chat.id).await;
    assert_eq!(
        texts(&snap.entries),
        [
            "Started it; I'll look when it's done.",
            "The background output is ready; reading it.",
            "Cleaning up.",
            "Done after carrying on."
        ],
        "{:?}",
        texts(&snap.entries)
    );
    // The plain read of its own output wasn't asked about.
    assert_eq!(
        s.logged("self-read"),
        [json!({"allowed": true, "why": null})]
    );
    assert_eq!(s.logged("self-risky")[0]["allowed"], true);
    assert_eq!(snap.info.status, SessionStatus::Idle);
    assert_eq!(s.runs().len(), 1, "the kept process did it all");
    assert_eq!(
        s.prompts().len(),
        1,
        "nothing was sent for the turn it took up"
    );
    // The next message goes to the same process, without that turn again.
    s.send(&chat.id, "next").await;
    s.settle(&chat.id).await;
    assert_eq!(s.runs().len(), 1);
    let last = s.prompts().last().unwrap().clone();
    assert!(!last.contains("Cleaning up"), "{last}");
    assert!(
        !snap.entries.iter().any(|e| matches!(&e.body,
            EntryBody::Notice { text, .. } if text.contains("no active turn"))),
        "{:?}",
        notices(&snap.entries)
    );
}

/// Declined, it is told plainly and carries on; Stop ends that turn as any.
#[tokio::test]
async fn a_turn_it_took_up_by_itself_can_be_declined_or_stopped() {
    let s = setup_with("[agent]\nreview_interval = 0\nclaude_keep_open_minutes = 10\n").await;
    std::fs::write(s.work.join("bg-output.txt"), "build ok").unwrap();
    let chat = s.chat(PermissionMode::Ask).await;
    let mut rx = s.engine.subscribe();
    s.send(&chat.id, "carry on after parking").await;
    let req = approval(&mut rx).await;
    s.call::<Value>(Request::RespondApproval {
        session_id: chat.id.clone(),
        request_id: req.id,
        decision: ApprovalDecision::Deny,
    })
    .await;
    s.settle(&chat.id).await;
    let risky = s.logged("self-risky").pop().unwrap();
    assert_eq!(risky["allowed"], false);
    let why = risky["hook"]["hookSpecificOutput"]["permissionDecisionReason"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(why.contains("declined"), "{why}");
    assert!(!why.contains("STOP"), "{why}");
    assert_eq!(
        texts(&s.snapshot(&chat.id).await.entries).last().unwrap(),
        "Not cleaned up."
    );
    // Stopped while it asks: that turn ends; the next message starts afresh.
    s.send(&chat.id, "carry on after parking").await;
    let _ = approval(&mut rx).await;
    s.call::<Value>(Request::Abort {
        session_id: chat.id.clone(),
    })
    .await;
    s.settle(&chat.id).await;
    let snap = s.snapshot(&chat.id).await;
    assert_eq!(snap.info.status, SessionStatus::Idle);
    assert!(
        notices(&snap.entries).iter().any(|(_, t)| t == "Stopped."),
        "{:?}",
        notices(&snap.entries)
    );
    assert_eq!(s.logged("self-risky").pop().unwrap()["allowed"], false);
    s.send(&chat.id, "after").await;
    s.settle(&chat.id).await;
    assert_eq!(s.runs().len(), 2, "{:#?}", s.runs());
}

/// Settings changed while it was kept: what it started by itself never goes
/// on under the old ones. It goes on in a new run, with the current settings.
#[tokio::test]
async fn work_it_takes_up_by_itself_after_a_settings_change_uses_the_new_settings() {
    let s = setup_with("[agent]\nreview_interval = 0\nclaude_keep_open_minutes = 10\n").await;
    std::fs::write(s.work.join("bg-output.txt"), "build ok").unwrap();
    let chat = s.chat(PermissionMode::Ask).await;
    s.send(&chat.id, "carry on after parking slowly").await;
    s.settle(&chat.id).await;
    let mut u = update(&chat.id);
    if let Request::UpdateSession { permission, .. } = &mut u {
        *permission = Some(PermissionMode::Default);
    }
    s.call::<SessionInfo>(u).await;
    // It takes the turn up by itself a moment later.
    for _ in 0..200 {
        if s.runs().len() == 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    s.settle(&chat.id).await;
    let runs = s.runs();
    assert_eq!(runs.len(), 2, "{runs:#?}");
    assert!(runs[0].contains("--permission-mode default"), "{}", runs[0]);
    assert!(
        runs[1].contains("--permission-mode acceptEdits"),
        "{}",
        runs[1]
    );
    assert!(runs[1].contains("--resume sess-"), "{}", runs[1]);
    let snap = s.snapshot(&chat.id).await;
    assert!(
        notices(&snap.entries)
            .iter()
            .any(|(_, t)| t.contains("settings changed meanwhile")),
        "{:?}",
        notices(&snap.entries)
    );
    assert_eq!(texts(&snap.entries).last().unwrap(), "Echo: Continue.");
    // Nothing of the old run's own turn ran under the old settings.
    assert!(s.logged("self-risky").iter().all(|r| r["allowed"] == false));
}

/// Stopped while idle: the Claude Code kept for its next turn is closed, so
/// nothing it starts by itself goes on in the chat the user just stopped.
#[tokio::test]
async fn stop_on_an_idle_chat_closes_its_kept_process() {
    let s = setup_with("[agent]\nreview_interval = 0\nclaude_keep_open_minutes = 10\n").await;
    std::fs::write(s.work.join("bg-output.txt"), "build ok").unwrap();
    let chat = s.chat(PermissionMode::Ask).await;
    s.send(&chat.id, "carry on after parking slowly").await;
    s.settle(&chat.id).await;
    s.call::<Value>(Request::Abort {
        session_id: chat.id.clone(),
    })
    .await;
    tokio::time::sleep(Duration::from_millis(3500)).await;
    s.settle(&chat.id).await;
    let snap = s.snapshot(&chat.id).await;
    assert_eq!(
        texts(&snap.entries),
        ["Started it; I'll look when it's done."],
        "{:?}",
        texts(&snap.entries)
    );
    assert!(s.logged("self-read").iter().all(|r| r["allowed"] == false));
    s.send(&chat.id, "after").await;
    s.settle(&chat.id).await;
    assert_eq!(s.runs().len(), 2, "a new process after the Stop");
}

/// After a Stop on this computer (even one resumed since), what a kept
/// process starts by itself from before it doesn't go on.
#[tokio::test]
async fn work_it_takes_up_by_itself_doesnt_go_on_across_a_stop_here() {
    let s = setup_with("[agent]\nreview_interval = 0\nclaude_keep_open_minutes = 10\n").await;
    std::fs::write(s.work.join("bg-output.txt"), "build ok").unwrap();
    let chat = s.chat(PermissionMode::Ask).await;
    s.send(&chat.id, "carry on after parking slowly").await;
    s.settle(&chat.id).await;
    // Stopped and resumed on this computer before it carries on.
    let paths = Paths::at(s.home.path());
    let latch = tessera_core::node::StopLatch::stop(&paths, 0).unwrap();
    tessera_core::node::StopLatch::set(&paths, false, latch.stops).unwrap();
    let mut told = false;
    for _ in 0..100 {
        let snap = s.snapshot(&chat.id).await;
        if notices(&snap.entries)
            .iter()
            .any(|(_, t)| t.contains("this computer was stopped since its last turn"))
        {
            told = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(told, "{:?}", notices(&s.snapshot(&chat.id).await.entries));
    s.settle(&chat.id).await;
    let snap = s.snapshot(&chat.id).await;
    assert_eq!(
        texts(&snap.entries),
        ["Started it; I'll look when it's done."]
    );
    assert!(s.logged("self-read").iter().all(|r| r["allowed"] == false));
    assert_eq!(snap.info.status, SessionStatus::Idle);
}

/// The turn started for a kept process's own work can fail before it takes
/// the process (here: `claude` is gone). It is started once, not again and
/// again while the process waits to be taken.
#[tokio::test]
async fn a_failing_turn_for_its_own_work_is_started_once() {
    let s = setup_with("[agent]\nreview_interval = 0\nclaude_keep_open_minutes = 10\n").await;
    std::fs::write(s.work.join("bg-output.txt"), "build ok").unwrap();
    let chat = s.chat(PermissionMode::Ask).await;
    s.send(&chat.id, "carry on after parking slowly").await;
    s.settle(&chat.id).await;
    let cli = s.fake.path().join("claude");
    std::fs::rename(&cli, s.fake.path().join("claude.away")).unwrap();
    // It carries on 2.5 s after its answer; give the failed turn time to repeat.
    tokio::time::sleep(Duration::from_millis(5500)).await;
    s.settle(&chat.id).await;
    let errors: Vec<String> = notices(&s.snapshot(&chat.id).await.entries)
        .into_iter()
        .filter(|(l, _)| *l == NoticeLevel::Error)
        .map(|(_, t)| t)
        .collect();
    assert_eq!(errors.len(), 1, "{errors:?}");
    std::fs::rename(s.fake.path().join("claude.away"), &cli).unwrap();
}

/// A helper's kept process works only through its runner (slot, budget,
/// report, worktree): what it starts by itself is closed, never a turn of the
/// helper's session outside it.
#[tokio::test]
async fn a_helpers_kept_process_doesnt_start_work_by_itself() {
    let s = setup_with("[agent]\nreview_interval = 0\nclaude_keep_open_minutes = 10\n").await;
    std::fs::write(s.work.join("bg-output.txt"), "build ok").unwrap();
    let mut rx = s.engine.subscribe();
    let chat = s.chat(PermissionMode::Ask).await;
    s.send(
        &chat.id,
        &helper_start(Some("claude-code/opus"), false, "carry on after parking"),
    )
    .await;
    let kids = bridged_children(&s, &chat.id, 1).await;
    // Its process would carry on 1 s after its answer.
    tokio::time::sleep(Duration::from_millis(3500)).await;
    s.settle(&kids[0].id).await;
    s.settle(&chat.id).await;
    let kid = s.snapshot(&kids[0].id).await;
    assert!(
        !texts(&kid.entries)
            .iter()
            .any(|t| t.contains("background output is ready")),
        "{:?}",
        texts(&kid.entries)
    );
    assert!(s.logged("self-read").iter().all(|r| r["allowed"] == false));
    while let Ok(e) = rx.try_recv() {
        if let Event::ApprovalRequested { request, .. } = e {
            assert_ne!(request.call_id, "toolu_bg_rm");
        }
    }
}

/// Something reached the chat while it was idle (a helper's report whose
/// batch isn't done starts no turn): when Claude Code then carries on by
/// itself, that is sent to it too, never skipped by the resume point.
#[tokio::test]
async fn a_turn_it_takes_up_by_itself_still_gets_what_reached_the_chat() {
    let s = setup_with("[agent]\nreview_interval = 0\nclaude_keep_open_minutes = 10\n").await;
    s._mock.route(
        "QUICK-REPORT",
        Reply::Slow(Duration::from_millis(800), "Quick report text.".into()),
    );
    s._mock.route(
        "SLOW-REPORT",
        Reply::Slow(Duration::from_secs(6), "Slow report text.".into()),
    );
    let chat = s.chat(PermissionMode::Default).await;
    s.send(&chat.id, "two helpers then carry on").await;
    let got = |what: &'static str| {
        let s = &s;
        async move {
            for _ in 0..300 {
                if s.prompts().iter().any(|p| p.contains(what)) {
                    return true;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            false
        }
    };
    assert!(got("Quick report text.").await, "{:#?}", s.prompts());
    assert!(got("Slow report text.").await, "{:#?}", s.prompts());
    s.settle(&chat.id).await;
    let snap = s.snapshot(&chat.id).await;
    assert!(
        texts(&snap.entries).contains(&"Carrying on by myself.".to_string()),
        "{:?}",
        texts(&snap.entries)
    );
}

/// A helper running on Claude Code waits for an approval. Its parent may
/// answer it where Claude Code's own policy would run the action unasked in
/// the parent's mode as it is when it answers: here the user put the parent
/// in YOLO after the helper asked. An action Claude Code asks about in every
/// mode stays the user's: the parent can only decline it.
#[tokio::test]
async fn a_helper_on_claude_code_waits_and_its_parent_answers_with_its_own_authority() {
    let s = setup().await;
    s._mock.route(
        "<CC-HELPER-PARENT>",
        Reply::Tools(vec![
            (
                "agent".into(),
                json!({"tasks": [{"name": "Cleaner", "prompt": "risky helper command classified", "model": "claude-code/opus"}]}),
            ),
            ("bash".into(), json!({"command": "sleep 4"})),
        ]),
    );
    // Approves the first wait, then works a moment while the helper asks again.
    s._mock.route(
        "<CC-HELPER-PARENT>",
        Reply::Tools(vec![
            (
                "agents".into(),
                json!({"action": "approve", "name": "Cleaner"}),
            ),
            ("bash".into(), json!({"command": "sleep 3"})),
        ]),
    );
    // Then the second wait, Claude Code's own question: approve is refused.
    s._mock.route(
        "<CC-HELPER-PARENT>",
        Reply::Tools(vec![(
            "agents".into(),
            json!({"action": "approve", "name": "Cleaner"}),
        )]),
    );
    s._mock.route(
        "<CC-HELPER-PARENT>",
        Reply::Tools(vec![(
            "agents".into(),
            json!({"action": "decline", "name": "Cleaner"}),
        )]),
    );
    s._mock
        .route("<CC-HELPER-PARENT>", Reply::Text("Parent done.".into()));
    // Claude Code's models are known (a helper may run on one).
    let _: ClaudeCodeInfo = s
        .call(Request::ClaudeCodeStatus {
            refresh: true,
            provider: None,
        })
        .await;
    let mut rx = s.engine.subscribe();
    let chat = s.session_on("mock/m", None, false).await;
    s.send(&chat.id, "Tidy with a Claude helper <CC-HELPER-PARENT>")
        .await;
    let first = approval(&mut rx).await;
    assert_eq!(first.call_id, "toolu_hr");
    // The user puts the parent in YOLO while the helper waits.
    let mut u = update(&chat.id);
    if let Request::UpdateSession { permission, .. } = &mut u {
        *permission = Some(PermissionMode::Auto);
    }
    s.call::<SessionInfo>(u).await;
    let second = approval(&mut rx).await;
    assert_eq!(second.call_id, "toolu_hc");
    for _ in 0..300 {
        if !s.logged("helper-classified").is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    s.settle(&chat.id).await;
    let risky = s.logged("helper-risky").pop().unwrap();
    assert_eq!(risky["allowed"], true, "{risky}");
    let classified = s.logged("helper-classified").pop().unwrap();
    assert_eq!(classified["allowed"], false, "{classified}");
    assert!(
        classified["message"]
            .as_str()
            .unwrap()
            .contains("The chat that started you declined"),
        "{classified}"
    );
    let results: Vec<(bool, String)> = s
        .snapshot(&chat.id)
        .await
        .entries
        .iter()
        .filter_map(|e| match &e.body {
            EntryBody::ToolResult {
                tool,
                is_error,
                content,
                ..
            } if tool == "agents" => Some((*is_error, plain_text(content))),
            _ => None,
        })
        .collect();
    assert!(results[0].1.starts_with("Approved"), "{results:?}");
    assert!(
        results[1].0 && results[1].1.contains("can't approve"),
        "{results:?}"
    );
    assert!(results[2].1.starts_with("Declined"), "{results:?}");
}

#[tokio::test]
async fn fast_mode_reaches_claude_code() {
    let s = setup().await;
    let chat = s.chat(PermissionMode::Default).await;
    // Its model list says which models have fast mode (Opus here).
    s.send(&chat.id, "hello").await;
    s.settle(&chat.id).await;
    assert!(!s.runs()[0].contains("--settings"));
    let mut u = update(&chat.id);
    if let Request::UpdateSession { fast, .. } = &mut u {
        *fast = Some(true);
    }
    s.call::<SessionInfo>(u).await;
    s.send(&chat.id, "fast now").await;
    s.settle(&chat.id).await;
    assert_eq!(
        s.last_arg("--settings").as_deref(),
        Some(r#"{"fastMode":true}"#)
    );
    // Not available on the account: the chat says why, and runs as usual.
    std::fs::write(s.fake.path().join("fast_off"), "").unwrap();
    s.send(&chat.id, "fast again").await;
    s.settle(&chat.id).await;
    let snap = s.snapshot(&chat.id).await;
    assert!(
        snap.entries.iter().any(|e| matches!(&e.body,
            EntryBody::Notice { text, .. } if text.contains("usage credits"))),
        "no notice"
    );
}

// ---- every way a turn can fail reaches the user (batch 15) ----------------------

fn notices(entries: &[Entry]) -> Vec<(NoticeLevel, String)> {
    entries
        .iter()
        .filter_map(|e| match &e.body {
            EntryBody::Notice { level, text, .. } => Some((*level, text.clone())),
            _ => None,
        })
        .collect()
}

fn last_notice(entries: &[Entry]) -> (NoticeLevel, String) {
    notices(entries).pop().expect("a notice")
}

fn install_fake(path: &Path) {
    std::fs::write(path, FAKE).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

/// After a failure the chat takes the next message as usual (the one the
/// failed turn left unanswered comes along with it).
async fn goes_on(s: &Setup, id: &str, text: &str) {
    s.send(id, text).await;
    s.settle(id).await;
    let snap = s.snapshot(id).await;
    assert_eq!(
        snap.info.status,
        SessionStatus::Idle,
        "{:?}",
        notices(&snap.entries)
    );
    let all = texts(&snap.entries);
    assert!(
        all.last()
            .is_some_and(|t| t.starts_with("Echo: ") && t.contains(text)),
        "{all:?}"
    );
}

#[tokio::test]
async fn a_crash_says_so_keeps_what_was_said_and_the_chat_goes_on() {
    let s = setup().await;
    let chat = s.chat(PermissionMode::Default).await;
    s.send(&chat.id, "crash midway").await;
    s.settle(&chat.id).await;
    let snap = s.snapshot(&chat.id).await;
    assert_eq!(snap.info.status, SessionStatus::Error);
    // What it said before it died stays, and the call it had started is closed.
    assert!(
        texts(&snap.entries).contains(&"Starting the build.".to_string()),
        "{:?}",
        texts(&snap.entries)
    );
    assert!(snap.entries.iter().any(|e| matches!(&e.body,
        EntryBody::ToolResult { call_id, is_error: true, content, .. }
            if call_id == "toolu_crash"
                && matches!(content.first(), Some(ContentPart::Text { text }) if text.contains("ended before")))));
    let (level, text) = last_notice(&snap.entries);
    assert_eq!(level, NoticeLevel::Error);
    assert!(
        text.contains("killed")
            && text.contains("signal 9")
            && text.ends_with("Send a message to try again."),
        "{text}"
    );
    goes_on(&s, &chat.id, "hello again").await;
}

#[tokio::test]
async fn plain_text_errors_and_an_old_claude_code_are_explained() {
    let s = setup().await;
    let chat = s.chat(PermissionMode::Default).await;
    goes_on(&s, &chat.id, "hello").await;
    // An error printed as plain text instead of its JSON.
    s.send(&chat.id, "print plainly").await;
    s.settle(&chat.id).await;
    let snap = s.snapshot(&chat.id).await;
    assert_eq!(snap.info.status, SessionStatus::Error);
    let (_, text) = last_notice(&snap.entries);
    assert!(
        text.contains("Fatal: the plain words of a failure") && text.contains("exit code 3"),
        "{text}"
    );
    // A version that doesn't know a flag Tessera uses: update it.
    std::fs::write(s.fake.path().join("old"), "").unwrap();
    s.send(&chat.id, "hello").await;
    s.settle(&chat.id).await;
    let snap = s.snapshot(&chat.id).await;
    assert_eq!(snap.info.status, SessionStatus::Error);
    let (_, text) = last_notice(&snap.entries);
    assert!(
        text.contains("too old") && text.contains("(9.9.9)") && text.contains("claude update"),
        "{text}"
    );
    std::fs::remove_file(s.fake.path().join("old")).unwrap();
    goes_on(&s, &chat.id, "updated now").await;
    // It never got to the conversation: the chat still continues the same one.
    let runs = s.runs();
    let first = runs[0].clone();
    let last = runs.last().unwrap();
    assert!(
        !first.contains("--resume") && last.contains("--resume sess-"),
        "{last}"
    );
    assert!(
        !texts(&s.snapshot(&chat.id).await.entries)
            .iter()
            .any(|t| t.contains("saw-digest"))
    );
}

#[tokio::test]
async fn odd_lines_retries_and_fallbacks_never_stop_a_turn() {
    let s = setup().await;
    let chat = s.chat(PermissionMode::Default).await;
    s.send(&chat.id, "garble please").await;
    s.settle(&chat.id).await;
    let snap = s.snapshot(&chat.id).await;
    assert_eq!(snap.info.status, SessionStatus::Idle);
    assert!(texts(&snap.entries).contains(&"Recovered fine.".to_string()));
    let n = notices(&snap.entries);
    // Its retries, once (not three times), and the model it fell back to.
    let retries: Vec<_> = n
        .iter()
        .filter(|(_, t)| t.contains("trying again"))
        .collect();
    assert_eq!(
        retries,
        [&(
            NoticeLevel::Warning,
            "Claude's servers didn't answer (they are overloaded); Claude Code is trying again (attempt 1 of 10).".to_string()
        )],
        "{n:?}"
    );
    assert!(
        n.contains(&(
            NoticeLevel::Warning,
            "Claude Code: Opus is busy, so this turn uses Sonnet.".to_string()
        )),
        "{n:?}"
    );
    // Requests Tessera doesn't serve are answered all the same (it would wait forever).
    s.send(&chat.id, "odd requests").await;
    s.settle(&chat.id).await;
    let snap = s.snapshot(&chat.id).await;
    assert!(
        texts(&snap.entries).contains(
            &"Answered: hook success, other server refused, new request error.".to_string()
        ),
        "{:?}",
        texts(&snap.entries)
    );
}

#[tokio::test]
async fn an_api_error_or_an_empty_answer_is_never_silent() {
    let s = setup().await;
    let chat = s.chat(PermissionMode::Default).await;
    // Its request failed: said in a message of its own, not in the result.
    s.send(&chat.id, "quiet api error").await;
    s.settle(&chat.id).await;
    let snap = s.snapshot(&chat.id).await;
    assert_eq!(snap.info.status, SessionStatus::Error);
    let (level, text) = last_notice(&snap.entries);
    assert_eq!(level, NoticeLevel::Error);
    assert!(text.contains("API Error: 500"), "{text}");
    // A turn that ends with nothing to show.
    s.send(&chat.id, "say nothing").await;
    s.settle(&chat.id).await;
    let snap = s.snapshot(&chat.id).await;
    assert_eq!(snap.info.status, SessionStatus::Idle);
    assert_eq!(
        last_notice(&snap.entries),
        (
            NoticeLevel::Warning,
            "Claude Code ended its turn without a reply. Send a message to try again.".to_string()
        )
    );
    // …or with its answer only in its result (a command's output).
    s.send(&chat.id, "show the cost").await;
    s.settle(&chat.id).await;
    let snap = s.snapshot(&chat.id).await;
    assert_eq!(
        last_notice(&snap.entries),
        (
            NoticeLevel::Info,
            "Claude Code: Total cost: $0.12".to_string()
        )
    );
    // An ordinary answer gets no such note.
    goes_on(&s, &chat.id, "hello").await;
    let snap = s.snapshot(&chat.id).await;
    assert_eq!(
        notices(&snap.entries).len(),
        3,
        "{:?}",
        notices(&snap.entries)
    );
}

#[tokio::test]
async fn when_tesseras_tools_dont_connect_it_says_so() {
    let s = setup().await;
    let chat = s.chat(PermissionMode::Default).await;
    std::fs::write(s.fake.path().join("mcp_failed"), "").unwrap();
    goes_on(&s, &chat.id, "hello").await;
    let (level, text) = last_notice(&s.snapshot(&chat.id).await.entries);
    assert_eq!(level, NoticeLevel::Warning);
    assert!(text.contains("didn't connect"), "{text}");
}

#[tokio::test]
async fn a_kept_process_that_ended_is_replaced_on_the_same_conversation() {
    let s = setup_with("[agent]\nreview_interval = 0\nclaude_keep_open_minutes = 10\n").await;
    let chat = s.chat(PermissionMode::Default).await;
    goes_on(&s, &chat.id, "first").await;
    // The kept process is gone when the next message comes: a new one takes it, on the
    // same Claude Code conversation (not a new one told the story so far).
    goes_on(&s, &chat.id, "die quietly").await;
    let runs = s.runs();
    assert_eq!(runs.len(), 2, "{runs:#?}");
    assert!(runs[1].contains("--resume sess-"), "{}", runs[1]);
    let snap = s.snapshot(&chat.id).await;
    assert!(
        notices(&snap.entries).is_empty(),
        "{:?}",
        notices(&snap.entries)
    );
    assert!(
        !texts(&snap.entries)
            .iter()
            .any(|t| t.contains("saw-digest"))
    );
}

#[tokio::test]
async fn a_missing_claude_code_or_folder_is_explained() {
    let s = setup().await;
    let chat = s.chat(PermissionMode::Default).await;
    // Not installed (any more).
    let claude = s.fake.path().join("claude");
    std::fs::remove_file(&claude).unwrap();
    s.send(&chat.id, "hello").await;
    s.settle(&chat.id).await;
    let snap = s.snapshot(&chat.id).await;
    assert_eq!(snap.info.status, SessionStatus::Error);
    let (_, text) = last_notice(&snap.entries);
    assert!(text.starts_with("Install Claude Code on"), "{text}");
    install_fake(&claude);
    goes_on(&s, &chat.id, "installed now").await;
    // The chat's folder is gone.
    let gone = s.work.join("gone");
    std::fs::create_dir_all(&gone).unwrap();
    let mut u = update(&chat.id);
    if let Request::UpdateSession { cwd, .. } = &mut u {
        *cwd = Some(gone.display().to_string());
    }
    s.call::<SessionInfo>(u).await;
    std::fs::remove_dir(&gone).unwrap();
    s.send(&chat.id, "hello there").await;
    s.settle(&chat.id).await;
    let snap = s.snapshot(&chat.id).await;
    assert_eq!(snap.info.status, SessionStatus::Error);
    let (_, text) = last_notice(&snap.entries);
    assert!(
        text.contains("This chat's folder")
            && text.contains("doesn't exist")
            && text.contains("gone"),
        "{text}"
    );
    let mut u = update(&chat.id);
    if let Request::UpdateSession { cwd, .. } = &mut u {
        *cwd = Some(s.work.display().to_string());
    }
    s.call::<SessionInfo>(u).await;
    goes_on(&s, &chat.id, "back home").await;
}

/// A Claude Code chat hands work to a bot (Tessera's `message_bot`, through
/// the bridge) (batch 17): the call returns at once, so Claude Code's turn
/// goes on and ends; the bot works on Tessera's own engine meanwhile; its
/// reply comes back into the chat as the bot's message, and Claude Code
/// takes it up in a new turn by itself.
#[tokio::test]
async fn a_bot_hand_off_from_claude_code_comes_back_in_a_new_turn() {
    let s = setup().await;
    let forge = s.save_bot("Forge", "mock/m", ToolPreset::Coding).await;
    s._mock.route(
        "FORGE check main",
        Reply::Slow(
            Duration::from_millis(1200),
            "Main is green: 21 passed.".into(),
        ),
    );
    let chat = s.chat(PermissionMode::Default).await;
    s.send(
        &chat.id,
        r#"tessera message_bot {"bot":"@forge","message":"FORGE check main"}"#,
    )
    .await;
    s.settle(&chat.id).await;
    let called = s.logged("called");
    let first = called.last().unwrap();
    assert!(
        first["name"] == "message_bot"
            && first["error"] == false
            && first["text"]
                .as_str()
                .unwrap()
                .starts_with("Sent to @forge"),
        "{first}"
    );
    // The reply arrives as Forge's message, and Claude Code is given it.
    let mut got = None;
    for _ in 0..200 {
        let snap = s.snapshot(&chat.id).await;
        let reply = snap.entries.iter().find_map(|e| match &e.body {
            EntryBody::User {
                content,
                author: Some(a),
            } if *a == forge.id => Some(plain_text(content)),
            _ => None,
        });
        let taken = s
            .logged("in")
            .iter()
            .any(|m| m.to_string().contains("Reply from @forge"));
        if reply.is_some() && taken && snap.info.status == SessionStatus::Idle {
            got = reply;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let reply = got.expect("Forge's reply reached the chat and Claude Code");
    assert!(
        reply.starts_with("[Reply from @forge · ") && reply.ends_with("Main is green: 21 passed."),
        "{reply}"
    );
    let told = s
        .logged("in")
        .into_iter()
        .rev()
        .find(|m| m.to_string().contains("Reply from @forge"))
        .unwrap()
        .to_string();
    assert!(told.contains("[@forge]:"), "labelled as Forge's: {told}");
    assert!(s.runs().len() >= 2, "a new turn took it up");
}

/// Another Claude account (batch 17): its own Claude Code folder, named by
/// Tessera and given to `claude` as `CLAUDE_CONFIG_DIR` for that account's
/// runs only; signed in by the user (Tessera only shows the command and
/// never writes in it); its own models; and, if the user wants, a chat on an
/// account whose plan is used up goes on with the next one.
#[tokio::test]
async fn another_claude_account_runs_with_its_own_folder() {
    let s = setup().await;
    // The accounts' folders sit next to the throwaway home: gone with the test, even a failed one.
    struct Gone(PathBuf);
    impl Drop for Gone {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _gone = Gone(Paths::at(s.home.path()).claude_accounts_dir());
    let work: ProviderInfo = s
        .call(Request::AddAccount {
            kind: ProviderKind::ClaudeCode,
            name: "Work".into(),
        })
        .await;
    assert_eq!(
        (work.id.as_str(), work.name.as_str()),
        ("claude-code-work", "Claude (Work)")
    );
    assert_eq!(
        work.base_url,
        s.fake.path().join("claude").display().to_string(),
        "the same `claude` as the usual account"
    );
    assert!(!work.authenticated, "not signed in yet");
    let folder = Paths::at(s.home.path())
        .claude_accounts_dir()
        .join("claude-code-work");
    assert!(
        !folder.starts_with(s.home.path()),
        "outside Tessera's own folders (hidden from the sandbox)"
    );
    let info: ClaudeCodeInfo = s
        .call(Request::ClaudeCodeStatus {
            refresh: true,
            provider: Some(work.id.clone()),
        })
        .await;
    let run = info.sign_in.clone().expect("the command to sign it in");
    assert!(
        !info.signed_in
            && run.contains(&folder.display().to_string())
            && run.contains("CLAUDE_CONFIG_DIR"),
        "{info:?}"
    );
    assert!(info.hint.contains(&run), "{}", info.hint);
    assert!(folder.is_dir(), "made for Claude Code to fill");
    let inside: Vec<String> = std::fs::read_dir(&folder)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        inside.is_empty(),
        "Tessera writes nothing in it: {inside:?}"
    );
    // The user signs in there with Claude Code's own login.
    std::fs::write(folder.join("logged-in"), "").unwrap();
    let info: ClaudeCodeInfo = s
        .call(Request::ClaudeCodeStatus {
            refresh: true,
            provider: Some(work.id.clone()),
        })
        .await;
    assert!(info.signed_in && info.account.as_deref() == Some("work@example.com"));
    let usual: ClaudeCodeInfo = s
        .call(Request::ClaudeCodeStatus {
            refresh: false,
            provider: None,
        })
        .await;
    assert_eq!(
        usual.account.as_deref(),
        Some("ann@example.com"),
        "the usual account is its own"
    );
    assert!(usual.sign_in.is_none());
    let providers: Vec<ProviderInfo> = s.call(Request::ListProviders { refresh: false }).await;
    let w = providers.iter().find(|p| p.id == work.id).unwrap();
    assert!(w.authenticated && w.account.as_deref() == Some("work@example.com"));
    // Signing in is never Tessera's: start_oauth only points the way.
    let e = s
        .engine
        .handle(
            Request::StartOauth {
                provider: work.id.clone(),
            },
            None,
        )
        .await
        .unwrap_err();
    assert!(
        e.message.contains("Ready") || e.message.contains("CLAUDE_CONFIG_DIR"),
        "{}",
        e.message
    );

    // A chat on each account: only the work one's turns get its folder.
    // (Turns, not the titles each new chat gets in the background meanwhile.)
    let accounts_of = |s: &Setup| -> Vec<Value> {
        s.calls()
            .iter()
            .filter(|c| {
                c["kind"] == "account"
                    && c["value"]["args"]
                        .as_array()
                        .is_some_and(|a| a.iter().any(|x| x == "--input-format"))
            })
            .map(|c| c["value"]["dir"].clone())
            .collect()
    };
    let before = accounts_of(&s).len();
    let on_work: SessionInfo = s
        .call(Request::CreateSession {
            cwd: Some(s.work.display().to_string()),
            model: Some(ModelRef {
                provider: work.id.clone(),
                model: "opus".into(),
            }),
            title: None,
            bot_id: None,
            private: false,
            lean: false,
        })
        .await;
    s.send(&on_work.id, "hello work").await;
    s.settle(&on_work.id).await;
    let runs = accounts_of(&s)[before..].to_vec();
    assert!(
        !runs.is_empty()
            && runs
                .iter()
                .all(|a| a == &Value::String(folder.display().to_string())),
        "{runs:?}"
    );
    let usual_chat = s.chat(PermissionMode::Default).await;
    let before = accounts_of(&s).len();
    s.send(&usual_chat.id, "hello usual").await;
    s.settle(&usual_chat.id).await;
    let usual_runs = accounts_of(&s)[before..].to_vec();
    assert!(
        !usual_runs.is_empty() && usual_runs.iter().all(Value::is_null),
        "the usual account runs as before: {usual_runs:?}"
    );

    // Going on with the next account at a limit (asked for).
    let set: AccountSettings = s
        .call(Request::SetAccounts {
            continue_on_next: true,
        })
        .await;
    assert!(set.continue_on_next);
    s.send(&usual_chat.id, "use up the plan now").await;
    tokio::time::timeout(Duration::from_secs(30), s.engine.wait_idle(&usual_chat.id))
        .await
        .expect("it didn't go on with the next account");
    let snap = s.snapshot(&usual_chat.id).await;
    assert_eq!(
        snap.info.model.provider, work.id,
        "the chat moved, for good"
    );
    assert!(
        texts(&snap.entries)
            .last()
            .unwrap()
            .starts_with("Answered on claude-code-work saw-digest"),
        "{:?}",
        texts(&snap.entries)
    );
    assert!(snap.entries.iter().any(|e| matches!(&e.body,
        EntryBody::Notice { text, .. } if text.contains("Claude (your subscription) reached its usage limit; this chat goes on with Claude (Work)"))));
}

// Tessera helpers through the Claude bridge, never the real Claude CLI.
fn helper_start(model: Option<&str>, wait: bool, prompt: &str) -> String {
    format!(
        "tessera agents {}",
        json!({"action":"start", "wait":wait,
        "tasks":[{"name":"Other model", "prompt":prompt, "model":model}]})
    )
}

async fn bridged_children(s: &Setup, parent: &str, n: usize) -> Vec<SessionInfo> {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let kids = s.snapshot(parent).await.children;
            if kids.len() == n && kids.iter().all(|c| c.parent_call.is_some()) {
                return kids;
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    })
    .await
    .expect("bridged helper sessions")
}

#[tokio::test]
async fn bridged_agents_waits_on_another_model_and_returns_its_result() {
    let s = setup().await;
    let chat = s.chat(PermissionMode::ReadOnly).await;
    s.send(
        &chat.id,
        &helper_start(Some("mock/m"), true, "Check the files"),
    )
    .await;
    s.settle(&chat.id).await;
    let snap = s.snapshot(&chat.id).await;
    let kids = bridged_children(&s, &chat.id, 1).await;
    let child = s.snapshot(&kids[0].id).await;
    assert_eq!(child.info.model.provider, "mock");
    assert_eq!(child.info.parent_call.as_deref(), Some("toolu_t0"));
    assert_eq!(child.info.parent_id.as_deref(), Some(chat.id.as_str()));
    assert_eq!(child.info.permission, PermissionMode::ReadOnly);
    assert_eq!(child.info.kind, SessionKind::Subagent);
    let called = s.logged("called").pop().unwrap();
    assert_eq!(called["error"], false, "{called}");
    assert!(
        called["text"]
            .as_str()
            .unwrap()
            .contains("hello from the other model")
    );
    let result = snap
        .entries
        .iter()
        .find_map(|e| match &e.body {
            EntryBody::ToolResult {
                call_id,
                tool,
                details: Some(d),
                ..
            } if tool == "agents" => Some((call_id, d)),
            _ => None,
        })
        .unwrap();
    assert_eq!(Some(result.0.as_str()), child.info.parent_call.as_deref());
    assert_eq!(result.1["subagents"][0]["session_id"], child.info.id);
    assert_eq!(result.1["subagents"][0]["background"], false);
    assert_eq!(snap.info.usage.input, 100 + child.info.usage.input);
    assert_eq!(snap.info.usage.output, 20 + child.info.usage.output);
    let req = s._mock.agent_requests().pop().unwrap();
    let tools = req["tools"].to_string();
    assert!(!tools.contains("\"name\":\"agent\"") && !tools.contains("\"name\":\"agents\""));
    assert!(!tools.contains("\"name\":\"write\"") && !tools.contains("\"name\":\"bash\""));
    assert_eq!(s.runs().len(), 1, "no second Claude process for the helper");
    assert_eq!(child.info.helper_owner, Some(HelperOwner::Tessera));
    s._mock.push(Reply::Text("continued independently".into()));
    s.engine
        .handle(
            Request::Send {
                session_id: child.info.id.clone(),
                content: vec![ContentPart::text("hello")],
                delivery: Delivery::Auto,
                attachments: Vec::new(),
            },
            None,
        )
        .await
        .unwrap();
    s.settle(&child.info.id).await;
    assert_eq!(
        texts(&s.snapshot(&child.info.id).await.entries)
            .last()
            .unwrap(),
        "continued independently"
    );
}

/// A helper of a Claude Code chat waits for the user's approval: the chat is
/// told at once (here, after its turn ended: a turn of its own), sees the wait
/// in `agents status`, can't approve what it would have to ask about itself,
/// and declines it; the helper is told so plainly and carries on.
#[tokio::test]
async fn a_claude_chat_is_told_of_its_helpers_wait_and_may_decline_it() {
    let s = setup().await;
    s._mock.route(
        "HELPER-WAIT",
        Reply::Tools(vec![("bash".into(), json!({"command": "git clean -nf"}))]),
    );
    s._mock.route(
        "HELPER-WAIT",
        Reply::Text("Not tidied: the clean-up was declined.".into()),
    );
    let mut rx = s.engine.subscribe();
    let chat = s.chat(PermissionMode::Default).await;
    s.send(
        &chat.id,
        &helper_start(Some("mock/m"), false, "HELPER-WAIT tidy"),
    )
    .await;
    let req = approval(&mut rx).await;
    assert_eq!(req.tool, "bash");
    let kids = bridged_children(&s, &chat.id, 1).await;
    let told = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if let Some(v) = s.logged("helper-wait").pop() {
                return v;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the chat was told of its helper's wait");
    let note = told["note"].as_str().unwrap();
    assert!(
        note.contains("Helper \"Other model\" waits for the user's approval ")
            && note.contains(": bash git clean -nf"),
        "{note}"
    );
    assert!(
        told["status"]
            .as_str()
            .unwrap()
            .contains(": bash git clean -nf"),
        "{told}"
    );
    assert_eq!(told["approve_error"], true, "{told}");
    assert!(told["approve"].as_str().unwrap().contains("can't approve"));
    assert_eq!(told["decline_error"], false, "{told}");
    s.settle(&kids[0].id).await;
    let kid = s.snapshot(&kids[0].id).await;
    assert!(
        kid.entries.iter().any(|e| matches!(&e.body,
            EntryBody::ToolResult { tool, content, is_error: true, .. }
                if tool == "bash" && plain_text(content).contains("The chat that started you declined"))),
        "{:?}",
        texts(&kid.entries)
    );
    s.settle(&chat.id).await;
}

#[tokio::test]
async fn bridged_agents_background_returns_early_and_reports_in_a_new_turn() {
    let s = setup().await;
    s._mock.route(
        "BRIDGE slow report",
        Reply::Slow(Duration::from_secs(2), "Other model report".into()),
    );
    let chat = s.chat(PermissionMode::Default).await;
    s.send(
        &chat.id,
        &helper_start(Some("mock/m"), false, "BRIDGE slow report"),
    )
    .await;
    s.settle(&chat.id).await;
    let ack = s.logged("called")[0]["text"].as_str().unwrap().to_string();
    assert!(ack.contains("Accepted 1 subagent"), "{ack}");
    assert!(
        ack.contains("queued — waiting for a provider lane or helper slot"),
        "{ack}"
    );
    assert!(
        ack.contains("role: general; permission: Default; effective tools: read"),
        "{ack}"
    );
    assert!(ack.contains("bash, job"), "{ack}");
    let kids = bridged_children(&s, &chat.id, 1).await;
    assert_eq!(
        s.snapshot(&kids[0].id).await.info.status,
        SessionStatus::Running
    );
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if s.logged("in")
                .iter()
                .any(|m| m.to_string().contains("Report from subagent"))
                && s.runs().len() >= 2
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("report taken up by Claude Code");
    s.settle(&chat.id).await;
    let snap = s.snapshot(&chat.id).await;
    assert!(snap.entries.iter().any(|e| matches!(&e.body,
        EntryBody::User { author: Some(a), content } if a == &format!("subagent:{}", kids[0].id) && plain_text(content).contains("Other model report"))));
}

#[tokio::test]
async fn bridged_agents_stop_chat_or_helper_including_queued_ones() {
    for (wait, at_once) in [(true, 1), (false, 1), (true, 4), (false, 4)] {
        let s = setup_with(&format!(
            "[agent]\nreview_interval = 0\nmax_subagents = {at_once}\n"
        ))
        .await;
        s._mock.route(
            "BRIDGE hang",
            Reply::Slow(Duration::from_secs(60), "too late".into()),
        );
        let chat = s.chat(PermissionMode::Default).await;
        s.send(
            &chat.id,
            &format!(
                "tessera agents {}",
                json!({"action":"start", "wait":wait, "tasks":[
                    {"name":"First", "prompt":"BRIDGE hang", "model":"mock/m"},
                    {"name":"Queued", "prompt":"BRIDGE queued", "model":"mock/m"}
                ]})
            ),
        )
        .await;
        let kids = bridged_children(&s, &chat.id, 2).await;
        tokio::time::timeout(Duration::from_secs(5), async {
            while s._mock.agent_requests().is_empty() {
                tokio::time::sleep(Duration::from_millis(30)).await;
            }
        })
        .await
        .unwrap();
        // Give a wrongly unblocked second helper time to reach the provider.
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert_eq!(
            s._mock.agent_requests().len(),
            1,
            "helper slots or the single-slot local provider lane enforced"
        );
        let queued = kids.iter().find(|c| c.title.contains("Queued")).unwrap();
        s.call::<Value>(Request::Abort {
            session_id: queued.id.clone(),
        })
        .await;
        s.engine.wait_idle(&queued.id).await;
        assert_eq!(
            s.snapshot(&queued.id).await.info.status,
            SessionStatus::Interrupted
        );
        s.call::<Value>(Request::Abort {
            session_id: chat.id.clone(),
        })
        .await;
        s.settle(&chat.id).await;
        for kid in kids {
            s.engine.wait_idle(&kid.id).await;
            assert_eq!(
                s.snapshot(&kid.id).await.info.status,
                SessionStatus::Interrupted
            );
        }
        assert_eq!(
            s._mock.agent_requests().len(),
            1,
            "queued helper never requested a model"
        );
    }
}

#[tokio::test]
async fn bridged_agents_default_model_and_unknown_models_are_deliberate() {
    let s = setup().await;
    let chat = s.chat(PermissionMode::Default).await;
    for (model, expected) in [
        (None, "server default is Claude Code"),
        (Some("absent/no-such-model"), "unknown model"),
        (Some("mock/no-such-model"), "unknown model"),
    ] {
        s.send(&chat.id, &helper_start(model, true, "Check files"))
            .await;
        s.settle(&chat.id).await;
        let called = s.logged("called").pop().unwrap();
        assert_eq!(called["error"], true);
        let text = called["text"].as_str().unwrap();
        assert!(text.contains(expected) && text.contains("mock/m"), "{text}");
        assert!(s.snapshot(&chat.id).await.children.is_empty());
    }
    s.call::<Value>(Request::SetDefaultModel {
        model: ModelRef {
            provider: "mock".into(),
            model: "m".into(),
        },
    })
    .await;
    s.send(&chat.id, &helper_start(None, true, "Check files"))
        .await;
    s.settle(&chat.id).await;
    assert_eq!(
        bridged_children(&s, &chat.id, 1).await[0].model.provider,
        "mock"
    );
}

#[tokio::test]
async fn bridged_agents_use_agent_type_models_and_reject_unknown_type_models() {
    let s = setup().await;
    let defs = s.work.join(".tessera/agents");
    std::fs::create_dir_all(&defs).unwrap();
    for (name, model) in [("other", "mock/m"), ("missing", "absent/model")] {
        std::fs::write(defs.join(format!("{name}.md")), format!("---\nname: {name}\ndescription: Check files\nmodel: {model}\n---\nCheck carefully.\n")).unwrap();
    }
    let chat = s.chat(PermissionMode::Default).await;
    for (agent, ok) in [("missing", false), ("other", true)] {
        s.send(&chat.id, &format!("tessera agents {}", json!({"action":"start", "wait":true, "tasks":[{"name":"Typed", "prompt":"Check files", "agent":agent}]}))).await;
        s.settle(&chat.id).await;
        assert_eq!(s.logged("called").pop().unwrap()["error"], !ok);
    }
    assert_eq!(
        bridged_children(&s, &chat.id, 1).await[0].model.provider,
        "mock"
    );
}

#[tokio::test]
async fn bridged_agents_are_removed_with_the_chat_even_while_running() {
    let s = setup().await;
    s._mock.route(
        "BRIDGE delete",
        Reply::Slow(Duration::from_secs(60), "late report".into()),
    );
    let chat = s.chat(PermissionMode::Default).await;
    s.send(
        &chat.id,
        &helper_start(Some("mock/m"), false, "BRIDGE delete"),
    )
    .await;
    s.settle(&chat.id).await;
    let child = bridged_children(&s, &chat.id, 1).await.pop().unwrap();
    s.call::<Value>(Request::DeleteSession {
        session_id: chat.id.clone(),
    })
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    for id in [chat.id, child.id] {
        let err = s
            .engine
            .handle(
                Request::GetSession {
                    session_id: id,
                    since_seq: None,
                },
                None,
            )
            .await
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::NotFound);
    }
}

#[tokio::test]
async fn claude_native_helpers_cannot_spawn_bridged_grandchildren() {
    let s = setup().await;
    let chat = s.chat(PermissionMode::Default).await;
    s.send(&chat.id, "delegate nested").await;
    s.settle(&chat.id).await;
    let call = s
        .logged("called")
        .into_iter()
        .find(|c| c["name"] == "agents")
        .unwrap();
    assert_eq!(call["error"], true);
    assert!(call["text"].as_str().unwrap().contains("Helpers cannot"));
    assert!(s._mock.agent_requests().is_empty());
    assert_eq!(s.snapshot(&chat.id).await.children.len(), 1);
}

#[tokio::test]
async fn bridged_helpers_on_explicit_claude_cannot_bridge_or_use_native_agent() {
    let s = setup().await;
    let chat = s.chat(PermissionMode::Default).await;
    s.send(
        &chat.id,
        &helper_start(Some("claude-code/opus"), true, "Check files"),
    )
    .await;
    s.settle(&chat.id).await;
    let child = bridged_children(&s, &chat.id, 1).await.pop().unwrap();
    assert_eq!(child.model.provider, "claude-code");
    let runs = s.runs();
    assert_eq!(runs.len(), 2);
    let args = s.logged("args").pop().unwrap();
    let args = args.as_array().unwrap();
    let deny = args.iter().position(|a| a == "--disallowedTools").unwrap();
    let denied = args[deny + 1].as_str().unwrap();
    assert!(denied.contains("Agent") && denied.contains("Task"));
    assert!(
        !s.logged("tools")[1]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["name"] == "mcp__tessera__agents")
    );
}

// Wave 1: actions pre-approved by Claude still pass the host hook. These
// scenarios do not use can_use_tool unless the action explicitly says so.
fn builtin_actions(actions: Value) -> String {
    format!("builtin actions {actions}")
}

async fn decide(s: &Setup, chat: &str, req: ApprovalRequest, decision: ApprovalDecision) {
    s.call::<Value>(Request::RespondApproval {
        session_id: chat.into(),
        request_id: req.id,
        decision,
    })
    .await;
}

fn no_extra_approvals(rx: &mut tokio::sync::broadcast::Receiver<Event>) {
    while let Ok(event) = rx.try_recv() {
        assert!(
            !matches!(event, Event::ApprovalRequested { .. }),
            "extra approval: {event:?}"
        );
    }
}

#[tokio::test]
async fn auto_approved_builtins_cannot_skip_hard_refusals_in_any_mode() {
    let s = setup_with("[security]\nsandbox = 'off'\n").await;
    let mut rx = s.engine.subscribe();
    let protected = s.home.path().join("must-not-exist.txt");
    #[cfg(unix)]
    std::os::unix::fs::symlink(s.home.path(), s.work.join("protected-alias")).unwrap();
    for mode in [
        PermissionMode::Default,
        PermissionMode::Ask,
        PermissionMode::Auto,
        PermissionMode::ReadOnly,
    ] {
        let chat = s.chat(mode).await;
        let actions = json!([
            {"tool":"Bash", "input":{"command":"rm -rf /"}},
            {"tool":"Write", "input":{"file_path":protected, "content":"x"}},
            {"tool":"Read", "input":{"file_path":protected}},
            {"tool":"REPL", "input":{"code":"print(1)"}},
            {"tool":"FutureUnknown", "input":{}},
        ]);
        #[cfg(unix)]
        let actions = {
            let mut actions = actions;
            actions.as_array_mut().unwrap().push(json!({"tool":"Write", "input":{"file_path":s.work.join("protected-alias/x.txt"), "content":"x"}}));
            actions
        };
        s.send(&chat.id, &builtin_actions(actions)).await;
        s.settle(&chat.id).await;
        assert!(
            tool_results(&s.snapshot(&chat.id).await.entries)
                .iter()
                .all(|r| r.1)
        );
    }
    assert!(s.logged("executed").is_empty());
    assert!(!protected.exists());
    no_extra_approvals(&mut rx);
}

#[tokio::test]
async fn native_credential_tools_are_denied_and_bridged_reads_are_redacted() {
    let s = setup_with("[security]\nsandbox = 'off'\n").await;
    let file = s.work.join(".env.production");
    std::fs::write(&file, "SERVICE_TOKEN=fixture-value-123456\nPORT=8080\n").unwrap();
    s.engine
        .auth()
        .set_secret("SERVICE_TOKEN", "fixture-value-123456")
        .unwrap();
    for tool in ["Read", "Grep", "Glob", "Bash", "NotebookEdit"] {
        let chat = s.chat(PermissionMode::Auto).await;
        s.send(
            &chat.id,
            &builtin_actions(json!([{"tool":tool, "input": {
                "path":file, "file_path":file, "notebook_path":file, "pattern":".env.*",
                "command":format!("cat '{}'", file.display())
            }}])),
        )
        .await;
        s.settle(&chat.id).await;
        let hooks = s.logged("hook");
        let hook = hooks
            .iter()
            .rev()
            .find(|h| h["tool"] == tool && h["callback"] == "tessera_policy")
            .unwrap();
        assert_eq!(
            hook["reply"]["hookSpecificOutput"]["permissionDecision"],
            "deny"
        );
        assert!(
            hook["reply"]["hookSpecificOutput"]["permissionDecisionReason"]
                .as_str()
                .unwrap()
                .contains("mcp__tessera__read")
        );
    }
    assert!(s.logged("executed").is_empty());
    let chat = s.chat(PermissionMode::Auto).await;
    s.send(&chat.id, &format!("tessera read {}", json!({"path":file})))
        .await;
    s.settle(&chat.id).await;
    let result = s.logged("called").pop().unwrap();
    assert_eq!(result["error"], false);
    let text = result["text"].as_str().unwrap();
    assert!(
        text.contains("[REDACTED]") && text.contains("PORT=8080"),
        "{text}"
    );
    assert!(!text.contains("fixture-value-123456"));
}
#[tokio::test]
async fn hook_and_permission_prompt_share_exactly_one_approval() {
    let s = setup().await;
    let chat = s.chat(PermissionMode::Ask).await;
    let mut rx = s.engine.subscribe();
    s.send(
        &chat.id,
        &builtin_actions(
            json!([{"tool":"Bash", "input":{"command":"echo contract"}, "prompt":true}]),
        ),
    )
    .await;
    let req = approval(&mut rx).await;
    assert_eq!(req.call_id, "toolu_contract_0");
    assert_eq!(req.summary, "echo contract");
    decide(&s, &chat.id, req, ApprovalDecision::Allow).await;
    s.settle(&chat.id).await;
    assert_eq!(s.logged("executed").len(), 1);
    no_extra_approvals(&mut rx);
    assert!(
        s.logged("hook")
            .iter()
            .filter(|h| h["callback"] == "tessera_policy")
            .all(|h| h["reply"] == json!({}))
    );
}

#[tokio::test]
async fn denying_a_hook_refuses_that_action_but_does_not_interrupt_other_work() {
    let s = setup().await;
    let chat = s.chat(PermissionMode::Ask).await;
    let mut rx = s.engine.subscribe();
    std::fs::write(s.work.join("own-file.txt"), "ordinary data\n").unwrap();
    s.send(
        &chat.id,
        &builtin_actions(json!([
            {"tool":"Write", "input":{"file_path":s.work.join("declined.txt"), "content":"x"}},
            {"tool":"Bash", "input":{"command":"cat own-file.txt | head"}},
            {"tool":"mcp__tessera__settings", "input":{"action":"read"}},
        ])),
    )
    .await;
    let req = approval(&mut rx).await;
    decide(&s, &chat.id, req, ApprovalDecision::Deny).await;
    // The read is independent work but still needs its own command approval.
    let read = approval(&mut rx).await;
    decide(&s, &chat.id, read, ApprovalDecision::Allow).await;
    s.settle(&chat.id).await;
    assert!(!s.work.join("declined.txt").exists());
    assert_eq!(s.logged("executed").len(), 2);
    assert!(
        !s.calls()
            .iter()
            .any(|c| c["value"]["request"]["subtype"] == "interrupt")
    );
    let denial = s
        .logged("hook")
        .into_iter()
        .find(|h| h["tool"] == "Write" && h["callback"] == "tessera_policy")
        .unwrap();
    assert!(
        denial["reply"]["hookSpecificOutput"]["permissionDecisionReason"]
            .as_str()
            .unwrap()
            .contains("declined this Write action")
    );
    no_extra_approvals(&mut rx);
}

#[tokio::test]
async fn session_approval_is_scoped_to_command_path_and_chat_across_resume() {
    let s = setup().await;
    let chat = s.chat(PermissionMode::Ask).await;
    let mut rx = s.engine.subscribe();
    let command = builtin_actions(json!([{"tool":"Bash", "input":{"command":"echo one"}}]));
    s.send(&chat.id, &command).await;
    let req = approval(&mut rx).await;
    decide(&s, &chat.id, req, ApprovalDecision::AllowSession).await;
    s.settle(&chat.id).await;
    s.send(&chat.id, &command).await;
    s.settle(&chat.id).await;
    no_extra_approvals(&mut rx);
    assert!(s.runs().last().unwrap().contains("--resume"));
    s.send(
        &chat.id,
        &builtin_actions(json!([{"tool":"Bash", "input":{"command":"echo two"}}])),
    )
    .await;
    let req = approval(&mut rx).await;
    decide(&s, &chat.id, req, ApprovalDecision::Allow).await;
    s.settle(&chat.id).await;
    let write = |name: &str| {
        builtin_actions(
            json!([{"tool":"Write", "input":{"file_path":s.work.join(name), "content":"x"}}]),
        )
    };
    s.send(&chat.id, &write("one.txt")).await;
    let req = approval(&mut rx).await;
    decide(&s, &chat.id, req, ApprovalDecision::AllowSession).await;
    s.settle(&chat.id).await;
    s.send(&chat.id, &write("one.txt")).await;
    s.settle(&chat.id).await;
    no_extra_approvals(&mut rx);
    s.send(&chat.id, &write("two.txt")).await;
    let req = approval(&mut rx).await;
    decide(&s, &chat.id, req, ApprovalDecision::Allow).await;
    s.settle(&chat.id).await;
    let other = s.chat(PermissionMode::Ask).await;
    s.send(&other.id, &command).await;
    let req = approval(&mut rx).await;
    decide(&s, &other.id, req, ApprovalDecision::Allow).await;
    s.settle(&other.id).await;
}

#[tokio::test]
async fn mid_turn_downgrade_precedes_the_next_auto_approved_mutation() {
    let s = setup().await;
    let chat = s.chat(PermissionMode::Auto).await;
    let mut rx = s.engine.subscribe();
    s.send(&chat.id, &builtin_actions(json!([{"tool":"Write", "wait_mode":true, "input":{"file_path":s.work.join("after.txt"), "content":"x"}}]))).await;
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if !s.logged("await-mode").is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let mut u = update(&chat.id);
    if let Request::UpdateSession { permission, .. } = &mut u {
        *permission = Some(PermissionMode::ReadOnly);
    }
    s.call::<SessionInfo>(u).await;
    s.settle(&chat.id).await;
    assert!(!s.work.join("after.txt").exists());
    assert_eq!(s.logged("permission-mode"), vec![json!("plan")]);
    assert!(s.logged("executed").is_empty());
    no_extra_approvals(&mut rx);
}

#[tokio::test]
async fn pending_approval_cannot_release_a_mutation_after_downgrade() {
    let s = setup().await;
    let chat = s.chat(PermissionMode::Ask).await;
    let mut rx = s.engine.subscribe();
    s.send(&chat.id, &builtin_actions(json!([{"tool":"Write", "input":{"file_path":s.work.join("pending.txt"), "content":"x"}}]))).await;
    let _req = approval(&mut rx).await;
    let mut u = update(&chat.id);
    if let Request::UpdateSession { permission, .. } = &mut u {
        *permission = Some(PermissionMode::ReadOnly);
    }
    s.call::<SessionInfo>(u).await;
    s.settle(&chat.id).await;
    assert!(!s.work.join("pending.txt").exists());
    assert!(s.logged("executed").is_empty());
    assert!(s.snapshot(&chat.id).await.approvals.is_empty());
    no_extra_approvals(&mut rx);
}

#[tokio::test]
async fn definition_specific_claude_helpers_have_positive_builtin_ceilings() {
    let s = setup().await;
    let definitions = s.work.join(".tessera/agents");
    std::fs::create_dir_all(&definitions).unwrap();
    std::fs::write(definitions.join("writer.md"), "---\ndescription: Writes a report file.\ntools: [write]\n---\nWrite only the requested report.\n").unwrap();
    let chat = s.chat(PermissionMode::Auto).await;
    let prompt = builtin_actions(json!([
        {"tool":"Edit", "input":{"file_path":s.work.join("omitted.txt"), "old_string":"a", "new_string":"b"}},
        {"tool":"Bash", "input":{"command":"echo omitted"}},
        {"tool":"Write", "input":{"file_path":s.work.join("report.txt"), "content":"x"}},
    ]));
    s.send(&chat.id, &format!("tessera agents {}", json!({"action":"start", "wait":true, "tasks":[{"name":"Writer", "model":"claude-code/opus", "agent":"writer", "prompt":prompt}]}))).await;
    s.settle(&chat.id).await;
    assert_eq!(s.logged("executed").len(), 1);
    assert_eq!(s.logged("executed")[0]["tool"], "Write");
    let tools = s.last_arg("--tools").unwrap();
    assert!(tools.split(',').any(|t| t == "Write"));
    assert!(
        !tools
            .split(',')
            .any(|t| matches!(t, "Edit" | "Bash" | "Read" | "Agent"))
    );
}

#[tokio::test]
async fn report_only_claude_helper_cannot_use_any_builtin() {
    let s = setup().await;
    let chat = s.chat(PermissionMode::Auto).await;
    s.send(&chat.id, &format!("tessera agents {}", json!({"action":"start", "wait":true, "tasks":[{"name":"Reporter", "model":"claude-code/opus", "prompt":"Give a short report", "output":{"type":"object"}}]}))).await;
    s.settle(&chat.id).await;
    assert_eq!(
        s.logged("report-action"),
        vec![json!({"allowed":false, "tools":""})]
    );
    assert!(!s.work.join("report-extra.txt").exists());
}

#[tokio::test]
async fn cli_refusal_after_host_approval_stays_a_failed_tool_result() {
    let s = setup().await;
    let chat = s.chat(PermissionMode::Ask).await;
    let mut rx = s.engine.subscribe();
    s.send(
        &chat.id,
        &builtin_actions(
            json!([{"tool":"Bash", "input":{"command":"echo refusal"}, "cli_refuses":true}]),
        ),
    )
    .await;
    let req = approval(&mut rx).await;
    decide(&s, &chat.id, req, ApprovalDecision::Allow).await;
    s.settle(&chat.id).await;
    let results = tool_results(&s.snapshot(&chat.id).await.entries);
    assert_eq!(results, vec![("Bash".into(), true, "not performed".into())]);
    assert!(s.logged("executed").is_empty());
    no_extra_approvals(&mut rx);
}

#[tokio::test]
async fn lifecycle_stop_is_registered_with_verification_off() {
    let s = setup_with("[agent]\nverify_check = 'off'\n").await;
    let chat = s.chat(PermissionMode::Default).await;
    s.send(&chat.id, "hello").await;
    s.settle(&chat.id).await;
    assert_eq!(
        s.logged("hooks"),
        vec![json!(["PostToolUse", "PreToolUse", "Stop"])]
    );
    assert!(s.logged("nudged").is_empty());
}

#[tokio::test]
async fn refused_hook_initialization_never_sends_user_work() {
    let s = setup().await;
    std::fs::write(s.work.join("reject-init"), "").unwrap();
    let chat = s.chat(PermissionMode::Auto).await;
    s.send(
        &chat.id,
        &builtin_actions(json!([{"tool":"Bash", "input":{"command":"echo unsafe"}}])),
    )
    .await;
    s.settle(&chat.id).await;
    assert!(s.prompts().is_empty());
    assert!(s.logged("executed").is_empty());
    let snap = s.snapshot(&chat.id).await;
    assert_eq!(snap.info.status, SessionStatus::Error);
    assert!(format!("{:?}", snap.entries).contains("permission hooks"));
}

#[tokio::test]
async fn bot_ceilings_refuse_auto_approved_omitted_builtins() {
    let s = setup().await;
    let bot = s
        .save_bot("Researcher", "claude-code/opus", ToolPreset::Research)
        .await;
    let chat = s.session_on("claude-code/opus", Some(&bot.id), false).await;
    s.send(
        &chat.id,
        &builtin_actions(json!([
            {"tool":"Bash", "input":{"command":"echo omitted"}},
            {"tool":"Write", "input":{"file_path":s.work.join("omitted.txt"), "content":"x"}},
            {"tool":"PowerShell", "input":{"command":"echo omitted"}},
            {"tool":"WebSearch", "input":{"query":"Tessera"}},
        ])),
    )
    .await;
    s.settle(&chat.id).await;
    assert_eq!(s.logged("executed").len(), 1);
    assert_eq!(s.logged("executed")[0]["tool"], "WebSearch");
    assert!(!s.work.join("omitted.txt").exists());
}

#[tokio::test]
async fn channel_session_grants_never_cover_another_bot() {
    let s = setup().await;
    let mut bots = Vec::new();
    for name in ["Ada", "Bob"] {
        let mut bot = s.save_bot(name, "claude-code/opus", ToolPreset::Full).await;
        bot.permission = PermissionMode::Ask;
        let updated: Bot = s.call(Request::SaveBot { bot }).await;
        bots.push(updated.id);
    }
    let room: SessionInfo = s
        .call(Request::CreateChannel {
            title: "Contract".into(),
            members: bots,
            cwd: Some(s.work.display().to_string()),
        })
        .await;
    let mut rx = s.engine.subscribe();
    let command = builtin_actions(json!([{"tool":"Bash", "input":{"command":"echo scoped"}}]));
    s.send(&room.id, &command).await;
    let first = approval(&mut rx).await;
    decide(&s, &room.id, first, ApprovalDecision::AllowSession).await;
    let second = approval(&mut rx).await;
    decide(&s, &room.id, second, ApprovalDecision::AllowSession).await;
    s.settle(&room.id).await;
    assert_eq!(s.logged("executed").len(), 2);
    s.send(&room.id, &command).await;
    s.settle(&room.id).await;
    assert_eq!(s.logged("executed").len(), 4);
    no_extra_approvals(&mut rx);
}

#[tokio::test]
async fn live_parent_downgrade_caps_an_inflight_claude_helper() {
    let s = setup().await;
    let chat = s.chat(PermissionMode::Auto).await;
    let prompt = builtin_actions(json!([{"tool":"Write", "wait_mode":true,
        "input":{"file_path":s.work.join("helper-after.txt"), "content":"x"}}]));
    s.send(
        &chat.id,
        &format!(
            "tessera agents {}",
            json!({"action":"start", "wait":true,
        "tasks":[{"name":"Writer", "model":"claude-code/opus", "prompt":prompt}]})
        ),
    )
    .await;
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if !s.logged("await-mode").is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let mut u = update(&chat.id);
    if let Request::UpdateSession { permission, .. } = &mut u {
        *permission = Some(PermissionMode::ReadOnly);
    }
    s.call::<SessionInfo>(u).await;
    s.settle(&chat.id).await;
    assert!(!s.work.join("helper-after.txt").exists());
    assert!(s.logged("executed").is_empty());
    assert!(s.logged("permission-mode").iter().all(|m| m == "plan"));
    assert!(s.logged("permission-mode").len() >= 2);
}

#[tokio::test]
async fn file_snapshots_are_taken_only_after_the_action_is_approved() {
    let s = setup().await;
    let chat = s.chat(PermissionMode::Ask).await;
    let file = s.work.join("before.txt");
    std::fs::write(&file, "original").unwrap();
    // Existing-file writes now need a recorded read before any approval.
    s.send(&chat.id, &format!("tessera read {}", json!({"path":file})))
        .await;
    s.settle(&chat.id).await;
    let count = || -> i64 {
        let paths = Paths::at(s.home.path());
        let db = rusqlite::Connection::open_with_flags(
            paths.db_file(),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        db.query_row(
            "SELECT COUNT(*) FROM file_snapshots WHERE session_id = ?1",
            [&chat.id],
            |r| r.get(0),
        )
        .unwrap()
    };
    let mut rx = s.engine.subscribe();
    let action =
        builtin_actions(json!([{"tool":"Write", "input":{"file_path":file, "content":"changed"}}]));
    s.send(&chat.id, &action).await;
    let req = approval(&mut rx).await;
    assert_eq!(count(), 0);
    decide(&s, &chat.id, req, ApprovalDecision::Deny).await;
    s.settle(&chat.id).await;
    assert_eq!(count(), 0);
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "original");
    s.send(&chat.id, &action).await;
    let req = approval(&mut rx).await;
    assert_eq!(count(), 0);
    decide(&s, &chat.id, req, ApprovalDecision::Allow).await;
    s.settle(&chat.id).await;
    assert_eq!(count(), 1);
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "changed");
    no_extra_approvals(&mut rx);
}

#[tokio::test]
async fn native_child_hook_metadata_only_restricts_and_denials_are_sticky() {
    let s = setup().await;
    let chat = s.chat(PermissionMode::Auto).await;
    let mut rx = s.engine.subscribe();
    s.send(&chat.id, &builtin_actions(json!([
        {"tool":"Write", "input":{"file_path":s.work.join("explore.txt"), "content":"x"},
         "metadata":{"agent_id":"child", "agent_type":"Explore"}, "prompt_even_if_denied":true},
        {"tool":"Write", "input":{"file_path":s.work.join("plan.txt"), "content":"x"},
         "metadata":{"permission_mode":"plan"}, "prompt_even_if_denied":true},
        {"tool":"Agent", "input":{"prompt":"grandchild"},
         "metadata":{"agent_id":"child", "agent_type":"general-purpose"}, "prompt_even_if_denied":true},
        {"tool":"mcp__tessera__agents", "input":{"action":"start"},
         "metadata":{"agent_id":"child"}, "prompt_even_if_denied":true},
        {"tool":"WebSearch", "input":{"query":"Tessera"},
         "metadata":{"agent_id":"child", "agent_type":"Explore"}}
    ]))).await;
    s.settle(&chat.id).await;
    assert_eq!(s.logged("executed").len(), 1);
    assert_eq!(s.logged("executed")[0]["tool"], "WebSearch");
    assert!(!s.work.join("explore.txt").exists());
    assert!(!s.work.join("plan.txt").exists());
    no_extra_approvals(&mut rx);
}

#[tokio::test]
async fn documented_local_builtins_remain_available_and_pass_policy_in_yolo() {
    let s = setup().await;
    let chat = s.chat(PermissionMode::Auto).await;
    let file = s.work.join("ordinary.txt");
    std::fs::write(&file, "hello").unwrap();
    // Exhaust every mapping: additions cannot be declared available but then
    // forgotten in the assessment adapter. No real tool actions except fake Write.
    let actions: Vec<_> = tessera_core::claude_code::BUILTINS
        .iter()
        .map(|(tool, _)| {
            let input = match *tool {
                "Bash" | "PowerShell" | "Monitor" => json!({"command":"echo local"}),
                "Read" | "Write" | "Edit" | "MultiEdit" | "NotebookEdit" => {
                    json!({"file_path":file, "content":"hello"})
                }
                "LSP" => json!({"filePath":file, "operation":"hover"}),
                "Grep" => json!({"path":file, "pattern":"hello"}),
                "Glob" => json!({"path":s.work, "pattern":"hello"}),
                "Workflow" => json!({"scriptPath":file, "workflowId":"w"}),
                "Artifact" => json!({"action":"list"}),
                "SendUserFile" => json!({"local_path":file}),
                "ShareOnboardingGuide" => json!({"title":"Welcome"}),
                "Projects" => json!({"method":"project_read"}),
                _ => json!({}),
            };
            json!({"tool":tool, "input":input, "prompt":true})
        })
        .collect();
    s.send(&chat.id, &builtin_actions(json!(actions))).await;
    s.settle(&chat.id).await;
    let executed = s.logged("executed");
    assert_eq!(
        executed.len(),
        tessera_core::claude_code::BUILTINS.len(),
        "{executed:?}\n{:?}",
        tool_results(&s.snapshot(&chat.id).await.entries)
    );
    let tools = s.last_arg("--tools").unwrap();
    for (tool, _) in tessera_core::claude_code::BUILTINS {
        assert!(tools.split(',').any(|t| t == *tool), "missing {tool}");
    }
    assert!(!tools.contains("RemoteTrigger"));
    assert!(!s.runs().last().unwrap().contains("--strict-mcp-config"));
}

#[tokio::test]
async fn restored_mutating_tools_show_one_ask_card_per_action() {
    let s = setup().await;
    let chat = s.chat(PermissionMode::Ask).await;
    let file = s.work.join("workflow.py");
    std::fs::write(&file, "print('local')").unwrap();
    let actions = json!([
        {"tool":"Monitor", "input":{"command":"echo monitor"}, "prompt":true},
        {"tool":"PowerShell", "input":{"command":"Write-Output hello"}, "prompt":true},
        {"tool":"CronCreate", "input":{"cron":"* * * * *", "prompt":"check"}, "prompt":true},
        {"tool":"CronDelete", "input":{"ids":["one"]}, "prompt":true},
        {"tool":"ScheduleWakeup", "input":{"seconds":60}, "prompt":true},
        {"tool":"EnterWorktree", "input":{"name":"test"}, "prompt":true},
        {"tool":"ExitWorktree", "input":{"action":"remove", "discard_changes":true}, "prompt":true},
        {"tool":"Workflow", "input":{"scriptPath":file, "workflowId":"w"}, "prompt":true},
        {"tool":"SendMessage", "input":{"recipient":"helper", "message":"finish"}, "prompt":true}
    ]);
    let count = actions.as_array().unwrap().len();
    let mut rx = s.engine.subscribe();
    s.send(&chat.id, &builtin_actions(actions)).await;
    for i in 0..count {
        let req = approval(&mut rx).await;
        assert_eq!(req.call_id, format!("toolu_contract_{i}"));
        decide(&s, &chat.id, req, ApprovalDecision::Allow).await;
    }
    s.settle(&chat.id).await;
    assert_eq!(s.logged("executed").len(), count);
    no_extra_approvals(&mut rx);
}

#[tokio::test]
async fn restored_tools_keep_hard_refusals_read_only_and_actor_ceilings() {
    let s = setup().await;
    for mode in [
        PermissionMode::Default,
        PermissionMode::Ask,
        PermissionMode::Auto,
        PermissionMode::ReadOnly,
    ] {
        let chat = s.chat(mode).await;
        let mut rx = s.engine.subscribe();
        s.send(&chat.id, &builtin_actions(json!([
            {"tool":"Monitor", "input":{"command":"rm -rf /"}},
            {"tool":"PowerShell", "input":{"command":"Remove-Item -Recurse -Force C:\\"}},
            {"tool":"Workflow", "input":{"scriptPath":s.home.path().join("secret.py"), "workflowId":"w"}},
            {"tool":"LSP", "input":{"filePath":s.home.path().join("secret.rs"), "operation":"hover"}},
            {"tool":"EnterWorktree", "input":{"name":"../../escape"}},
            {"tool":"RemoteTrigger", "input":{}},
            {"tool":"Agent", "input":{"isolation":"remote", "prompt":"work"}},
            {"tool":"FutureTool", "input":{}}
        ]))).await;
        s.settle(&chat.id).await;
        assert!(
            tool_results(&s.snapshot(&chat.id).await.entries)
                .iter()
                .all(|r| r.1)
        );
        no_extra_approvals(&mut rx);
    }
    let chat = s.chat(PermissionMode::ReadOnly).await;
    s.send(
        &chat.id,
        &builtin_actions(json!([
            {"tool":"CronCreate", "input":{}}, {"tool":"CronDelete", "input":{}},
            {"tool":"ScheduleWakeup", "input":{}}, {"tool":"EnterWorktree", "input":{}},
            {"tool":"ExitWorktree", "input":{}}, {"tool":"SendMessage", "input":{}},
            {"tool":"Workflow", "input":{"scriptPath":s.work.join("w.py")}},
            {"tool":"CronList", "input":{}}, {"tool":"Skill", "input":{"skill":"review"}},
            {"tool":"ListAgents", "input":{}}
        ])),
    )
    .await;
    s.settle(&chat.id).await;
    assert_eq!(
        s.logged("executed")
            .iter()
            .map(|r| r["tool"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["CronList", "Skill", "ListAgents"]
    );
    let bot = s
        .save_bot("Researcher", "claude-code/opus", ToolPreset::Research)
        .await;
    let chat = s.session_on("claude-code/opus", Some(&bot.id), false).await;
    s.send(
        &chat.id,
        &builtin_actions(json!([
            {"tool":"Monitor", "input":{"command":"echo omitted"}},
            {"tool":"CronCreate", "input":{}}, {"tool":"EnterWorktree", "input":{}},
            {"tool":"Workflow", "input":{"scriptPath":s.work.join("w.py")}},
            {"tool":"Skill", "input":{"skill":"review"}}
        ])),
    )
    .await;
    s.settle(&chat.id).await;
    assert!(s.runs().last().unwrap().contains("--strict-mcp-config"));
    assert_eq!(s.logged("executed").len(), 4);
}

#[tokio::test]
async fn external_mcp_default_and_ask_have_one_card_and_per_tool_session_grants() {
    let s = setup().await;
    for mode in [PermissionMode::Default, PermissionMode::Ask] {
        let chat = s.chat(mode).await;
        let mut rx = s.engine.subscribe();
        let action = |tool: &str, value: &str| {
            builtin_actions(json!([{"tool":tool, "input":{"value":value}, "prompt":true,
            "metadata":{"mcp_server":{"name":"personal", "type":"http", "url":"https://example.invalid/mcp"}}}]))
        };
        s.send(&chat.id, &action("mcp__personal__lookup", "first"))
            .await;
        let req = approval(&mut rx).await;
        assert_eq!(req.tool, "mcp__personal__lookup");
        assert!(req.reason.contains("external integration"));
        decide(&s, &chat.id, req, ApprovalDecision::AllowSession).await;
        s.settle(&chat.id).await;
        s.send(&chat.id, &action("mcp__personal__lookup", "second"))
            .await;
        s.settle(&chat.id).await;
        no_extra_approvals(&mut rx);
        s.send(&chat.id, &action("mcp__personal__change", "third"))
            .await;
        let req = approval(&mut rx).await;
        decide(&s, &chat.id, req, ApprovalDecision::Allow).await;
        s.settle(&chat.id).await;
        let other = s.chat(mode).await;
        s.send(&other.id, &action("mcp__personal__lookup", "fourth"))
            .await;
        let req = approval(&mut rx).await;
        decide(&s, &other.id, req, ApprovalDecision::Allow).await;
        s.settle(&other.id).await;
        no_extra_approvals(&mut rx);
    }
    assert_eq!(s.logged("executed").len(), 8);
    assert!(s.runs().iter().all(|r| !r.contains("--strict-mcp-config")));
}

#[tokio::test]
async fn external_mcp_yolo_readonly_downgrade_and_denial_cannot_bypass_policy() {
    let s = setup().await;
    let chat = s.chat(PermissionMode::Auto).await;
    let mut rx = s.engine.subscribe();
    s.send(
        &chat.id,
        &builtin_actions(
            json!([{"tool":"mcp__claude_ai_drive__upload", "input":{}, "prompt":true}]),
        ),
    )
    .await;
    s.settle(&chat.id).await;
    assert_eq!(s.logged("executed").len(), 1);
    no_extra_approvals(&mut rx);
    let readonly = s.chat(PermissionMode::ReadOnly).await;
    s.send(&readonly.id, &builtin_actions(json!([{"tool":"mcp__personal__read_file",
        "input":{"readOnlyHint":true}, "metadata":{"annotations":{"readOnlyHint":true}}, "prompt_even_if_denied":true}]))).await;
    s.settle(&readonly.id).await;
    assert_eq!(s.logged("executed").len(), 1);
    no_extra_approvals(&mut rx);
    let ask = s.chat(PermissionMode::Ask).await;
    s.send(
        &ask.id,
        &builtin_actions(json!([{"tool":"mcp__personal__change", "input":{}},
        {"tool":"mcp__claude_ai_drive__upload", "input":{}}])),
    )
    .await;
    let req = approval(&mut rx).await;
    decide(&s, &ask.id, req, ApprovalDecision::Deny).await;
    // A denial is no longer an interrupt: independent work may ask its own
    // question, but cannot reuse the declined call's answer.
    let next = approval(&mut rx).await;
    decide(&s, &ask.id, next, ApprovalDecision::Deny).await;
    s.settle(&ask.id).await;
    assert_eq!(s.logged("executed").len(), 1);
    no_extra_approvals(&mut rx);
    let ask = s.chat(PermissionMode::Ask).await;
    s.send(
        &ask.id,
        &builtin_actions(json!([{"tool":"mcp__personal__change", "input":{}}])),
    )
    .await;
    let _req = approval(&mut rx).await;
    let mut u = update(&ask.id);
    if let Request::UpdateSession { permission, .. } = &mut u {
        *permission = Some(PermissionMode::ReadOnly);
    }
    s.call::<SessionInfo>(u).await;
    s.settle(&ask.id).await;
    assert_eq!(s.logged("executed").len(), 1);
    no_extra_approvals(&mut rx);
}

#[tokio::test]
async fn limited_bot_and_helper_cannot_use_external_integrations_or_resource_escape() {
    let s = setup().await;
    let bot = s
        .save_bot("Researcher", "claude-code/opus", ToolPreset::Research)
        .await;
    let chat = s.session_on("claude-code/opus", Some(&bot.id), false).await;
    let action = builtin_actions(json!([
        {"tool":"mcp__personal__lookup", "input":{}},
        {"tool":"mcp__claude_ai_drive__read", "input":{}},
        {"tool":"ReadMcpResourceTool", "input":{"server":"personal", "uri":"file:///secret"}},
        {"tool":"ListMcpResourcesTool", "input":{}}
    ]));
    let mut rx = s.engine.subscribe();
    s.send(&chat.id, &action).await;
    s.settle(&chat.id).await;
    assert!(s.logged("executed").is_empty());
    no_extra_approvals(&mut rx);
    let definitions = s.work.join(".tessera/agents");
    std::fs::create_dir_all(&definitions).unwrap();
    std::fs::write(
        definitions.join("reader.md"),
        "---\ndescription: Read only.\ntools: [read]\n---\nRead the report.\n",
    )
    .unwrap();
    let chat = s.chat(PermissionMode::Auto).await;
    s.send(
        &chat.id,
        &format!(
            "tessera agents {}",
            json!({"action":"start", "wait":true,
        "tasks":[{"name":"Reader", "agent":"reader", "model":"claude-code/opus", "prompt":action}]})
        ),
    )
    .await;
    s.settle(&chat.id).await;
    assert!(s.logged("executed").is_empty());
    assert!(s.runs().last().unwrap().contains("--strict-mcp-config"));
    no_extra_approvals(&mut rx);
}

#[tokio::test]
async fn explicitly_allowed_external_mcp_has_a_trusted_readonly_floor() {
    let home = tempfile::tempdir().unwrap();
    let fake = tempfile::tempdir().unwrap();
    let server = fake.path().join("mcp-standin.py");
    std::fs::write(&server, r#"import json, sys
for line in sys.stdin:
    m = json.loads(line)
    if 'id' not in m: continue
    if m['method'] == 'initialize':
        result = {'protocolVersion':'2025-06-18','capabilities':{'tools':{}},'serverInfo':{'name':'personal','version':'1'}}
    elif m['method'] == 'tools/list':
        open(__file__ + '.ready', 'w').write('ready')
        result = {'tools':[{'name':'lookup','description':'Read-only lookup','inputSchema':{'type':'object'},'annotations':{'readOnlyHint':True}},
                           {'name':'change','description':'Change state','inputSchema':{'type':'object'}}]}
    else: result = {'content':[{'type':'text','text':'ok'}]}
    print(json.dumps({'jsonrpc':'2.0','id':m['id'],'result':result}), flush=True)
"#).unwrap();
    let s = setup_in(
        home,
        fake,
        &format!(
            "[[mcp]]\nname = 'personal'\ncommand = 'python3'\nargs = ['{}']\n",
            server.display()
        ),
    )
    .await;
    tokio::time::timeout(Duration::from_secs(10), async {
        while !Path::new(&format!("{}.ready", server.display())).exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    })
    .await
    .unwrap();
    let chat = s.chat(PermissionMode::ReadOnly).await;
    let provenance = json!({"mcp_server":{"name":"personal", "type":"stdio", "command":"python3", "args":[server]}});
    let actions = builtin_actions(json!([
        {"tool":"mcp__personal__lookup", "input":{}, "prompt":true, "metadata":provenance},
        {"tool":"mcp__personal__change", "input":{}, "prompt_even_if_denied":true},
        {"tool":"mcp__personal__lookup", "input":{}, "prompt_even_if_denied":true,
            "metadata":{"mcp_server":{"name":"personal", "type":"stdio", "command":"unknown", "args":[server]}}}
    ]));
    let mut rx = s.engine.subscribe();
    s.send(&chat.id, &actions).await;
    s.settle(&chat.id).await;
    assert_eq!(s.logged("executed").len(), 1);
    no_extra_approvals(&mut rx);
    let mut restricted: Value =
        serde_json::from_str(actions.strip_prefix("builtin actions ").unwrap()).unwrap();
    restricted.as_array_mut().unwrap().push(json!({"tool":"ReadMcpResourceTool", "input":{"server":"elsewhere", "uri":"file:///secret"}}));
    let restricted_actions = builtin_actions(restricted);
    let mut bot = s
        .save_bot("Lookup", "claude-code/opus", ToolPreset::Custom)
        .await;
    bot.allowed_tools = vec!["read".into(), "mcp__personal__lookup".into()];
    bot.permission = PermissionMode::ReadOnly;
    let bot: Bot = s.call(Request::SaveBot { bot }).await;
    let chat = s.session_on("claude-code/opus", Some(&bot.id), false).await;
    s.send(&chat.id, &restricted_actions).await;
    s.settle(&chat.id).await;
    assert_eq!(s.logged("executed").len(), 2);
    assert!(!s.runs().last().unwrap().contains("--strict-mcp-config"));
    no_extra_approvals(&mut rx);
    let definitions = s.work.join(".tessera/agents");
    std::fs::create_dir_all(&definitions).unwrap();
    std::fs::write(
        definitions.join("lookup.md"),
        "---\ndescription: Read lookup.\ntools: [read, mcp__personal__lookup]\n---\nLookup only.\n",
    )
    .unwrap();
    let chat = s.chat(PermissionMode::ReadOnly).await;
    s.send(&chat.id, &format!("tessera agents {}", json!({"action":"start", "wait":true,
        "tasks":[{"name":"Lookup", "agent":"lookup", "model":"claude-code/opus", "prompt":restricted_actions}]}))).await;
    s.settle(&chat.id).await;
    assert_eq!(s.logged("executed").len(), 3);
    no_extra_approvals(&mut rx);
}

#[tokio::test]
async fn monitor_websockets_follow_mode_without_erasing_hard_refusals() {
    let s = setup().await;
    for mode in [
        PermissionMode::Default,
        PermissionMode::Ask,
        PermissionMode::Auto,
        PermissionMode::ReadOnly,
    ] {
        let chat = s.chat(mode).await;
        let mut rx = s.engine.subscribe();
        let before = s.logged("executed").len();
        s.send(&chat.id, &builtin_actions(json!([
            {"tool":"Monitor", "input":{"ws":{"url":"ws://example.invalid/events"}}, "metadata":{"cwd":s.home.path()}},
            {"tool":"Monitor", "input":{"ws":{"url":"wss://user:password@example.invalid/events"}}},
            {"tool":"Monitor", "input":{"ws":{"url":"file:///tmp/events"}}},
            {"tool":"Monitor", "input":{"ws":{"url":"wss://example.invalid/events"}}, "prompt":true}
        ]))).await;
        if matches!(mode, PermissionMode::Default | PermissionMode::Ask) {
            let req = approval(&mut rx).await;
            assert_eq!(req.call_id, "toolu_contract_3");
            decide(&s, &chat.id, req, ApprovalDecision::Allow).await;
        }
        s.settle(&chat.id).await;
        assert_eq!(
            s.logged("executed").len() - before,
            usize::from(mode != PermissionMode::ReadOnly)
        );
        no_extra_approvals(&mut rx);
    }
}

#[tokio::test]
async fn powershell_windows_grants_do_not_erase_native_secret_requirements() {
    let s = setup().await;
    // Native commands remain native without stored values. Once a value is
    // stored no privilege grant can bypass the redacted bridge.
    let chat = s.chat(PermissionMode::Default).await;
    let mut rx = s.engine.subscribe();
    let action = |command: &str| {
        builtin_actions(json!([{"tool":"PowerShell", "input":{"command":command}, "prompt":true}]))
    };
    s.send(&chat.id, &action("Start-Process harmless -Verb RunAs"))
        .await;
    let req = approval(&mut rx).await;
    assert!(req.reason.contains("elevated privileges"));
    decide(&s, &chat.id, req, ApprovalDecision::AllowSession).await;
    s.settle(&chat.id).await;
    s.engine
        .auth()
        .set_secret("LAB_TOKEN", "test-only-token-123456")
        .unwrap();
    let with_secret = action("Start-Process harmless -Verb RunAs; Write-Output $LAB_TOKEN");
    s.send(&chat.id, &with_secret).await;
    s.settle(&chat.id).await;
    assert_eq!(s.logged("executed").len(), 1);
    let hooks = s.logged("hook");
    let denial = hooks
        .iter()
        .rev()
        .find(|h| h["tool"] == "PowerShell" && h["callback"] == "tessera_policy")
        .expect("PowerShell policy hook");
    assert_eq!(
        denial["reply"]["hookSpecificOutput"]["permissionDecision"],
        "deny"
    );
    assert!(
        denial["reply"]["hookSpecificOutput"]["permissionDecisionReason"]
            .as_str()
            .unwrap()
            .contains("bridged tools")
    );
    no_extra_approvals(&mut rx);
}

#[tokio::test]
async fn wave2_secure_entry_refreshes_inventory_and_keeps_safe_warm_history() {
    let s = setup_with("[agent]\nreview_interval = 0\nclaude_keep_open_minutes = 10\n").await;
    let chat = s.chat(PermissionMode::Auto).await;
    s.send(&chat.id, "tessera save_secret {\"action\":\"request\",\"name\":\"LAB_LOGIN\",\"title\":\"Lab login\",\"kind\":\"login\"}").await;
    s.settle(&chat.id).await;
    let schema = &s.logged("schemas")[0]["save_secret"];
    assert!(
        schema["properties"].get("value").is_none()
            && schema["properties"].get("password").is_none()
    );
    let snap = s.snapshot(&chat.id).await;
    let id = snap
        .entries
        .iter()
        .find_map(|e| match &e.body {
            EntryBody::ToolResult {
                call_id,
                tool,
                details: Some(d),
                is_error: false,
                ..
            } if tool == "save_secret" && d["secret_entry"]["name"] == "LAB_LOGIN" => {
                Some(call_id.clone())
            }
            _ => None,
        })
        .expect("secure card metadata");
    let sentinel = "vault-only-test-password-897";
    s.call::<Value>(Request::SaveChatSecret {
        session_id: chat.id.clone(),
        call_id: id.clone(),
        fields: SecretEnv(BTreeMap::from([
            ("username".into(), "lab-user".into()),
            ("password".into(), sentinel.into()),
        ])),
        chat_only: true,
    })
    .await;
    let snap = s.snapshot(&chat.id).await;
    let text = serde_json::to_string(&snap).unwrap();
    assert!(
        !text.contains(sentinel) && !text.contains("lab-user"),
        "values leaked to transcript"
    );
    assert!(text.contains("Saved securely in Tessera."));
    assert!(!text.contains("Claude Code's own history") && !text.contains("sanitized history"));
    assert!(text.contains("\"saved\":true"));
    s.send(&chat.id, "echo context").await;
    s.settle(&chat.id).await;
    let input = s.prompts().join("\n");
    assert!(input.contains("LAB_LOGIN_PASSWORD"), "{input}");
    assert!(!input.contains(sentinel));
    s.send(&chat.id, "inspect refreshed bridge").await;
    s.settle(&chat.id).await;
    assert!(
        s.logged("tools")
            .last()
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t == "bash")
    );
    assert_eq!(
        s.runs().len(),
        2,
        "saving the first value refreshes native tools once"
    );
    s.send(&chat.id, "another safe turn").await;
    s.settle(&chat.id).await;
    assert_eq!(
        s.runs().len(),
        2,
        "warm process was only preserved for one turn"
    );
    let mut u = update(&chat.id);
    if let Request::UpdateSession { thinking, .. } = &mut u {
        *thinking = Some(ThinkingLevel::High);
    }
    s.call::<Value>(u).await;
    s.send(&chat.id, "echo context").await;
    s.settle(&chat.id).await;
    assert!(
        s.runs().last().unwrap().contains("--resume sess-"),
        "safe resume link was lost"
    );
    let duplicate = s
        .engine
        .handle(
            Request::SaveChatSecret {
                session_id: chat.id.clone(),
                call_id: id,
                fields: SecretEnv(BTreeMap::from([(
                    "password".into(),
                    "another-test-password".into(),
                )])),
                chat_only: true,
            },
            None,
        )
        .await;
    assert!(duplicate.is_err());
}

#[tokio::test]
async fn wave2_search_settings_refresh_between_turns() {
    let s = setup_with("[agent]\nreview_interval = 0\nclaude_keep_open_minutes = 10\n").await;
    let chat = s.chat(PermissionMode::Auto).await;
    s.send(&chat.id, "echo context").await;
    s.settle(&chat.id).await;
    assert!(
        !s.logged("tools")[0]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t == "web_search")
    );
    let args = s.turn_args()[0].clone();
    let args = args.as_array().unwrap();
    let at = args.iter().position(|a| a == "--tools").unwrap();
    assert!(args[at + 1].as_str().unwrap().contains("WebSearch"));
    let mut rx = s.engine.subscribe();
    s.send(
        &chat.id,
        "tessera settings {\"action\":\"set\",\"key\":\"web.search\",\"value\":\"duckduckgo\"}",
    )
    .await;
    let req = approval(&mut rx).await;
    decide(&s, &chat.id, req, ApprovalDecision::Allow).await;
    s.settle(&chat.id).await;
    s.send(&chat.id, "echo context").await;
    s.settle(&chat.id).await;
    assert!(
        s.logged("tools")
            .last()
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t == "web_search")
    );
    let args = s.turn_args().pop().unwrap();
    let args = args.as_array().unwrap();
    let at = args.iter().position(|a| a == "--tools").unwrap();
    assert!(!args[at + 1].as_str().unwrap().contains("WebSearch"));
    assert!(
        s.logged("context")
            .last()
            .unwrap()
            .as_str()
            .unwrap()
            .contains("user's chosen search provider")
    );
}

#[tokio::test]
async fn wave2_skills_and_project_guidance_use_native_index_and_reader() {
    let s = setup().await;
    let skills = s.work.join(".tessera/skills/lab");
    std::fs::create_dir_all(&skills).unwrap();
    std::fs::write(skills.join("SKILL.md"), "---\nname: lab\ndescription: Test lab procedures\n---\n# Procedure\nUse the lab fixture.\n").unwrap();
    std::fs::write(
        s.work.join("AGENTS.md"),
        "# Guidance\nProject fixture guidance.\n",
    )
    .unwrap();
    let user_skill = s.home.path().join("skills/user-fixture");
    std::fs::create_dir_all(&user_skill).unwrap();
    std::fs::write(user_skill.join("SKILL.md"), "---\nname: user-fixture\ndescription: User fixture procedures\n---\nFollow the user's fixture.\n").unwrap();
    let chat = s.chat(PermissionMode::Auto).await;
    s.send(&chat.id, "echo context").await;
    s.settle(&chat.id).await;
    let context = s.logged("context").pop().unwrap().to_string();
    assert!(context.contains("Project fixture guidance"), "{context}");
    assert!(context.contains("- lab: Test lab procedures"), "{context}");
    assert!(
        context.contains("- user-fixture: User fixture procedures"),
        "{context}"
    );
    assert!(context.contains(
        "When one is relevant to the request, load it with the `skill` tool before starting"
    ));
    for action in ["list", "read"] {
        s.send(
            &chat.id,
            &format!("tessera skill {{\"action\":\"{action}\",\"name\":\"lab\"}}"),
        )
        .await;
        s.settle(&chat.id).await;
        assert!(
            !s.logged("called").last().unwrap()["error"]
                .as_bool()
                .unwrap()
        );
    }
    s.send(
        &chat.id,
        "tessera skill {\"action\":\"read\",\"name\":\"user-fixture\"}",
    )
    .await;
    s.settle(&chat.id).await;
    assert!(
        tool_results(&s.snapshot(&chat.id).await.entries)
            .iter()
            .any(|(_, _, t)| t.contains("Follow the user's fixture"))
    );
    assert!(
        tool_results(&s.snapshot(&chat.id).await.entries)
            .iter()
            .any(|(_, _, t)| t.contains("Use the lab fixture"))
    );
}

#[tokio::test]
async fn wave2_background_commands_both_emit_job_cards() {
    let s = setup().await;
    let chat = s.chat(PermissionMode::Auto).await;
    s.send(&chat.id, "claude background command").await;
    s.settle(&chat.id).await;
    let snap = s.snapshot(&chat.id).await;
    assert!(tool_calls(&snap.entries).contains(&"Bash".into()));
    assert!(
        tool_results(&snap.entries)
            .iter()
            .any(|(n, _, text)| n == "Bash" && text.contains("background"))
    );
    s.call::<Value>(Request::SetSecret {
        name: "LAB_TOKEN".into(),
        value: "test-token-only-897".into(),
    })
    .await;
    s.send(
        &chat.id,
        "tessera bash {\"command\":\"printf '%s' \\\"$LAB_TOKEN\\\"\",\"background\":true}",
    )
    .await;
    s.settle(&chat.id).await;
    let snap = s.snapshot(&chat.id).await;
    assert!(
        tool_results(&snap.entries)
            .iter()
            .any(|(n, error, text)| n == "bash" && !error && text.contains("background job"))
    );
    assert!(
        !serde_json::to_string(&snap)
            .unwrap()
            .contains("test-token-only-897")
    );
}

#[tokio::test]
async fn wave2_cross_provider_worktree_helpers_all_controls() {
    let s = setup().await;
    // The chat is outside git; per-task cwd chooses the nested repository.
    let repo = s.work.join("repo");
    std::fs::create_dir(&repo).unwrap();
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(&repo)
            .env("GIT_AUTHOR_NAME", "Test")
            .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
            .env("GIT_COMMITTER_NAME", "Test")
            .env("GIT_COMMITTER_EMAIL", "test@example.invalid")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    git(&["init", "-q"]);
    std::fs::write(repo.join("app.txt"), "v1\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-qm", "init"]);
    let mock = common::Mock::start(vec![]).await;
    s.call::<Value>(Request::SaveProvider {
        spec: ProviderSpec {
            id: "helper-api".into(),
            name: "Fixture API".into(),
            kind: ProviderKind::OpenaiChat,
            base_url: mock.url.clone(),
            api_key: None,
            discover: true,
            models: vec![],
        },
    })
    .await;
    mock.route(
        "<WT_APPLY>",
        // `write` replaces a file only after reading it.
        common::Reply::Tools(vec![
            ("read".into(), json!({"path":"app.txt"})),
            ("write".into(), json!({"path":"app.txt", "content":"v2\n"})),
        ]),
    );
    mock.route("<WT_APPLY>", common::Reply::Text("ready to apply".into()));
    mock.route(
        "<WT_DROP>",
        common::Reply::Tools(vec![(
            "write".into(),
            json!({"path":"drop.txt", "content":"do not apply\n"}),
        )]),
    );
    mock.route("<WT_DROP>", common::Reply::Text("ready to discard".into()));
    mock.route(
        "<SLOW_HELPER>",
        common::Reply::Slow(Duration::from_secs(20), "slow report".into()),
    );
    let chat = s.chat(PermissionMode::Auto).await;
    let command = |action: &str, name: &str| {
        format!("tessera agents {{\"action\":\"{action}\",\"name\":\"{name}\"}}")
    };
    for (name, prompt, isolation) in [
        ("apply-helper", "<WT_APPLY>", "worktree"),
        ("drop-helper", "<WT_DROP>", "worktree"),
        ("slow-helper", "<SLOW_HELPER>", "none"),
    ] {
        s.send(&chat.id, &format!("tessera agents {{\"action\":\"start\",\"tasks\":[{{\"name\":\"{name}\",\"prompt\":\"{prompt}\",\"cwd\":\"repo\",\"model\":\"helper-api/m\",\"isolation\":\"{isolation}\"}}]}}")).await;
        s.settle(&chat.id).await;
    }
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let children = s.snapshot(&chat.id).await.children;
            if children
                .iter()
                .filter(|i| i.status == SessionStatus::Idle)
                .count()
                >= 2
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    for (action, name) in [
        ("status", "apply-helper"),
        ("report", "apply-helper"),
        ("apply", "apply-helper"),
        ("discard", "drop-helper"),
        ("stop", "slow-helper"),
    ] {
        if action == "stop" {
            s.send(&chat.id, "tessera agents {\"action\":\"message\",\"name\":\"slow-helper\",\"text\":\"follow fixture guidance\"}").await;
            s.settle(&chat.id).await;
            assert_eq!(s.logged("called").last().unwrap()["error"], false);
        }
        s.send(&chat.id, &command(action, name)).await;
        s.settle(&chat.id).await;
        assert_eq!(
            s.logged("called").last().unwrap()["error"],
            false,
            "{action}: {:?}",
            s.logged("called").last()
        );
    }
    // The bridge rejects an out-of-scope cwd before starting any child.
    let outside = tempfile::tempdir().unwrap();
    s.send(&chat.id, &format!("tessera agents {}", json!({"action":"start","wait":true,"tasks":[{"name":"outside","prompt":"<WT_APPLY>","model":"helper-api/m","cwd":outside.path()}]}))).await;
    s.settle(&chat.id).await;
    assert_eq!(s.logged("called").last().unwrap()["error"], true);
    assert!(
        tool_results(&s.snapshot(&chat.id).await.entries)
            .last()
            .unwrap()
            .2
            .contains("outside this chat's folder scope")
    );
    assert_eq!(
        std::fs::read_to_string(repo.join("app.txt")).unwrap(),
        "v2\n"
    );
    assert!(!repo.join("drop.txt").exists());
    let mut children = Vec::new();
    for child in s.snapshot(&chat.id).await.children {
        children.push(s.snapshot(&child.id).await.info);
    }
    assert_eq!(children.len(), 3);
    assert!(children.iter().all(|i| i.model.provider == "helper-api"));
    assert_eq!(
        children
            .iter()
            .filter(|i| i.cwd.contains(".git/tessera/worktrees"))
            .count(),
        2
    );
    for child in children.iter().filter(|i| i.title == "drop-helper") {
        assert!(!Path::new(&child.cwd).exists());
    }
}

#[tokio::test]
async fn wave2_tessera_external_mcp_relay_keeps_native_approval_and_names() {
    let dir = tempfile::tempdir().unwrap();
    let server = dir.path().join("echo.py");
    std::fs::write(&server, r#"
import json, sys
for line in sys.stdin:
    m = json.loads(line)
    if 'id' not in m: continue
    method = m['method']
    r = {'protocolVersion':'2025-06-18', 'capabilities':{'tools':{}}, 'serverInfo':{'name':'echo', 'version':'1'}} if method == 'initialize' else {}
    if method == 'tools/list': r = {'tools':[{'name':'shout','description':'Echo text','inputSchema':{'type':'object','properties':{'text':{'type':'string'}}},'annotations':{'readOnlyHint':False}}]}
    if method == 'tools/call': r = {'content':[{'type':'text','text':m['params']['arguments']['text'].upper()}]}
    print(json.dumps({'jsonrpc':'2.0','id':m['id'],'result':r}), flush=True)
"#).unwrap();
    let s = setup_with(&format!(
        "[[mcp]]\nname='lab'\ncommand='python3'\nargs=['{}']\n",
        server.display()
    ))
    .await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let chat = s.chat(PermissionMode::Default).await;
    let mut rx = s.engine.subscribe();
    s.send(
        &chat.id,
        "tessera mcp__lab__shout {\"text\":\"fixture hello\"}",
    )
    .await;
    let req = approval(&mut rx).await;
    assert!(req.reason.contains("MCP"), "{req:?}");
    decide(&s, &chat.id, req, ApprovalDecision::Allow).await;
    s.settle(&chat.id).await;
    assert!(
        s.logged("tools")[0]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t == "mcp__lab__shout")
    );
    assert_eq!(
        tool_calls(&s.snapshot(&chat.id).await.entries),
        ["mcp__lab__shout"]
    );
    assert!(
        tool_results(&s.snapshot(&chat.id).await.entries)
            .iter()
            .any(|(_, _, t)| t.contains("FIXTURE HELLO"))
    );
}

#[tokio::test]
async fn wave2_lsp_inventory_changes_on_enable_without_engine_restart() {
    let dir = tempfile::tempdir().unwrap();
    let server = dir.path().join("noop.py");
    std::fs::write(&server, "# not launched in this inventory test\n").unwrap();
    let s = setup_with(&format!("[lsp]\nenabled=false\n[[lsp.server]]\nname='fixture'\ncommand=['python3','{}']\nextensions=['foo']\n", server.display())).await;
    std::fs::write(s.work.join("app.foo"), "fixture\n").unwrap();
    let chat = s.chat(PermissionMode::Auto).await;
    s.send(&chat.id, "echo context").await;
    s.settle(&chat.id).await;
    assert!(
        !s.logged("tools")[0]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t == "lsp")
    );
    let mut rx = s.engine.subscribe();
    s.send(
        &chat.id,
        "tessera settings {\"action\":\"set\",\"key\":\"lsp.enabled\",\"value\":true}",
    )
    .await;
    let req = approval(&mut rx).await;
    decide(&s, &chat.id, req, ApprovalDecision::Allow).await;
    s.settle(&chat.id).await;
    s.send(&chat.id, "echo context").await;
    s.settle(&chat.id).await;
    assert!(
        s.logged("tools")
            .last()
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t == "lsp")
    );
    assert!(
        s.logged("context")
            .last()
            .unwrap()
            .to_string()
            .contains("descriptions give actions")
    );
}

#[tokio::test]
async fn wave2_added_machine_refreshes_warm_context_even_with_same_tool_names() {
    let s = setup_with("[agent]\nreview_interval = 0\nclaude_keep_open_minutes = 10\n").await;
    let chat = s.chat(PermissionMode::Auto).await;
    for (id, name) in [
        ("fixture-node-1", "Fixture Lab"),
        ("fixture-node-2", "Fixture Desk"),
    ] {
        let (conn, _rx) = s.engine.node_conn(id);
        s.engine
            .node_request(
                Request::NodeHello {
                    name: name.into(),
                    os: "linux".into(),
                    version: "test".into(),
                    scope: MachineScope {
                        commands: true,
                        ..Default::default()
                    },
                    generation: 1,
                    sandbox: "on".into(),
                    sandbox_ok: true,
                    home: Some("/home/fixture".into()),
                    computer: None,
                    host: None,
                    features: None,
                    guard_key: None,
                },
                &conn,
            )
            .await
            .unwrap();
        s.send(&chat.id, "echo context").await;
        s.settle(&chat.id).await;
        assert!(
            s.logged("context")
                .last()
                .unwrap()
                .to_string()
                .contains("descriptions give actions")
        );
        assert!(
            s.logged("tools")
                .last()
                .unwrap()
                .as_array()
                .unwrap()
                .iter()
                .any(|t| t == "machines")
        );
    }
    assert_eq!(s.runs().len(), 2);
}

#[tokio::test]
async fn wave2_secure_entry_rejects_values_in_arguments_and_claude_signin() {
    let s = setup().await;
    let chat = s.chat(PermissionMode::Auto).await;
    s.send(
        &chat.id,
        "tessera save_secret {\"name\":\"LAB_KEY\",\"value\":\"never-save-this-model-value\"}",
    )
    .await;
    s.settle(&chat.id).await;
    assert_eq!(s.logged("called").last().unwrap()["error"], true);
    let snap = s.snapshot(&chat.id).await;
    // Only the deliberate test user message contains it; no model call copies.
    for e in snap
        .entries
        .iter()
        .filter(|e| !matches!(e.body, EntryBody::User { .. }))
    {
        assert!(
            !serde_json::to_string(e)
                .unwrap()
                .contains("never-save-this-model-value")
        );
    }
    s.send(
        &chat.id,
        "tessera save_secret {\"name\":\"CLAUDE_LOGIN\",\"kind\":\"token\",\"action\":\"request\"}",
    )
    .await;
    s.settle(&chat.id).await;
    assert_eq!(s.logged("called").last().unwrap()["error"], true);
    assert!(
        s.call::<Vec<SecretInfo>>(Request::ListSecrets)
            .await
            .is_empty()
    );
}

#[tokio::test]
async fn wave2_bridged_lsp_navigation_references_diagnostics_and_rename() {
    let dir = tempfile::tempdir().unwrap();
    let server = dir.path().join("fixture-lsp.py");
    std::fs::write(&server, r#"
import json, sys
inp, out = sys.stdin.buffer, sys.stdout.buffer
def send(m):
    data = json.dumps(m).encode()
    out.write(('Content-Length: %d\r\n\r\n' % len(data)).encode()+data); out.flush()
def pos(): return {'start':{'line':0,'character':0}, 'end':{'line':0,'character':3}}
while True:
    headers={}
    while True:
        line=inp.readline()
        if not line: sys.exit()
        if line in (b'\r\n', b'\n'): break
        k,v=line.decode().split(':',1); headers[k.lower()]=v.strip()
    m=json.loads(inp.read(int(headers['content-length'])))
    method=m.get('method'); p=m.get('params',{})
    if method=='textDocument/didOpen':
        send({'jsonrpc':'2.0','method':'textDocument/publishDiagnostics','params':{'uri':p['textDocument']['uri'],'version':p['textDocument']['version'],'diagnostics':[{'range':pos(),'severity':2,'message':'fixture warning'}]}})
    if 'id' not in m: continue
    r=None
    if method=='initialize': r={'capabilities':{'textDocumentSync':1,'definitionProvider':True,'referencesProvider':True,'renameProvider':True}, 'serverInfo':{'name':'fixture'}}
    if method in ('textDocument/definition','textDocument/references'): r=[{'uri':p['textDocument']['uri'],'range':pos()}]
    if method=='textDocument/rename': r={'changes':{p['textDocument']['uri']:[{'range':pos(),'newText':p['newName']}]}}
    send({'jsonrpc':'2.0','id':m['id'],'result':r})
"#).unwrap();
    let s = setup_with(&format!("[lsp]\nenabled=true\n[[lsp.server]]\nname='fixture'\ncommand=['python3','{}']\nextensions=['foo']\n", server.display())).await;
    std::fs::write(s.work.join("app.foo"), "foo fixture\n").unwrap();
    let chat = s.chat(PermissionMode::Auto).await;
    for action in ["definition", "references", "diagnostics", "rename"] {
        s.send(&chat.id, &format!("tessera lsp {{\"action\":\"{action}\",\"path\":\"app.foo\",\"line\":1,\"symbol\":\"foo\",\"new_name\":\"bar\"}}")).await;
        s.settle(&chat.id).await;
        assert_eq!(
            s.logged("called").last().unwrap()["error"],
            false,
            "{action}: {:?}",
            s.logged("called").last()
        );
    }
    assert_eq!(
        std::fs::read_to_string(s.work.join("app.foo")).unwrap(),
        "bar fixture\n"
    );
    assert!(
        tool_results(&s.snapshot(&chat.id).await.entries)
            .iter()
            .any(|(_, _, t)| t.contains("fixture warning"))
    );
    let readonly = s.chat(PermissionMode::ReadOnly).await;
    s.send(&readonly.id, "tessera lsp {\"action\":\"rename\",\"path\":\"app.foo\",\"line\":1,\"symbol\":\"bar\",\"new_name\":\"unsafe\"}").await;
    s.settle(&readonly.id).await;
    assert_eq!(s.logged("called").last().unwrap()["error"], true);
    assert_eq!(
        std::fs::read_to_string(s.work.join("app.foo")).unwrap(),
        "bar fixture\n"
    );
    s.engine.shutdown().await;
}

#[tokio::test]
async fn wave2_settings_change_during_turn_refreshes_only_before_queued_turn() {
    let s = setup_with("[agent]\nreview_interval = 0\nclaude_keep_open_minutes = 10\n").await;
    let chat = s.chat(PermissionMode::Auto).await;
    s.send(&chat.id, "wave2 slow boundary").await;
    tokio::time::timeout(Duration::from_secs(5), async {
        while s.logged("boundary").is_empty() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    s.call::<Value>(Request::SetSecret {
        name: "BOUNDARY_KEY".into(),
        value: "boundary-test-value-only".into(),
    })
    .await;
    assert_eq!(s.runs().len(), 1, "live turn was restarted");
    s.engine
        .handle(
            Request::Send {
                session_id: chat.id.clone(),
                content: vec![ContentPart::text("echo context")],
                delivery: Delivery::FollowUp,
                attachments: vec![],
            },
            None,
        )
        .await
        .unwrap();
    s.settle(&chat.id).await;
    assert_eq!(s.runs().len(), 2, "queued turn used stale inventory");
    assert!(
        s.logged("context")
            .last()
            .unwrap()
            .to_string()
            .contains("BOUNDARY_KEY")
    );
    assert!(
        texts(&s.snapshot(&chat.id).await.entries)
            .iter()
            .any(|t| t == "Boundary finished.")
    );
}

#[tokio::test]
async fn wave2_claude_signin_is_never_saved_even_with_secret_saving_off() {
    let s = setup().await;
    let chat = s.chat(PermissionMode::Auto).await;
    let setting: Request = serde_json::from_value(
        json!({"method":"set_security", "params":{"save_secrets_from_chat":false}}),
    )
    .unwrap();
    s.call::<Value>(setting).await;
    let sentinel = "sk-ant-oat01-fixture-not-a-real-credential-897";
    s.send(&chat.id, &format!("Do not save {sentinel}")).await;
    s.settle(&chat.id).await;
    assert!(
        !serde_json::to_string(&s.snapshot(&chat.id).await)
            .unwrap()
            .contains(sentinel)
    );
    assert!(
        !serde_json::to_string(&s.logged("input"))
            .unwrap()
            .contains(sentinel)
    );
    assert!(
        s.call::<Vec<SecretInfo>>(Request::ListSecrets)
            .await
            .is_empty()
    );
    let denied = s
        .engine
        .handle(
            Request::SetSecret {
                name: "TEST_SIGNIN".into(),
                value: sentinel.into(),
            },
            None,
        )
        .await;
    assert!(denied.is_err());
}

#[tokio::test]
async fn wave2_private_search_uses_chosen_provider_without_local_context() {
    let s = setup_with("[web]\nsearch='duckduckgo'\n").await;
    let chat = s.session_on("claude-code/opus", None, true).await;
    s.send(&chat.id, "echo context").await;
    s.settle(&chat.id).await;
    assert_eq!(s.logged("tools")[0], json!(["web_search"]));
    assert_eq!(s.last_arg("--tools").as_deref(), Some("WebFetch"));
    assert_eq!(
        s.last_arg("--disallowedTools").as_deref(),
        Some("WebSearch")
    );
    let run = s.runs().pop().unwrap();
    assert!(run.contains("--strict-mcp-config") && run.contains("--no-session-persistence"));
    assert!(!run.contains("--append-system-prompt"));
    assert!(
        !serde_json::to_string(&s.logged("context"))
            .unwrap()
            .contains("# Tessera")
    );
}

#[tokio::test]
async fn wave2_refresh_waits_for_an_already_queued_second_turn_to_finish() {
    let s = setup_with("[agent]\nreview_interval=0\nclaude_keep_open_minutes=10\n").await;
    let chat = s.chat(PermissionMode::Auto).await;
    s.send(&chat.id, "wave2 slow boundary first").await;
    let shared = &s;
    let wait_boundary = |count| async move {
        tokio::time::timeout(Duration::from_secs(5), async {
            while shared.logged("boundary").len() < count {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
    };
    wait_boundary(1).await;
    s.engine
        .handle(
            Request::Send {
                session_id: chat.id.clone(),
                content: vec![ContentPart::text("wave2 slow boundary second")],
                delivery: Delivery::FollowUp,
                attachments: vec![],
            },
            None,
        )
        .await
        .unwrap();
    wait_boundary(2).await;
    // This second turn was delivered within the same Turn state; its earlier
    // `finished` flag must not mistake it for a safe restart boundary.
    s.call::<Value>(Request::SetSecret {
        name: "SECOND_TURN_KEY".into(),
        value: "test-only-second-turn-key".into(),
    })
    .await;
    s.engine
        .handle(
            Request::Send {
                session_id: chat.id.clone(),
                content: vec![ContentPart::text("echo context")],
                delivery: Delivery::FollowUp,
                attachments: vec![],
            },
            None,
        )
        .await
        .unwrap();
    s.settle(&chat.id).await;
    assert_eq!(s.runs().len(), 2);
    let snap = s.snapshot(&chat.id).await;
    assert_eq!(
        texts(&snap.entries)
            .iter()
            .filter(|t| *t == "Boundary finished.")
            .count(),
        2,
        "an in-flight queued turn was cut short"
    );
    assert!(
        s.logged("context")
            .last()
            .unwrap()
            .to_string()
            .contains("SECOND_TURN_KEY")
    );
}

#[tokio::test]
async fn secure_entry_resets_claude_when_password_was_seen_in_chat_or_tool_output() {
    for in_tool in [false, true] {
        let s = setup_with("[agent]\nreview_interval=0\nclaude_keep_open_minutes=10\n").await;
        let chat = s.chat(PermissionMode::Auto).await;
        let sentinel = "earlier-plaintext-fixture-897";
        if in_tool {
            s.send(&chat.id, "earlier password tool fixture").await;
        } else {
            s.send(&chat.id, &format!("Earlier chat password: {sentinel}"))
                .await;
        }
        s.settle(&chat.id).await;
        s.send(&chat.id, "tessera save_secret {\"action\":\"request\",\"name\":\"EARLIER_LOGIN\",\"kind\":\"token\"}").await;
        s.settle(&chat.id).await;
        let snap = s.snapshot(&chat.id).await;
        let id = snap
            .entries
            .iter()
            .find_map(|e| match &e.body {
                EntryBody::ToolResult {
                    call_id,
                    tool,
                    is_error: false,
                    details: Some(d),
                    ..
                } if tool == "save_secret" && d.get("secret_entry").is_some() => {
                    Some(call_id.clone())
                }
                _ => None,
            })
            .expect("secure card");
        let before = s.runs().len();
        s.call::<Value>(Request::SaveChatSecret {
            session_id: chat.id.clone(),
            call_id: id,
            fields: SecretEnv(BTreeMap::from([("value".into(), sentinel.into())])),
            chat_only: true,
        })
        .await;
        let text = serde_json::to_string(&s.snapshot(&chat.id).await).unwrap();
        assert!(!text.contains(sentinel));
        assert!(
            text.contains("Claude Code's own history")
                && text.contains("starts from Tessera's sanitized history")
        );
        s.send(&chat.id, "echo context").await;
        s.settle(&chat.id).await;
        assert_eq!(s.runs().len(), before + 1);
        assert!(!s.runs().last().unwrap().contains("--resume"));
        let last = s.prompts().pop().unwrap();
        assert!(!last.contains(sentinel));
    }
}

// Wave 3 uses only this stand-in, never a real subscription process.
#[tokio::test]
async fn wave3_context_control_is_correlated_and_not_cumulative_billing() {
    let s = setup().await;
    let chat = s.chat(PermissionMode::Default).await;
    std::fs::write(
        s.work.join("context-count"),
        r#"{"totalTokens":777,"maxTokens":200000,"memoryFiles":[{"path":"private-not-retained"}]}"#,
    )
    .unwrap();
    s.send(&chat.id, "hello").await;
    s.settle(&chat.id).await;
    let snap = s.snapshot(&chat.id).await;
    assert_eq!(snap.info.context.tokens, 777);
    assert_eq!(snap.info.context.window, 200000);
    assert_eq!(snap.info.context.source.as_deref(), Some("reported"));
    assert!(
        !serde_json::to_string(&snap)
            .unwrap()
            .contains("private-not-retained")
    );
    std::fs::remove_file(s.work.join("context-count")).unwrap();
    s.send(&chat.id, "hello again").await;
    s.settle(&chat.id).await;
    assert_eq!(
        s.snapshot(&chat.id).await.info.context.source.as_deref(),
        Some("estimated")
    );
}

#[tokio::test]
async fn wave3_scoped_checks_do_not_clear_another_file_or_failed_checks() {
    let s = setup_with("[agent]\nreview_interval=0\nverify_check='strict'\n[lsp]\nenabled=false\n")
        .await;
    let chat = s.chat(PermissionMode::Auto).await;
    let actions = builtin_actions(json!([
        {"tool":"Write","input":{"file_path":s.work.join("a.py"),"content":"print(1)\n"}},
        {"tool":"Write","input":{"file_path":s.work.join("b.py"),"content":"print(2)\n"}},
        {"tool":"Bash","input":{"command":"python3 -m py_compile a.py"},"result":"checked"},
        {"tool":"Bash","input":{"command":"python3 -m py_compile b.py"},"result":"SyntaxError: failed","is_error":true}
    ]));
    s.send(&chat.id, &actions).await;
    s.settle(&chat.id).await;
    let nudged = s.logged("nudged");
    assert_eq!(nudged.len(), 1, "bounded Stop feedback");
    let note = nudged[0].as_str().unwrap();
    assert!(note.contains("b.py") && note.contains("failed"), "{note}");
    assert!(!note.contains("You changed a.py"), "{note}");
}

#[tokio::test]
async fn wave3_post_write_obeys_quick_checks_and_strict_mode() {
    for (setting, want_note, want_check) in [
        ("verify_check='on'", false, true),
        ("verify_check='strict'", true, true),
        ("verify_check='on'\nquick_checks=false", true, false),
        ("verify_check='off'\nquick_checks=false", false, false),
    ] {
        let s = setup_with(&format!(
            "[agent]\nreview_interval=0\n{setting}\n[lsp]\nenabled=false\n"
        ))
        .await;
        let chat = s.chat(PermissionMode::Auto).await;
        let actions = builtin_actions(
            json!([{"tool":"Write","input":{"file_path":s.work.join("app.py"),"content":"print(1)\n"}}]),
        );
        s.send(&chat.id, &actions).await;
        s.settle(&chat.id).await;
        assert_eq!(!s.logged("nudged").is_empty(), want_note, "{setting}");
        let post: Vec<_> = s
            .logged("hook")
            .into_iter()
            .filter(|h| h["event"] == "PostToolUse")
            .collect();
        assert_eq!(post.len(), 1);
        assert_eq!(
            post[0]["reply"].to_string().contains("File check"),
            want_check,
            "{setting}: {post:?}"
        );
    }
}

#[tokio::test]
async fn wave3_rewind_reports_conflicts_and_missing_cli_snapshots() {
    let s = setup().await;
    let chat = s.chat(PermissionMode::Auto).await;
    std::fs::write(s.work.join("old.txt"), "original").unwrap();
    std::fs::write(s.work.join("large.txt"), vec![b'x'; 16 * 1024 * 1024 + 1]).unwrap();
    s.send(
        &chat.id,
        &format!("tessera read {}", json!({"path":s.work.join("old.txt")})),
    )
    .await;
    s.settle(&chat.id).await;
    // An unread, oversized target is now refused before any snapshot too.
    let actions = builtin_actions(json!([
        {"tool":"Write","input":{"file_path":s.work.join("old.txt"),"content":"agent"}},
        {"tool":"Write","input":{"file_path":s.work.join("large.txt"),"content":"replacement"}},
        {"tool":"Bash","input":{"command":"echo not-restored"}}
    ]));
    s.send(&chat.id, &actions).await;
    s.settle(&chat.id).await;
    std::fs::write(s.work.join("old.txt"), "user change").unwrap();
    for dry_run in [true, false] {
        let r: RewindResult = s
            .call(Request::Rewind {
                session_id: chat.id.clone(),
                to_seq: 0,
                files: true,
                conversation: false,
                dry_run,
            })
            .await;
        assert!(
            r.notes
                .iter()
                .any(|n| n.contains("old.txt")
                    && (n.contains("overwrite") || n.contains("overwrote"))),
            "{r:?}"
        );
        assert!(
            r.skipped
                .iter()
                .any(|s| s.path == "large.txt" && s.reason.contains("No saved snapshot")),
            "{r:?}"
        );
        assert!(r.not_undone.iter().any(|c| c.contains("not-restored")));
        assert_eq!(
            std::fs::read_to_string(s.work.join("old.txt")).unwrap(),
            if dry_run { "user change" } else { "original" }
        );
        let large = std::fs::read(s.work.join("large.txt")).unwrap();
        assert_eq!(large.len(), 16 * 1024 * 1024 + 1);
        assert!(large.iter().all(|b| *b == b'x'));
    }
}

#[tokio::test]
async fn wave3_stop_allowance_is_not_renewed_by_child_background_or_cli_false_flag() {
    for completion in [true, false] {
        let s = setup_with(&format!(
            "[agent]\nreview_interval=0\ncompletion_check={completion}\n"
        ))
        .await;
        let chat = s.chat(PermissionMode::Auto).await;
        std::fs::write(s.work.join("stop-storm"), "").unwrap();
        s.send(&chat.id, r#"tessera task {"done_criteria":["Impossible check"],"verify":[{"index":1,"status":"unmet","evidence":"not done"}],"plan":[{"text":"Actionable check","status":"pending"}]}"#).await;
        s.settle(&chat.id).await;
        assert_eq!(s.logged("nudged").len(), usize::from(completion));
        assert_eq!(s.logged("storm-child"), [json!({})]);
        assert_eq!(s.logged("storm-parent"), [json!({})]);
        if completion {
            assert_eq!(s.logged("again"), [json!({})]);
        }
    }
}

#[tokio::test]
async fn wave3_exhausted_cli_helper_refuses_builtin_and_external_actions() {
    let s = setup_with("[agent]\nsubagent_steps=1\n").await;
    let chat = s.chat(PermissionMode::Auto).await;
    let prompt = builtin_actions(json!([
        {"tool":"Bash","input":{"command":"echo must-not-run"}},
        {"tool":"mcp__external__danger","input":{}},
        {"tool":"Write","input":{"file_path":s.work.join("budget.txt"),"content":"bad"}}
    ]));
    s.send(&chat.id, &format!("tessera agents {}", json!({"action":"start","wait":true,"tasks":[{"name":"Budgeted","model":"claude-code/opus","max_steps":1,"prompt":prompt}]}))).await;
    s.settle(&chat.id).await;
    let executed = s.logged("executed");
    assert_eq!(
        executed.len(),
        1,
        "one permitted step, then report-only: {executed:?}"
    );
    assert_eq!(executed[0]["tool"], "Bash");
    let hooks = s.logged("hook");
    assert!(
        hooks.iter().any(|h| h["reply"]
            .to_string()
            .contains("Budget exhausted; not executed")),
        "{hooks:?}"
    );
    assert!(!s.work.join("budget.txt").exists());
}

#[tokio::test]
async fn wave3_rewind_restores_shared_native_helper_edits() {
    let s = setup().await;
    let chat = s.chat(PermissionMode::Auto).await;
    std::fs::write(s.work.join("shared.txt"), "original").unwrap();
    s._mock.route(
        "wave3 helper write",
        Reply::Tools(vec![
            ("read".into(), json!({"path":"shared.txt"})),
            (
                "write".into(),
                json!({"path":"shared.txt","content":"helper"}),
            ),
        ]),
    );
    s._mock
        .route("wave3 helper write", Reply::Text("Written.".into()));
    s.send(&chat.id, &format!("tessera agents {}", json!({"action":"start","wait":true,"tasks":[{"name":"Shared writer","prompt":"wave3 helper write","model":"mock/m","isolation":"shared"}]}))).await;
    s.settle(&chat.id).await;
    assert_eq!(
        std::fs::read_to_string(s.work.join("shared.txt")).unwrap(),
        "helper"
    );
    let r: RewindResult = s
        .call(Request::Rewind {
            session_id: chat.id.clone(),
            to_seq: 0,
            files: true,
            conversation: false,
            dry_run: false,
        })
        .await;
    assert_eq!(r.restored, ["shared.txt"]);
    assert_eq!(
        std::fs::read_to_string(s.work.join("shared.txt")).unwrap(),
        "original"
    );
}

#[tokio::test]
async fn wave3_observational_failures_share_one_stop_note() {
    let s = setup_with("[agent]\nreview_interval=0\nverify_check='off'\n").await;
    let chat = s.chat(PermissionMode::Auto).await;
    let actions = (0..5).map(|_| json!({"tool":"Bash","input":{"command":"make test"},"result":"failed: boom","is_error":true})).collect::<Vec<_>>();
    s.send(&chat.id, &builtin_actions(json!(actions))).await;
    s.settle(&chat.id).await;
    let notes = s.logged("nudged");
    assert_eq!(notes.len(), 1);
    assert!(
        notes[0]
            .as_str()
            .unwrap()
            .contains("Loop warning: this call has failed 2 times"),
        "{}",
        notes[0]
    );
    assert_eq!(s.logged("again"), [json!({})]);
}

/// The screen is the main chat's: Claude Code's own helpers share its
/// session and can't be stopped one by one, so their desktop calls are
/// refused and never reach the machine.
#[tokio::test]
async fn claude_code_helpers_cannot_use_the_screen() {
    let s = setup().await;
    let chat = s.chat(PermissionMode::Auto).await;
    let (conn, mut rx) = s.engine.node_conn("fixture-pc");
    s.engine
        .node_request(
            Request::NodeHello {
                name: "PC".into(),
                os: "windows x86_64".into(),
                version: "test".into(),
                scope: MachineScope {
                    commands: true,
                    desktop: true,
                    ask: MachineAsk::Chat,
                    ..Default::default()
                },
                generation: 1,
                sandbox: "off".into(),
                sandbox_ok: true,
                home: None,
                computer: None,
                host: None,
                features: Some(vec!["desktop".into()]),
                guard_key: None,
            },
            &conn,
        )
        .await
        .unwrap();
    let reached = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let r2 = reached.clone();
    tokio::spawn(async move {
        while let Some(ev) = rx.recv().await {
            if let Event::NodeCall { .. } = ev {
                r2.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
        }
    });
    s.send(&chat.id, "delegate screen").await;
    s.settle(&chat.id).await;
    let called = s.logged("called");
    let desk = called
        .iter()
        .find(|c| c["name"] == "desktop")
        .expect("the helper's desktop call");
    assert_eq!(desk["error"], true, "{desk}");
    assert!(
        desk["text"]
            .as_str()
            .unwrap_or_default()
            .contains("Helpers cannot use a computer's screen"),
        "{desk}"
    );
    assert_eq!(reached.load(std::sync::atomic::Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a_cancelled_cli_card_is_no_answer_not_a_denial_or_stop() {
    let s = setup().await;
    let chat = s.chat(PermissionMode::Ask).await;
    let mut rx = s.engine.subscribe();
    s.send(&chat.id, "permission dismissed").await;
    let _ = approval(&mut rx).await;
    s.settle(&chat.id).await;
    let snap = s.snapshot(&chat.id).await;
    let results = tool_results(&snap.entries);
    assert!(
        results
            .iter()
            .any(|r| r.1 && r.2.contains("closed without an answer")),
        "{results:?}"
    );
    assert_eq!(
        texts(&snap.entries).last().unwrap(),
        "Carried on after no answer."
    );
    assert!(
        !s.calls()
            .iter()
            .any(|c| c["value"]["request"]["subtype"] == "interrupt")
    );
    assert!(snap.approvals.is_empty());
}

#[tokio::test]
async fn bridged_agents_per_task_cwd_isolates_a_repo_below_the_chat_folder() {
    let s = setup().await;
    let repo = s.work.join("repo");
    std::fs::create_dir(&repo).unwrap();
    for args in [
        &["init", "-q"][..],
        &["add", "."][..],
        &["commit", "--allow-empty", "-qm", "init"][..],
    ] {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(args)
            .env("GIT_AUTHOR_NAME", "Test")
            .env("GIT_AUTHOR_EMAIL", "test@example.com")
            .env("GIT_COMMITTER_NAME", "Test")
            .env("GIT_COMMITTER_EMAIL", "test@example.com")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    s._mock
        .route("BRIDGE repo report", Reply::Text("repo inspected".into()));
    let chat = s.chat(PermissionMode::Auto).await;
    s.send(&chat.id, &format!("tessera agents {}", json!({"action":"start","wait":true,"tasks":[
        {"name":"Repo","prompt":"BRIDGE repo report","cwd":"repo","isolation":"worktree","model":"mock/m"}
    ]}))).await;
    s.settle(&chat.id).await;
    let calls = s.logged("called");
    assert_eq!(calls[0]["error"], false, "{calls:?}");
    let text = calls[0]["text"].as_str().unwrap();
    assert!(
        text.contains("repo inspected") && text.contains("repo/.git/tessera/worktrees/"),
        "{text}"
    );
    let kids = bridged_children(&s, &chat.id, 1).await;
    assert!(
        kids[0].cwd.contains("repo/.git/tessera/worktrees/"),
        "{:?}",
        kids[0]
    );
    assert!(!Path::new(&kids[0].cwd).exists());
}

#[tokio::test]
async fn bridged_memory_lists_ids_and_previews_without_saving() {
    let s = setup().await;
    let chat = s.chat(PermissionMode::Default).await;
    for args in [
        json!({"target":"memory","action":"add","content":"Signal SUCCESS means acceptance","preview":true}),
        json!({"target":"memory","action":"list"}),
        json!({"target":"memory","action":"add","content":"Signal SUCCESS means acceptance"}),
        json!({"target":"memory","action":"list"}),
    ] {
        s.send(&chat.id, &format!("tessera memory {args}")).await;
        s.settle(&chat.id).await;
    }
    let calls = s.logged("called");
    assert!(calls.iter().all(|c| c["error"] == false), "{calls:?}");
    assert!(calls[0]["text"].as_str().unwrap().contains("NOT saved"));
    assert!(
        !calls[1]["text"]
            .as_str()
            .unwrap()
            .contains("Signal SUCCESS")
    );
    let list = calls[3]["text"].as_str().unwrap();
    assert!(
        list.contains("remaining")
            && list.contains("[m-")
            && list.contains("Signal SUCCESS means acceptance"),
        "{list}"
    );
    let id = list.split('[').nth(1).unwrap().split(']').next().unwrap();
    s.send(&chat.id, &format!("tessera memory {}", json!({"target":"memory","action":"replace","old":id,"content":"Signal SUCCESS means accepted, not delivered"}))).await;
    s.settle(&chat.id).await;
    assert_eq!(s.logged("called").last().unwrap()["error"], false);
}

#[tokio::test]
async fn environment_changes_do_not_rebuild_claudes_frozen_context() {
    let s = setup().await;
    let chat = s.chat(PermissionMode::Auto).await;
    s.send(&chat.id, "first message").await;
    s.settle(&chat.id).await;
    let mut request = update(&chat.id);
    if let Request::UpdateSession { model, .. } = &mut request {
        *model = Some("claude-code/sonnet".parse().unwrap());
    }
    s.call::<Value>(request).await;
    for text in ["second message", "third message"] {
        s.send(&chat.id, text).await;
        s.settle(&chat.id).await;
    }
    let contexts = s.logged("context");
    assert_eq!(contexts.len(), 3);
    assert!(
        contexts
            .windows(2)
            .all(|p| serde_json::to_vec(&p[0]).unwrap() == serde_json::to_vec(&p[1]).unwrap())
    );
    let prompts = s.prompts();
    assert_eq!(prompts.len(), 3);
    assert!(
        !prompts[0].contains("[Environment update]"),
        "fresh context already holds current facts"
    );
    assert!(
        contexts[0]
            .as_str()
            .unwrap()
            .contains("- Model: claude-code/opus")
    );
    assert!(
        prompts[1].contains("[Environment update]") && prompts[1].contains("claude-code/sonnet")
    );
    assert!(
        !prompts[2].contains("[Environment update]"),
        "unchanged facts were resent"
    );
    assert!(s.runs()[1].contains("--resume"));
    assert_eq!(s.snapshot(&chat.id).await.info.status, SessionStatus::Idle);
}

#[tokio::test]
async fn unchanged_work_state_is_only_a_version_marker_in_request_snapshots() {
    let s = setup().await;
    let chat = s.chat(PermissionMode::Auto).await;
    s.send(&chat.id, r#"tessera task {"goal":"Unique current job","decisions":["Use normal readable prose"],"notes":["Old evidence"]}"#).await;
    s.settle(&chat.id).await;
    s.send(&chat.id, "continue once").await;
    s.settle(&chat.id).await;
    s.send(&chat.id, "continue again").await;
    s.settle(&chat.id).await;
    let prompts = s.prompts();
    assert!(prompts[1].contains("Goal: Unique current job"));
    assert!(prompts[2].contains("work state unchanged (version "));
    assert!(!prompts[2].contains("Unique current job") && !prompts[2].contains("Old evidence"));
    let contexts = s.logged("context");
    assert!(
        contexts
            .windows(2)
            .all(|p| serde_json::to_vec(&p[0]).unwrap() == serde_json::to_vec(&p[1]).unwrap())
    );
    // Explicit pruning is available to both native and bridged task tools.
    s.send(
        &chat.id,
        r#"tessera task {"decisions":[],"remove":{"notes":[1]}}"#,
    )
    .await;
    s.settle(&chat.id).await;
    let l = s.snapshot(&chat.id).await.info.task.unwrap();
    assert!(l.decisions.is_empty() && l.notes.is_empty());
    assert_eq!(l.archived["decisions"].count, 1);
    assert_eq!(l.archived["notes"].count, 1);
    let id = l.archived["notes"].artifact.clone();
    s.send(
        &chat.id,
        &format!("tessera recall {}", json!({"artifact": id})),
    )
    .await;
    s.settle(&chat.id).await;
    assert!(
        tool_results(&s.snapshot(&chat.id).await.entries)
            .iter()
            .any(|r| r.0 == "recall" && !r.1 && r.2.contains("Old evidence"))
    );
    println!(
        "CC ledger request snapshots: changed_bytes={}, unchanged_bytes={}, contexts=identical",
        prompts[1].len(),
        prompts[2].len()
    );
}

#[tokio::test]
async fn completion_ack_is_internal_and_an_unchanged_stop_hook_does_not_reask() {
    use tessera_core::tools::meta::{CHECK_AUTHOR, is_protocol_entry};
    let s = setup_with("[agent]\nreview_interval=0\n").await;
    let chat = s.chat(PermissionMode::Auto).await;
    std::fs::write(s.work.join("completion-ack"), "").unwrap();
    let mut events = s.engine.subscribe();
    s.send(
        &chat.id,
        r#"tessera task {"goal":"Combined ledger fixture","notes":["Retained evidence"],"plan":[{"text":"Check files","status":"pending"}]}"#,
    )
    .await;
    s.settle(&chat.id).await;
    assert_eq!(s.logged("nudged").len(), 1);
    assert!(
        s.snapshot(&chat.id)
            .await
            .entries
            .iter()
            .all(|e| !is_protocol_entry(e))
    );
    let raw = s.engine.store().entries(&chat.id, 0).unwrap();
    assert!(raw.iter().any(|e| matches!(&e.body, EntryBody::Assistant { author, .. } if author.as_deref() == Some(CHECK_AUTHOR))));
    while let Ok(e) = events.try_recv() {
        match e {
            Event::EntryAdded { entry, .. } => assert!(!is_protocol_entry(&entry)),
            Event::StreamDelta {
                delta: StreamDelta::Text { text, .. },
                ..
            } => assert!(!text.contains('✓'), "{text}"),
            _ => {}
        }
    }
    // A new process/turn uses the persisted acknowledgement, not t.nudged.
    s.send(&chat.id, "hello again").await;
    s.settle(&chat.id).await;
    assert_eq!(s.logged("nudged").len(), 1);
    s.send(&chat.id, r#"tessera task {"add_notes":["Unrelated note"]}"#)
        .await;
    s.settle(&chat.id).await;
    assert_eq!(s.logged("nudged").len(), 1);
    s.send(
        &chat.id,
        r#"tessera task {"update":[{"index":1,"status":"in_progress"}]}"#,
    )
    .await;
    s.settle(&chat.id).await;
    assert_eq!(
        s.logged("nudged").len(),
        2,
        "changed actionable work still checked"
    );
    s.send(
        &chat.id,
        r#"tessera task {"update":[{"index":1,"status":"blocked"}]}"#,
    )
    .await;
    s.settle(&chat.id).await;
    assert_eq!(s.logged("nudged").len(), 2, "blocked work waits");
    // H4's request versions and H2's open-state checks are independent:
    // unchanged requests carry one short marker, and Stop never repeats the ledger.
    let prompts = s.prompts();
    assert_eq!(
        prompts[1].matches("Goal: Combined ledger fixture").count(),
        1
    );
    assert_eq!(
        prompts[2].matches("work state unchanged (version ").count(),
        1
    );
    assert!(
        !prompts[2].contains("Combined ledger fixture")
            && !prompts[2].contains("Retained evidence")
    );
    for note in s.logged("nudged") {
        let note = note.as_str().unwrap();
        assert_eq!(note.matches("[Completion check]").count(), 1);
        assert!(
            !note.contains("Tessera work state")
                && !note.contains("Combined ledger fixture")
                && !note.contains("Retained evidence")
        );
    }
}

#[tokio::test]
async fn wave4_auto_memory_is_off_and_tessera_memory_is_the_single_system() {
    let s = setup().await;
    let chat = s.chat(PermissionMode::Auto).await;
    s.send(&chat.id, "hello").await;
    s.settle(&chat.id).await;
    assert_eq!(s.logged("auto_memory").last(), Some(&json!("1")));
    let context = s.logged("context");
    let text = context.last().unwrap().as_str().unwrap();
    assert!(text.contains("Only memory persists here"), "{text}");
    assert!(text.contains("auto-memory is off"), "{text}");
    assert!(text.contains("memory"), "{text}");
}

#[tokio::test]
async fn wave4_ordinary_commands_and_jobs_do_not_require_a_vault() {
    let s = setup().await;
    let chat = s.chat(PermissionMode::Auto).await;
    s.send(&chat.id, "hello").await;
    s.settle(&chat.id).await;
    let tools = s.logged("tools");
    let tools = tools.last().unwrap().as_array().unwrap();
    assert!(
        tools.contains(&json!("bash")) && tools.contains(&json!("job")),
        "{tools:?}"
    );
    s.send(&chat.id, "tessera bash {\"command\":\"printf ordinary\"}")
        .await;
    s.settle(&chat.id).await;
    assert!(
        tool_results(&s.snapshot(&chat.id).await.entries)
            .iter()
            .any(|(_, bad, text)| !bad && text.contains("ordinary"))
    );
    s.send(
        &chat.id,
        "tessera bash {\"command\":\"sleep 60\",\"background\":true}",
    )
    .await;
    s.settle(&chat.id).await;
    let snap = s.snapshot(&chat.id).await;
    let job = snap
        .entries
        .iter()
        .rev()
        .find_map(|e| match &e.body {
            EntryBody::ToolResult {
                tool,
                details: Some(d),
                ..
            } if tool == "bash" => d.get("job_id").and_then(Value::as_str),
            _ => None,
        })
        .expect("native job id")
        .to_owned();
    s.send(
        &chat.id,
        &format!("tessera job {{\"action\":\"kill\",\"id\":{}}}", json!(job)),
    )
    .await;
    s.settle(&chat.id).await;
    assert!(
        tool_results(&s.snapshot(&chat.id).await.entries)
            .last()
            .is_some_and(|(_, bad, _)| !bad)
    );
    // CLI task ids are not aliases for Tessera command ids.
    s.send(
        &chat.id,
        "tessera job {\"action\":\"kill\",\"id\":\"cli-task\"}",
    )
    .await;
    s.settle(&chat.id).await;
    assert!(
        tool_results(&s.snapshot(&chat.id).await.entries)
            .last()
            .unwrap()
            .1
    );
}

#[tokio::test]
async fn wave4_ordinary_command_approval_and_readonly_are_native() {
    let s = setup().await;
    let chat = s.chat(PermissionMode::Ask).await;
    let mut rx = s.engine.subscribe();
    s.send(&chat.id, "tessera bash {\"command\":\"printf approved\"}")
        .await;
    let req = approval(&mut rx).await;
    decide(&s, &chat.id, req, ApprovalDecision::Allow).await;
    s.settle(&chat.id).await;
    no_extra_approvals(&mut rx);
    let readonly = s.chat(PermissionMode::ReadOnly).await;
    s.send(&readonly.id, "tessera bash {\"command\":\"printf denied\"}")
        .await;
    s.settle(&readonly.id).await;
    assert!(
        tool_results(&s.snapshot(&readonly.id).await.entries)
            .last()
            .unwrap()
            .1
    );
}

#[tokio::test]
async fn wave4_legacy_helper_ownership_survives_restart_without_second_workers() {
    let s = setup().await;
    let chat = s.chat(PermissionMode::Auto).await;
    s.send(&chat.id, "delegate this").await;
    s.settle(&chat.id).await;
    let (_, cli) = child_of(&s.snapshot(&chat.id).await.entries, "toolu_ag");
    assert_eq!(
        s.snapshot(&cli).await.info.helper_owner,
        Some(HelperOwner::ClaudeCode)
    );
    s.send(&chat.id, &helper_start(Some("mock/m"), true, "work"))
        .await;
    s.settle(&chat.id).await;
    let native = s
        .snapshot(&chat.id)
        .await
        .children
        .into_iter()
        .find(|c| c.helper_owner == Some(HelperOwner::Tessera))
        .unwrap()
        .id;
    s.engine.shutdown().await;
    // Simulate only the legacy public info in this throwaway DB.
    let db = rusqlite::Connection::open(Paths::at(s.home.path()).db_file()).unwrap();
    for id in [&cli, &native] {
        db.execute(
            "UPDATE sessions SET info = json_remove(info, '$.helper_owner') WHERE id = ?1",
            [id],
        )
        .unwrap();
    }
    drop(db);
    let Setup {
        engine,
        home,
        fake,
        _work_dir: _keep_old_folder,
        ..
    } = s;
    drop(engine);
    let s = setup_in(home, fake, "").await;
    assert_eq!(
        s.snapshot(&cli).await.info.helper_owner,
        Some(HelperOwner::ClaudeCode)
    );
    assert_eq!(
        s.snapshot(&native).await.info.helper_owner,
        Some(HelperOwner::Tessera)
    );
    assert!(
        s.engine
            .handle(
                Request::Send {
                    session_id: cli.clone(),
                    content: vec![ContentPart::text("do more")],
                    delivery: Delivery::Auto,
                    attachments: Vec::new(),
                },
                None
            )
            .await
            .is_err(),
        "CLI mirror became independently runnable"
    );
    assert_eq!(
        s.snapshot(&chat.id).await.children.len(),
        2,
        "duplicate children after restore"
    );
}

#[tokio::test]
async fn wave4_duplicate_child_stop_and_late_completion_never_stop_parent() {
    let s = setup().await;
    let chat = s.chat(PermissionMode::Auto).await;
    let mut rx = s.engine.subscribe();
    s.send(&chat.id, "slow helper in the background late completion")
        .await;
    let child = subagent_session(&mut rx, "toolu_slow_ag").await;
    for _ in 0..2 {
        s.call::<Value>(Request::Abort {
            session_id: child.id.clone(),
        })
        .await;
    }
    s.settle(&chat.id).await;
    assert_eq!(
        s.snapshot(&child.id).await.info.status,
        SessionStatus::Interrupted
    );
    let stops = s
        .logged("in")
        .iter()
        .filter(|m| m["request"]["subtype"] == "stop_task")
        .count();
    assert_eq!(stops, 1, "duplicate stop_task requests");
    assert!(
        texts(&s.snapshot(&chat.id).await.entries)
            .iter()
            .any(|t| t == "Went on without it.")
    );
}

#[tokio::test]
async fn helper_switch_refreshes_claude_bridge_builtins_and_context() {
    let s = setup_with("[agent]\nreview_interval = 0\nclaude_keep_open_minutes = 10\n").await;
    let chat = s.chat(PermissionMode::Auto).await;
    for enabled in [true, false, true] {
        let mut u = update(&chat.id);
        if let Request::UpdateSession { subagents, .. } = &mut u {
            *subagents = Some(enabled);
        }
        s.call::<SessionInfo>(u).await;
        s.send(&chat.id, "inspect refreshed bridge").await;
        s.settle(&chat.id).await;
        let builtins = s.last_arg("--tools").unwrap();
        for tool in ["Agent", "Task", "Workflow", "SendMessage", "ListAgents"] {
            assert_eq!(
                builtins.split(',').any(|n| n == tool),
                enabled,
                "{builtins}"
            );
        }
        let tools = s.logged("tools").pop().unwrap();
        assert_eq!(
            tools.as_array().unwrap().iter().any(|n| n == "agents"),
            enabled
        );
        let context = s.logged("context").pop().unwrap();
        assert!(context.as_str().unwrap().contains(if enabled {
            "Helpers are on"
        } else {
            "Helpers are off"
        }));
    }
    assert_eq!(
        s.runs().len(),
        3,
        "a parked CLI must not keep the old allowlist"
    );
}

#[tokio::test]
async fn unavailable_helpers_are_not_offered_to_claude_bots_lean_chats_or_zero_slots() {
    let s = setup().await;
    let bot = s
        .save_bot("Clerk", "claude-code/opus", ToolPreset::Assistant)
        .await;
    let chat = s.session_on("claude-code/opus", Some(&bot.id), false).await;
    s.send(&chat.id, "inspect refreshed bridge").await;
    s.settle(&chat.id).await;
    assert!(
        !s.last_arg("--tools")
            .unwrap()
            .split(',')
            .any(|n| n == "Agent")
    );
    assert!(
        s.logged("context")
            .pop()
            .unwrap()
            .as_str()
            .unwrap()
            .contains("Helpers are off")
    );
    let lean: SessionInfo = s
        .call(Request::CreateSession {
            cwd: Some(s.work.display().to_string()),
            model: Some("claude-code/opus".parse().unwrap()),
            title: Some("Lean fixture".into()),
            bot_id: None,
            private: false,
            lean: true,
        })
        .await;
    s.send(&lean.id, "inspect refreshed bridge").await;
    s.settle(&lean.id).await;
    let tools = s.logged("tools").pop().unwrap();
    for tool in ["agents", "note", "whiteboard"] {
        assert!(
            !tools.as_array().unwrap().iter().any(|n| n == tool),
            "{tools}"
        );
    }
    assert!(
        !s.last_arg("--tools")
            .unwrap()
            .split(',')
            .any(|n| n == "Agent")
    );
    let zero = setup_with("[agent]\nreview_interval = 0\nmax_subagents = 0\n").await;
    let chat = zero.chat(PermissionMode::Auto).await;
    zero.send(&chat.id, "inspect refreshed bridge").await;
    zero.settle(&chat.id).await;
    assert!(
        !zero
            .last_arg("--tools")
            .unwrap()
            .split(',')
            .any(|n| n == "Agent")
    );
    assert!(
        !zero
            .logged("tools")
            .pop()
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n == "agents")
    );
}

#[tokio::test]
async fn mid_turn_helper_switch_relaunches_claude_at_an_action_boundary() {
    let s = setup().await;
    let chat = s.chat(PermissionMode::Auto).await;
    s.send(&chat.id, "helper inventory boundary").await;
    tokio::time::timeout(Duration::from_secs(10), async {
        while s.logged("helper-boundary").is_empty() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let mut u = update(&chat.id);
    if let Request::UpdateSession { subagents, .. } = &mut u {
        *subagents = Some(false);
    }
    s.call::<SessionInfo>(u).await;
    s.settle(&chat.id).await;
    assert_eq!(s.runs().len(), 2);
    assert!(
        !s.last_arg("--tools")
            .unwrap()
            .split(',')
            .any(|n| n == "Agent")
    );
    assert!(
        !s.logged("tools")
            .pop()
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n == "agents")
    );
    assert!(
        s.logged("context")
            .pop()
            .unwrap()
            .as_str()
            .unwrap()
            .contains("Helpers are off")
    );
    assert!(s.snapshot(&chat.id).await.entries.iter().any(|e| matches!(&e.body,
        EntryBody::User { author: Some(a), content } if a == "harness" && plain_text(content).contains("Helpers are off"))));
}

#[tokio::test]
async fn claude_rooms_refresh_every_speakers_helper_capabilities() {
    let s = setup().await;
    let bot = s
        .save_bot("Fixture", "claude-code/opus", ToolPreset::Full)
        .await;
    let room: SessionInfo = s
        .call(Request::CreateChannel {
            title: "Fixture room".into(),
            members: vec![bot.id],
            cwd: Some(s.work.display().to_string()),
        })
        .await;
    for enabled in [true, false, true] {
        let mut req = update(&room.id);
        if let Request::UpdateSession { subagents, .. } = &mut req {
            *subagents = Some(enabled);
        }
        s.call::<SessionInfo>(req).await;
        s.send(&room.id, "inspect refreshed bridge").await;
        s.settle(&room.id).await;
        assert_eq!(
            s.last_arg("--tools")
                .unwrap()
                .split(',')
                .any(|t| t == "Agent"),
            enabled
        );
        assert_eq!(
            s.logged("tools")
                .pop()
                .unwrap()
                .as_array()
                .unwrap()
                .iter()
                .any(|t| t == "agents"),
            enabled
        );
        assert!(
            s.logged("context")
                .pop()
                .unwrap()
                .as_str()
                .unwrap()
                .contains(if enabled {
                    "Helpers are on"
                } else {
                    "Helpers are off"
                })
        );
    }
}

#[tokio::test]
async fn empty_claude_helper_reports_get_one_cached_same_session_nudge() {
    for recover in [true, false] {
        let s = setup_with("[agent]\nreview_interval = 0\nclaude_keep_open_minutes = 0\n").await;
        let _: ClaudeCodeInfo = s
            .call(Request::ClaudeCodeStatus {
                refresh: true,
                provider: None,
            })
            .await;
        let chat = s.session_on("mock/m", None, false).await;
        s._mock.route("Run fixture helper", common::Reply::Tools(vec![("agent".into(), json!({"wait":true,"tasks":[{
            "name":"fixture","prompt": if recover {"empty helper report"} else {"empty helper report, never report"},
            "model":"claude-code/opus"
        }]}))]));
        s._mock.route(
            "Run fixture helper",
            common::Reply::Text("Parent done".into()),
        );
        s.send(&chat.id, "Run fixture helper").await;
        s.settle(&chat.id).await;
        let snap = s.snapshot(&chat.id).await;
        let report = snap
            .entries
            .iter()
            .find_map(|e| match &e.body {
                EntryBody::ToolResult {
                    tool,
                    content,
                    is_error,
                    ..
                } if tool == "agent" => Some((plain_text(content), *is_error)),
                _ => None,
            })
            .unwrap();
        assert_eq!(report.1, !recover, "{report:?}");
        assert!(
            report.0.contains(if recover {
                "fixture work is uncommitted"
            } else {
                "exhausted its budget without a final answer"
            }),
            "{report:?}"
        );
        assert_eq!(s.logged("report-nudge").len(), 1);
        let runs = s.runs();
        assert_eq!(runs.len(), 2, "{runs:?}");
        assert!(runs[1].contains("--resume"), "same CLI session: {runs:?}");
        let contexts = s.logged("context");
        assert_eq!(contexts.len(), 2);
        assert_eq!(
            contexts[0], contexts[1],
            "cached system prefix stays identical"
        );
        assert_eq!(
            s.logged("tools")[0],
            s.logged("tools")[1],
            "tools stay identical"
        );
    }
}

#[cfg(unix)]
#[tokio::test]
async fn native_and_bridged_bash_guard_credentials_aliases_and_indirect_reads() {
    let s = setup_with("[security]\nsandbox = 'off'\n").await;
    let mut rx = s.engine.subscribe();
    std::fs::create_dir_all(s.home.path().join("skills")).unwrap();
    std::fs::create_dir_all(s.home.path().join("memory")).unwrap();
    std::fs::write(s.home.path().join("memory/USER.md"), "protected sentinel").unwrap();
    std::os::unix::fs::symlink(s.home.path(), s.work.join("store-link")).unwrap();
    for file in [
        ".netrc",
        ".git-credentials",
        ".ssh/id_ed25519",
        ".aws/credentials",
        ".config/gcloud/application_default_credentials.json",
        ".azure/config",
        ".kube/config",
    ] {
        let path = s.work.join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "credential sentinel").unwrap();
    }
    std::os::unix::fs::symlink(".netrc", s.work.join("read-alias")).unwrap();
    let credentials = [
        "cat .netrc | head -20",
        "cat ~/.netrc | head -20",
        "cat .git-credentials | head",
        "cat .ssh/id_ed25519",
        "cat .aws/credentials",
        "cat .config/gcloud/application_default_credentials.json",
        "cat .azure/config",
        "cat .kube/config",
        "cat read-alias | head",
    ];
    let unknown = [
        "sha256sum -c list",
        "grep -r pattern .",
        "grep -R pattern .",
        "grep -f patterns log",
        "grep --file=patterns log",
        "cat log* | head",
        "cat log?",
        "cat log[12]",
        "cat missing | head",
        "cat log | head",
    ];
    for tool in ["Bash", "mcp__tessera__bash"] {
        // In Default, credential/uncertain guards must ask themselves, not
        // merely rely on Ask mode's general command question.
        let chat = s.chat(PermissionMode::Default).await;
        for command in credentials {
            s.send(
                &chat.id,
                &if tool.starts_with("mcp__") {
                    format!("tessera bash {}", json!({"command":command}))
                } else {
                    builtin_actions(json!([{ "tool":tool, "input":{"command":command} }]))
                },
            )
            .await;
            if tool.starts_with("mcp__") {
                let req = approval(&mut rx).await;
                assert!(req.reason.contains("credential"), "{req:?}");
                decide(&s, &chat.id, req, ApprovalDecision::Deny).await;
            }
            s.settle(&chat.id).await;
            assert!(
                tool_results(&s.snapshot(&chat.id).await.entries)
                    .last()
                    .unwrap()
                    .1
            );
            no_extra_approvals(&mut rx);
        }
        for mode in [PermissionMode::Default, PermissionMode::Ask] {
            let chat = s.chat(mode).await;
            for command in unknown {
                s.send(
                    &chat.id,
                    &if tool.starts_with("mcp__") {
                        format!("tessera bash {}", json!({"command":command}))
                    } else {
                        builtin_actions(json!([{ "tool":tool, "input":{"command":command} }]))
                    },
                )
                .await;
                decide(
                    &s,
                    &chat.id,
                    approval(&mut rx).await,
                    ApprovalDecision::Deny,
                )
                .await;
                s.settle(&chat.id).await;
                no_extra_approvals(&mut rx);
            }
        }
        for mode in [PermissionMode::Ask, PermissionMode::Auto] {
            let chat = s.chat(mode).await;
            for command in [
                "cat store-link/memory/USER.md | head -20".into(),
                format!(
                    "cat '{}/skills/../config.toml' | head -20",
                    s.home.path().display()
                ),
            ] {
                s.send(
                    &chat.id,
                    &if tool.starts_with("mcp__") {
                        format!("tessera bash {}", json!({"command":command}))
                    } else {
                        builtin_actions(json!([{ "tool":tool, "input":{"command":command} }]))
                    },
                )
                .await;
                s.settle(&chat.id).await;
                let results = tool_results(&s.snapshot(&chat.id).await.entries);
                assert!(results.last().unwrap().1, "{results:?}");
                if tool.starts_with("mcp__") {
                    assert!(
                        results.last().unwrap().2.contains("Tessera's own"),
                        "{results:?}"
                    );
                } else {
                    // The fake CLI records its own generic result; check
                    // the real host denial reason at the permission boundary.
                    let denied = s
                        .logged("hook")
                        .into_iter()
                        .rev()
                        .find(|h| h["callback"] == "tessera_policy")
                        .unwrap();
                    assert!(
                        denied["reply"]["hookSpecificOutput"]["permissionDecisionReason"]
                            .as_str()
                            .unwrap()
                            .contains("Tessera's own"),
                        "{denied:?}"
                    );
                }
                no_extra_approvals(&mut rx);
            }
        }
    }
    assert!(s.logged("executed").is_empty());
}
#[tokio::test]
async fn completion_stop_checks_unmet_criteria_despite_a_blocked_plan() {
    let s = setup_with("[agent]\nreview_interval=0\n").await;
    let chat = s.chat(PermissionMode::Auto).await;
    std::fs::write(s.work.join("completion-ack"), "").unwrap();
    s.send(&chat.id,r#"tessera task {"plan":[{"text":"Wait for device","status":"blocked"}],"done_criteria":["Fix local tests"],"verify":[{"index":1,"status":"unmet","evidence":"Fixable now"}]}"#).await;
    s.settle(&chat.id).await;
    let notes = s.logged("nudged");
    assert_eq!(notes.len(), 1);
    assert!(
        notes[0]
            .as_str()
            .unwrap()
            .contains("Fix local tests [unmet]"),
        "{notes:?}"
    );
}

#[tokio::test]
async fn completion_stop_rechecks_a_finished_dependency_while_another_helper_runs() {
    let s = setup_with("[agent]\nreview_interval=0\n").await;
    let chat = s.chat(PermissionMode::Auto).await;
    std::fs::write(s.work.join("completion-ack"), "").unwrap();
    s.send(&chat.id, "completion dependency fixture").await;
    s.settle(&chat.id).await;
    assert_eq!(
        s.logged("completion-stop"),
        vec![
            json!({"phase":"helpers waiting","nudged":true}),
            json!({"phase":"report needs action","nudged":true}),
            json!({"phase":"duplicate notification","nudged":false}),
            json!({"phase":"finished helpers","nudged":true}),
        ]
    );
    assert_eq!(s.logged("nudged").len(), 3);
    let notes = s.logged("nudged");
    assert!(
        notes.iter().all(|n| n
            .as_str()
            .unwrap()
            .contains("fast helper report needs action")),
        "{notes:?}"
    );
    let (_, internal) = s.engine.store().get_session(&chat.id).unwrap().unwrap();
    assert!(
        !internal.claude_dependencies,
        "last finished task clears the recovery marker"
    );
}

#[tokio::test]
async fn completion_stop_drops_the_ack_after_restart_interruption() {
    let mut s = setup_with("[agent]\nreview_interval=0\n").await;
    let chat = s.chat(PermissionMode::Auto).await;
    std::fs::write(s.work.join("completion-ack"), "").unwrap();
    s.send(
        &chat.id,
        r#"tessera task {"plan":[{"text":"Check files","status":"in_progress"}]}"#,
    )
    .await;
    s.settle(&chat.id).await;
    let (mut info, internal) = s.engine.store().get_session(&chat.id).unwrap().unwrap();
    assert!(internal.completion_acknowledged.is_some());
    info.status = SessionStatus::Running;
    s.engine.store().put_session(&info, &internal).unwrap();
    s.engine = Engine::open(Paths::at(s.home.path()), EngineOptions { scheduler: false })
        .await
        .unwrap();
    let (_, after) = s.engine.store().get_session(&chat.id).unwrap().unwrap();
    assert!(after.completion_acknowledged.is_none() && after.completion_pending.is_none());
    s.send(&chat.id, "Continue after restart").await;
    s.settle(&chat.id).await;
    assert_eq!(
        s.logged("nudged").len(),
        2,
        "same ledger is checked again after interrupted recovery"
    );
}
async fn wait_for_ledger_ready(s: &Setup) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while s.logged("ledger-ready").is_empty() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

async fn reopen_ledger_fixture(s: Setup) -> Setup {
    let Setup {
        engine,
        _mock,
        home,
        fake,
        work,
        _work_dir,
    } = s;
    drop(engine);
    let engine = Engine::open(Paths::at(home.path()), EngineOptions { scheduler: false })
        .await
        .unwrap();
    Setup {
        engine,
        _mock,
        home,
        fake,
        work,
        _work_dir,
    }
}

#[tokio::test]
async fn ledger_closed_before_queued_take_is_resent_with_work_even_after_restart() {
    for restart in [false, true] {
        let mut s = setup().await;
        let chat = s.chat(PermissionMode::Auto).await;
        s.call::<Value>(Request::SetGoal {
            session_id: chat.id.clone(),
            goal: "V1 current job".into(),
        })
        .await;
        std::fs::write(s.work.join("ledger-close-queued"), "").unwrap();
        s.send(&chat.id, "ledger change then wait").await;
        wait_for_ledger_ready(&s).await;
        s.call::<Value>(Request::Send {
            session_id: chat.id.clone(),
            content: vec![ContentPart::text("queued ledger work must not disappear")],
            delivery: Delivery::Steer,
            attachments: Vec::new(),
        })
        .await;
        s.settle(&chat.id).await;
        assert_eq!(s.logged("ledger-untaken").len(), 1);
        let snap = s.snapshot(&chat.id).await;
        assert_eq!(snap.info.status, SessionStatus::Error);
        let queued = snap.entries.iter().find(|e| matches!(&e.body, EntryBody::User { content, .. } if plain_text(content) == "queued ledger work must not disappear")).unwrap();
        let (_, internal) = s.engine.store().get_session(&chat.id).unwrap().unwrap();
        let link = internal.claude_code.unwrap();
        assert!(
            link.seq < queued.seq,
            "late output must not skip the unaccepted queued input"
        );
        assert!(
            link.ledger_version.is_empty(),
            "enqueued V2 must not be recorded as delivered"
        );
        if restart {
            s = reopen_ledger_fixture(s).await;
        }
        s.send(&chat.id, "continue the queued work").await;
        s.settle(&chat.id).await;
        let prompts = s.prompts();
        let resent = prompts.last().unwrap();
        assert!(
            resent.contains("V2 constraint: never deploy"),
            "restart={restart}: {resent}"
        );
        assert!(
            resent.contains("queued ledger work must not disappear"),
            "resume boundary skipped work: {resent}"
        );
        assert!(!resent.contains("work state unchanged"));
        s.send(&chat.id, "normal next message").await;
        s.settle(&chat.id).await;
        let prompts = s.prompts();
        assert!(prompts.last().unwrap().contains("work state unchanged"));
        assert!(
            !prompts
                .last()
                .unwrap()
                .contains("V2 constraint: never deploy")
        );
        assert_eq!(s.snapshot(&chat.id).await.info.status, SessionStatus::Idle);
    }
}

#[tokio::test]
async fn ledger_initial_input_without_take_does_not_advance_a_cold_or_warm_link() {
    for keep_open in [0, 2] {
        let mut s = setup_with(&format!(
            "[agent]\nclaude_keep_open_minutes = {keep_open}\nreview_interval = 0"
        ))
        .await;
        let chat = s.chat(PermissionMode::Auto).await;
        s.send(&chat.id, "first accepted turn").await;
        s.settle(&chat.id).await;
        let (_, before) = s.engine.store().get_session(&chat.id).unwrap().unwrap();
        let before = before.claude_code.unwrap();
        assert!(!before.ledger_version.is_empty());
        s.call::<Value>(Request::SetGoal {
            session_id: chat.id.clone(),
            goal: "V2 changed goal before initial take".into(),
        })
        .await;
        std::fs::write(s.work.join("ledger-close-initial"), "").unwrap();
        s.send(&chat.id, "initial pending work must not disappear")
            .await;
        s.settle(&chat.id).await;
        let (_, after) = s.engine.store().get_session(&chat.id).unwrap().unwrap();
        let after = after.claude_code.unwrap();
        assert_eq!((after.seq, after.ts), (before.seq, before.ts));
        assert!(after.ledger_version.is_empty());
        s = reopen_ledger_fixture(s).await;
        s.send(&chat.id, "continue after restart").await;
        s.settle(&chat.id).await;
        let prompts = s.prompts();
        let resent = prompts.last().unwrap();
        assert!(
            resent.contains("V2 changed goal before initial take")
                && resent.contains("initial pending work must not disappear"),
            "{resent}"
        );
        assert!(!resent.contains("work state unchanged"));
        assert_eq!(s.snapshot(&chat.id).await.info.status, SessionStatus::Idle);
    }
}

#[tokio::test]
async fn ledger_accepted_queued_delivery_keeps_the_short_marker_after_restart() {
    let mut s = setup().await;
    let chat = s.chat(PermissionMode::Auto).await;
    s.send(&chat.id, "ledger change then wait").await;
    wait_for_ledger_ready(&s).await;
    s.call::<Value>(Request::Send {
        session_id: chat.id.clone(),
        content: vec![ContentPart::text("accepted queued ledger work")],
        delivery: Delivery::Steer,
        attachments: Vec::new(),
    })
    .await;
    s.settle(&chat.id).await;
    assert_eq!(s.logged("ledger-accepted").len(), 1);
    let (_, internal) = s.engine.store().get_session(&chat.id).unwrap().unwrap();
    assert!(!internal.claude_code.unwrap().ledger_version.is_empty());
    s = reopen_ledger_fixture(s).await;
    s.send(&chat.id, "normal next message").await;
    s.settle(&chat.id).await;
    let prompts = s.prompts();
    let next = prompts.last().unwrap();
    assert!(next.contains("work state unchanged"), "{next}");
    assert!(
        !next.contains("V2 constraint: never deploy")
            && !next.contains("accepted queued ledger work")
    );
    assert_eq!(s.snapshot(&chat.id).await.info.status, SessionStatus::Idle);
}

#[tokio::test]
async fn completion_native_bridged_report_renews_stop_only_after_confirmed_delivery() {
    let s = setup().await;
    let chat = s.chat(PermissionMode::Auto).await;
    std::fs::write(s.work.join("completion-ack"), "").unwrap();
    s._mock.route(
        "COMPLETION_NATIVE_REPORT",
        Reply::Slow(
            Duration::from_secs(1),
            "A fixable report for the parent.".into(),
        ),
    );
    s.send(&chat.id, "completion native bridged fixture").await;
    s.settle(&chat.id).await;
    assert_eq!(
        s.runs().len(),
        1,
        "one live CLI, not a fresh Turn allowance: runs={:?}, stops={:?}, inputs={:?}",
        s.runs(),
        s.logged("completion-stop"),
        s.prompts()
    );
    assert_eq!(
        s.logged("completion-stop"),
        vec![
            json!({"phase":"native helper pending","nudged":true}),
            json!({"phase":"native report unconfirmed","nudged":false}),
            json!({"phase":"native report accepted","nudged":true}),
            json!({"phase":"native report replay","nudged":false}),
            json!({"phase":"native report done","nudged":false}),
        ]
    );
    let notes = s.logged("nudged");
    assert_eq!(notes.len(), 2);
    assert!(notes.iter().all(|n| {
        n.as_str()
            .unwrap()
            .contains("Apply the native helper report")
    }));
    let snap = s.snapshot(&chat.id).await;
    let report = snap.entries.iter().find(|e| matches!(&e.body, EntryBody::User { author: Some(a), .. } if a.starts_with("subagent:"))).unwrap();
    let children = bridged_children(&s, &chat.id, 1).await;
    assert_eq!(children[0].model.provider, "mock");
    let (_, internal) = s.engine.store().get_session(&chat.id).unwrap().unwrap();
    assert!(internal.completion_acknowledged.is_some());
    let link = internal.claude_code.unwrap();
    assert!(link.seq >= report.seq);
    assert!(!link.ledger_version.is_empty());
}

#[tokio::test]
async fn completion_unaccepted_native_report_preserves_confirmed_ledger_boundary() {
    let s = setup().await;
    let chat = s.chat(PermissionMode::Auto).await;
    for mark in ["completion-ack", "completion-native-close"] {
        std::fs::write(s.work.join(mark), "").unwrap();
    }
    s._mock.route(
        "COMPLETION_NATIVE_REPORT",
        Reply::Slow(
            Duration::from_secs(1),
            "A fixable report for the parent.".into(),
        ),
    );
    s.send(&chat.id, "completion native bridged fixture").await;
    s.settle(&chat.id).await;
    assert_eq!(
        s.logged("nudged").len(),
        1,
        "unconfirmed report cannot renew Stop"
    );
    let snap = s.snapshot(&chat.id).await;
    let report = snap.entries.iter().find(|e| matches!(&e.body, EntryBody::User { author: Some(a), .. } if a.starts_with("subagent:"))).unwrap();
    let (_, internal) = s.engine.store().get_session(&chat.id).unwrap().unwrap();
    let link = internal.claude_code.unwrap();
    assert!(
        link.seq < report.seq,
        "late output cannot skip the unseen report"
    );
    assert!(
        link.ledger_version.is_empty(),
        "uncertain delivery must resend the full ledger"
    );
    s.send(&chat.id, "Continue after lost report delivery")
        .await;
    s.settle(&chat.id).await;
    let prompts = s.prompts();
    let resent = prompts.last().unwrap();
    assert!(
        resent.contains("A fixable report for the parent."),
        "{resent}"
    );
    assert!(
        resent.contains("Apply the native helper report")
            && !resent.contains("work state unchanged"),
        "{resent}"
    );
    assert_eq!(s.logged("nudged").len(), 2);
}

#[tokio::test]
async fn completion_idle_background_bash_without_a_child_expires_its_ack_on_restart() {
    // Bound the entire setup/restart/recovery flow, including every engine
    // request, not just the loop waiting for background acknowledgement.
    tokio::time::timeout(Duration::from_secs(90), async {
        let mut s = setup().await;
        std::fs::write(s.work.join("completion-ack"), "").unwrap();
        let quiet = s.chat(PermissionMode::Auto).await;
        s.send(
            &quiet.id,
            r#"tessera task {"plan":[{"text":"Unchanged quiet work","status":"in_progress"}]}"#,
        )
        .await;
        s.settle(&quiet.id).await;
        let (_, quiet_before) = s.engine.store().get_session(&quiet.id).unwrap().unwrap();
        assert!(
            quiet_before.completion_acknowledged.is_some() && !quiet_before.claude_dependencies
        );
        let chat = s.chat(PermissionMode::Auto).await;
        s.send(&chat.id, "completion idle bash fixture").await;
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let (info, internal) = s.engine.store().get_session(&chat.id).unwrap().unwrap();
                if info.status == SessionStatus::Idle
                    && internal.claude_dependencies
                    && internal.completion_acknowledged.is_some()
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert!(
            s.snapshot(&chat.id).await.children.is_empty(),
            "Bash has no child session"
        );
        assert_eq!(s.logged("nudged").len(), 2);
        std::fs::write(s.work.join("completion-bash-crash"), "").unwrap();
        s.settle(&chat.id).await;
        let (info, before) = s.engine.store().get_session(&chat.id).unwrap().unwrap();
        assert_eq!(info.status, SessionStatus::Idle);
        assert!(before.claude_dependencies && before.completion_acknowledged.is_some());
        s = reopen_ledger_fixture(s).await;
        let (info, after) = s.engine.store().get_session(&chat.id).unwrap().unwrap();
        assert_eq!(info.status, SessionStatus::Idle);
        assert!(
            !after.claude_dependencies
                && after.completion_acknowledged.is_none()
                && after.completion_pending.is_none()
        );
        let (_, quiet_after) = s.engine.store().get_session(&quiet.id).unwrap().unwrap();
        assert_eq!(
            quiet_after.completion_acknowledged, quiet_before.completion_acknowledged,
            "unaffected idle chats keep their mark"
        );
        s.send(&quiet.id, "Continue unchanged quiet work").await;
        s.settle(&quiet.id).await;
        assert_eq!(s.logged("nudged").len(), 2);
        s.send(&chat.id, "Continue after background command recovery")
            .await;
        s.settle(&chat.id).await;
        assert_eq!(
            s.logged("nudged").len(),
            3,
            "unchanged ledger is checked after dependency loss"
        );
    })
    .await
    .expect("completion/restart regression exceeded its 90-second deadline");
}

// B27 Claude/native limits and file semantics (distinct native27 fixtures).
#[tokio::test]
async fn native27_watched_plan_hits_have_one_clean_line_and_no_wait() {
    for mode in ["weekly", "missing", "result-only", "credits", "api"] {
        let s = setup().await;
        let chat = s.chat(PermissionMode::Default).await;
        s.send(&chat.id, &format!("native27 limit {mode}")).await;
        s.settle(&chat.id).await;
        let snap = s.snapshot(&chat.id).await;
        assert_eq!(snap.info.status, SessionStatus::Error, "{mode}");
        let errors: Vec<_> = snap
            .entries
            .iter()
            .filter_map(|e| match &e.body {
                EntryBody::Notice {
                    level: tessera_proto::NoticeLevel::Error,
                    text,
                    ..
                } => Some(text),
                _ => None,
            })
            .collect();
        assert_eq!(errors.len(), 1, "{mode}: {errors:?}");
        let text = errors[0];
        assert!(
            !text.contains("Claude Code:")
                && !text.contains("· resets")
                && !text.contains("I'll continue"),
            "{text}"
        );
        if mode == "weekly" {
            assert!(
                text.contains("weekly") && text.contains("2026-10-12T16:00:00Z"),
                "{text}"
            );
        } else if mode == "credits" {
            assert!(text.contains("needs usage credits"), "{text}");
        } else {
            assert!(text.contains("didn't report a reset time"), "{text}");
        }
        assert_eq!(s.runs().len(), 1);
        assert!(
            texts(&snap.entries).is_empty(),
            "plan errors aren't assistant replies"
        );
    }
}

#[tokio::test]
async fn native27_unattended_reset_resumes_and_stop_cancels() {
    for mode in ["resume", "stop"] {
        let s = setup().await;
        let chat = s.chat(PermissionMode::Auto).await;
        s.send(&chat.id, &format!("native27 limit {mode}")).await;
        let until = Instant::now() + Duration::from_secs(5);
        loop {
            let snap = s.snapshot(&chat.id).await;
            if snap.entries.iter().any(|e| matches!(&e.body, EntryBody::Notice {text,..} if text.contains("Stop cancels the wait"))) { break; }
            assert!(
                Instant::now() < until,
                "no limit wait notice: {:?}",
                snap.entries
            );
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        if mode == "stop" {
            s.call::<Value>(Request::Abort {
                session_id: chat.id.clone(),
            })
            .await;
        }
        s.settle(&chat.id).await;
        let snap = s.snapshot(&chat.id).await;
        if mode == "resume" {
            assert!(
                texts(&snap.entries)
                    .join("\n")
                    .contains("native27 continued after reset")
            );
            assert_eq!(s.runs().len(), 2);
        } else {
            assert!(
                !texts(&snap.entries)
                    .join("\n")
                    .contains("continued after reset")
            );
            assert_eq!(s.runs().len(), 1);
            assert_eq!(snap.info.status, SessionStatus::Idle);
        }
    }
}

#[tokio::test]
async fn native27_bridged_files_have_native_tags_seen_lines_and_reader_errors() {
    let s = setup().await;
    fs::write(s.work.join("native27-unseen.txt"), "alpha\nbeta\n").unwrap();
    let chat = s.chat(PermissionMode::Auto).await;
    s.send(&chat.id, "native27 files").await;
    s.settle(&chat.id).await;
    assert_eq!(
        fs::read_to_string(s.work.join("native27.txt")).unwrap(),
        "gamma\nbeta\n"
    );
    let calls = s.logged("native27-tool");
    assert_eq!(calls.len(), 7, "{calls:?}");
    for call in &calls {
        match call["name"].as_str().unwrap() {
            "unseen" => {
                assert_eq!(call["error"], true);
                assert!(
                    call["text"].as_str().unwrap().contains("weren't shown"),
                    "{call}"
                );
            }
            "web_fetch" => {
                assert_eq!(call["error"], true);
                assert!(
                    !call["text"].as_str().unwrap().contains("Unknown tool"),
                    "{call}"
                );
            }
            _ => assert_eq!(call["error"], false, "{call}"),
        }
    }
    assert!(calls[1]["text"].as_str().unwrap().contains("1:alpha"));
    assert!(calls[5]["text"].as_str().unwrap().contains("native27.txt"));
    assert_eq!(
        fs::read_to_string(s.work.join("native27-unseen.txt")).unwrap(),
        "alpha\nbeta\n"
    );
    let list = s.logged("native27-inventory").pop().unwrap();
    for name in ["write", "edit", "glob", "web_fetch"] {
        let tool = list
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["name"] == name)
            .unwrap();
        assert_eq!(
            tool["_meta"]["anthropic/alwaysLoad"], false,
            "new tools are deferred"
        );
    }
}

#[tokio::test]
async fn native27_bridged_write_is_not_offered_to_read_only_chats() {
    let s = setup().await;
    let chat = s.chat(PermissionMode::ReadOnly).await;
    s.send(
        &chat.id,
        r#"tessera write {"path":"native27-forbidden","content":"no"}"#,
    )
    .await;
    s.settle(&chat.id).await;
    let call = s.logged("called").pop().unwrap();
    assert_eq!(call["error"], true);
    assert!(!s.work.join("native27-forbidden").exists());
}

#[tokio::test]
async fn native27_fake_first_text_new_and_kept_measurement() {
    let s = setup_with("[agent]\nreview_interval = 0\nclaude_keep_open_minutes = 10\n").await;
    let chat = s.chat(PermissionMode::Auto).await;
    let mut times = Vec::new();
    for msg in ["native27 speed first", "native27 speed kept"] {
        let mut rx = s.engine.subscribe();
        let now = Instant::now();
        s.send(&chat.id, msg).await;
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Ok(Event::StreamDelta {
                    session_id,
                    delta: tessera_proto::StreamDelta::Text { text, .. },
                }) = rx.recv().await
                    && session_id == chat.id
                    && !text.is_empty()
                {
                    times.push(now.elapsed().as_millis());
                    break;
                }
            }
        })
        .await
        .unwrap();
        s.settle(&chat.id).await;
    }
    assert_eq!(s.runs().len(), 1, "second turn is kept");
    println!(
        "native27 fake first-text milliseconds: new={} kept={}",
        times[0], times[1]
    );
}

#[tokio::test]
async fn native27_repeated_hits_notify_once_and_stop_after_three_waits() {
    let s = setup_with("[agent]\nreview_interval = 0\nnotify_after_minutes = 0.0000001\n").await;
    let chat = s.chat(PermissionMode::Auto).await;
    let mut rx = s.engine.subscribe();
    s.send(&chat.id, "native27 limit repeat").await;
    s.settle(&chat.id).await;
    assert_eq!(s.runs().len(), 4);
    let mut notices = 0;
    while let Ok(ev) = rx.try_recv() {
        if let Event::Notification { .. } = ev {
            notices += 1;
        }
    }
    assert_eq!(
        notices, 1,
        "all notifications in the episode, not just plan-wait titles"
    );
    assert_eq!(s.snapshot(&chat.id).await.info.status, SessionStatus::Error);
}

#[tokio::test]
async fn native27_bridged_write_uses_native_ask_approval() {
    let s = setup().await;
    let chat = s.chat(PermissionMode::Ask).await;
    let mut rx = s.engine.subscribe();
    s.send(
        &chat.id,
        r#"tessera write {"path":"native27-asked.txt","content":"approved"}"#,
    )
    .await;
    let req = approval(&mut rx).await;
    assert_eq!(req.tool, "write");
    assert!(!s.work.join("native27-asked.txt").exists());
    s.call::<Value>(Request::RespondApproval {
        session_id: chat.id.clone(),
        request_id: req.id,
        decision: ApprovalDecision::Allow,
    })
    .await;
    s.settle(&chat.id).await;
    assert_eq!(
        fs::read_to_string(s.work.join("native27-asked.txt")).unwrap(),
        "approved"
    );
}

#[tokio::test]
async fn native27_sdk_partial_blocks_keep_text_thinking_and_count_message_once() {
    let s = setup().await;
    let chat = s.chat(PermissionMode::Auto).await;
    s.send(&chat.id, "native27 partial blocks").await;
    s.settle(&chat.id).await;
    let snap = s.snapshot(&chat.id).await;
    assert_eq!(snap.info.status, SessionStatus::Idle, "{:?}", snap.entries);
    let entries: Vec<_> = snap.entries.iter().filter(|e| matches!(&e.body, EntryBody::Assistant {content,..} if content.iter().any(|p| matches!(p, AssistantPart::Text {text} if text.contains("native27 partial"))))).collect();
    assert_eq!(entries.len(), 1);
    let EntryBody::Assistant { content, usage, .. } = &entries[0].body else {
        unreachable!()
    };
    assert_eq!(content.len(), 2);
    assert!(
        matches!(&content[0], AssistantPart::Thinking {text,..} if text == "thinking in blocks")
    );
    assert!(
        matches!(&content[1], AssistantPart::Text {text} if text == "native27 partial blocks finished")
    );
    assert_eq!(usage.input, 11);
    assert_eq!(usage.output, 7);
}

#[tokio::test]
async fn native27_exact_agent_schema_runs_a_native_helper_with_shared_permissions() {
    let s = setup().await;
    let chat = s.chat(PermissionMode::ReadOnly).await;
    s.send(&chat.id, &format!("tessera agent {}", json!({"tasks":[{"name":"native27 helper","prompt":"Check files","model":"mock/m","readonly":true}],"wait":true}))).await;
    s.settle(&chat.id).await;
    let called = s.logged("called").pop().unwrap();
    assert_eq!(called["error"], false, "{called}");
    assert!(
        called["text"]
            .as_str()
            .unwrap()
            .contains("hello from the other model")
    );
    let kids = bridged_children(&s, &chat.id, 1).await;
    let child = s.snapshot(&kids[0].id).await;
    assert_eq!(child.info.model.provider, "mock");
    assert_eq!(child.info.permission, PermissionMode::ReadOnly);
    assert_eq!(child.info.helper_owner, Some(HelperOwner::Tessera));
    let snap = s.snapshot(&chat.id).await;
    let details = snap
        .entries
        .iter()
        .find_map(|e| match &e.body {
            EntryBody::ToolResult {
                tool,
                details: Some(d),
                ..
            } if tool == "agent" => Some(d),
            _ => None,
        })
        .unwrap();
    assert_eq!(details["subagents"][0]["session_id"], child.info.id);
    assert_eq!(details["subagents"][0]["background"], false);
    assert_eq!(
        s.runs().len(),
        1,
        "native helper doesn't launch another CLI"
    );
}

#[tokio::test]
async fn native27_limit_resume_notifies_once_total_and_next_episode_can_notify() {
    let s = setup_with("[agent]\nreview_interval = 0\nnotify_after_minutes = 0.0000001\n").await;
    let chat = s.chat(PermissionMode::Auto).await;
    for episode in 0..2 {
        let mut rx = s.engine.subscribe();
        s.send(&chat.id, "native27 limit resume").await;
        s.settle(&chat.id).await;
        let mut notices = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            if let Event::Notification { notification } = ev {
                notices.push(notification.title);
            }
        }
        assert_eq!(notices.len(), 1, "episode {episode}: {notices:?}");
        assert_eq!(
            notices[0], "Finished",
            "automatic waits stay quiet, then the awaited reply alerts once"
        );
        assert_eq!(s.snapshot(&chat.id).await.info.status, SessionStatus::Idle);
        fs::remove_file(s.work.join("native27-limit-hit")).unwrap();
    }
}

#[tokio::test]
async fn native27_identical_limit_window_notifies_once_across_outer_runs() {
    let s = setup_with("[agent]\nreview_interval = 0\nnotify_after_minutes = 0.0000001\n").await;
    let chat = s.chat(PermissionMode::Auto).await;
    let mut rx = s.engine.subscribe();
    for _ in 0..2 {
        s.send(&chat.id, "native27 limit repeat").await;
        s.settle(&chat.id).await;
    }
    let mut notices = 0;
    while let Ok(ev) = rx.try_recv() {
        if matches!(ev, Event::Notification { .. }) {
            notices += 1;
        }
    }
    assert_eq!(
        notices, 1,
        "no second waiting/error/Finished notification in the same reset window"
    );
}

#[tokio::test]
async fn unknown_cli_result_subtype_reports_unconfirmed_instead_of_success() {
    let s = setup().await;
    let chat = s.chat(PermissionMode::ReadOnly).await;
    s.send(&chat.id, "unknown result").await;
    s.settle(&chat.id).await;
    let snap = s.snapshot(&chat.id).await;
    assert!(snap.entries.iter().any(|e| matches!(&e.body, EntryBody::Notice{text,..} if text.contains("unrecognized or incomplete result") && text.contains("not confirmed"))), "{:?}", snap.entries);
}

async fn helper_budget27_cli_case(mode: &str) {
    let s = setup_with("[agent]\nsubagent_steps=1\ncompletion_check=false\n").await;
    let chat = s.chat(PermissionMode::Auto).await;
    s.send(&chat.id, &format!("tessera agents {}", json!({"action":"start", "wait":true,
        "tasks":[{"name":"One step", "model":"claude-code/opus", "prompt":format!("helper budget27 {mode}")}]}))).await;
    s.settle(&chat.id).await;
    let children = s.engine.store().children_of(&chat.id).unwrap();
    assert_eq!(children.len(), 1);
    let child = s.snapshot(&children[0].id).await;
    assert_eq!(child.info.steps, 1);
    assert!(
        texts(&child.entries)
            .iter()
            .any(|t| t.contains("Final budget27 report"))
    );
    assert!(
        tool_results(&child.entries)
            .iter()
            .any(|t| t.2.contains("Budget exhausted; not executed"))
    );
    if mode == "bridge" {
        let calls: Vec<_> = s
            .logged("called")
            .into_iter()
            .filter(|c| c["name"] == "docs")
            .collect();
        assert_eq!(calls.iter().filter(|c| c["error"] == false).count(), 1);
        assert_eq!(calls.iter().filter(|c| c["error"] == true).count(), 1);
    } else {
        assert_eq!(s.logged("executed").len(), 1);
        assert_eq!(
            s.logged("budget27-action")
                .iter()
                .filter(|c| c["allowed"] == false)
                .count(),
            1
        );
    }
}

#[tokio::test]
async fn helper_budget27_concurrent_cli_permissions_consume_only_one_step() {
    helper_budget27_cli_case("concurrent").await;
}

#[tokio::test]
async fn helper_budget27_sequential_cli_permissions_consume_only_one_step() {
    helper_budget27_cli_case("sequential").await;
}

#[tokio::test]
async fn helper_budget27_bridge_dispatch_cannot_bypass_step_allowance() {
    helper_budget27_cli_case("bridge").await;
}

#[tokio::test]
async fn helper_budget27_cli_refusal_and_executed_text_collision_stay_consumed() {
    for first in [
        json!({"tool":"Bash", "input":{"command":"echo refused"}, "cli_refuses":true}),
        json!({"tool":"Bash", "input":{"command":"echo error"}, "is_error":true, "result":"execution failed"}),
        json!({"tool":"Bash", "input":{"command":"echo collision"}, "is_error":true, "result":"not performed"}),
    ] {
        let s = setup_with("[agent]\nsubagent_steps=1\ncompletion_check=false\n").await;
        let chat = s.chat(PermissionMode::Auto).await;
        let refused = first["cli_refuses"] == true;
        let prompt = builtin_actions(json!([first, {"tool":"Glob", "input":{"pattern":"after*"}}]));
        s.send(
            &chat.id,
            &format!(
                "tessera agents {}",
                json!({"action":"start", "wait":true,
            "tasks":[{"name":"Refusal accounting", "model":"claude-code/opus", "prompt":prompt}]})
            ),
        )
        .await;
        s.settle(&chat.id).await;
        let executed = s.logged("executed");
        assert_eq!(executed.len(), if refused { 0 } else { 1 }, "{executed:?}");
        assert!(
            executed.iter().all(|e| e["tool"] == "Bash"),
            "no second action after a refusal/text collision"
        );
        let children = s.engine.store().children_of(&chat.id).unwrap();
        assert_eq!(children[0].steps, 1);
        let (_, internal) = s
            .engine
            .store()
            .get_session(&children[0].id)
            .unwrap()
            .unwrap();
        let budget = internal.helper_budget.unwrap();
        assert!(
            budget.closed && budget.calls.is_empty(),
            "terminal CLI helpers retain only the aggregate"
        );
        assert_eq!(budget.used, 1);
    }
}

#[tokio::test]
async fn helper_budget27_question_without_id_denies_without_reserving() {
    let s = setup_with("[agent]\nsubagent_steps=1\ncompletion_check=false\n").await;
    let chat = s.chat(PermissionMode::Auto).await;
    s.send(&chat.id, &format!("tessera agents {}", json!({"action":"start", "wait":true,
        "tasks":[{"name":"Missing question ID", "model":"claude-code/opus", "prompt":"question budget27 missing"}]}))).await;
    s.settle(&chat.id).await;
    let got = s.logged("question-budget27");
    assert_eq!(got[0]["reply"]["behavior"], "deny");
    assert_eq!(s.logged("executed").len(), 1);
}

#[tokio::test]
async fn helper_budget27_duplicate_questions_share_answer_and_one_cancel_is_local() {
    for cancelled in [false, true] {
        let s = setup().await;
        let chat = s.chat(PermissionMode::Auto).await;
        s.send(
            &chat.id,
            if cancelled {
                "question budget27 cancel duplicate"
            } else {
                "question budget27 duplicate"
            },
        )
        .await;
        let board = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let boards: Vec<Whiteboard> = s
                    .call(Request::ListBoards {
                        session_id: Some(chat.id.clone()),
                    })
                    .await;
                if let Some(board) = boards.into_iter().find(|b| b.ask.is_some()) {
                    break board;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let boards: Vec<Whiteboard> = s
            .call(Request::ListBoards {
                session_id: Some(chat.id.clone()),
            })
            .await;
        assert_eq!(boards.len(), 1, "one shared question task");
        s.call::<Value>(Request::AnswerBoard {
            id: board.id,
            ask_id: board.ask.unwrap().id,
            answer: AskAnswer::Choice { index: 1 },
        })
        .await;
        s.settle(&chat.id).await;
        let got = s.logged("question-budget27");
        assert_eq!(got.len(), 2);
        assert_eq!(
            got[0]["reply"]["behavior"],
            if cancelled { "deny" } else { "allow" }
        );
        assert_eq!(got[1]["reply"]["behavior"], "allow");
        if !cancelled {
            assert_eq!(got[0]["reply"], got[1]["reply"]);
        }
    }
}

// T9 lite only tests the stream-reported work/input contract. No real CLI,
// new exit classifications or supervisor machinery is part of these tests.
async fn lite_setup(kind: &str, count: usize, variant: &str) -> (Setup, SessionInfo) {
    let s = setup().await;
    std::fs::write(s.work.join("lite-kind"), kind).unwrap();
    std::fs::write(s.work.join("lite-count"), count.to_string()).unwrap();
    std::fs::write(s.work.join("lite-variant"), variant).unwrap();
    let chat = s.chat(PermissionMode::Auto).await;
    s.send(&chat.id, "lite background fixture").await;
    (s, chat)
}

async fn lite_started(kind: &str, count: usize) -> (Setup, SessionInfo) {
    lite_started_variant(kind, count, "").await
}

async fn lite_started_variant(kind: &str, count: usize, variant: &str) -> (Setup, SessionInfo) {
    let (s, chat) = lite_setup(kind, count, variant).await;
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let snap = s.snapshot(&chat.id).await;
            if !s.logged("lite-ready").is_empty()
                && notices(&snap.entries)
                    .iter()
                    .any(|(_, text)| text.contains(&format!("background work ({count} running)")))
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "no visible background wait/count for {kind}; calls {:?}",
            s.calls()
        )
    });
    assert_eq!(s.snapshot(&chat.id).await.info.status, SessionStatus::Idle);
    (s, chat)
}

async fn lite_advance(seconds: u64) {
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(seconds)).await;
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
    tokio::time::resume();
}

async fn lite_log(s: &Setup, kind: &str) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while s.logged(kind).is_empty() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("missing {kind}; calls {:?}", s.calls()));
}

async fn lite_probe(s: &Setup, token: &str) {
    std::fs::write(s.work.join("lite-probe"), token).unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while !s.logged("lite-probe").iter().any(|v| v == token) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "bridge probe {token} was not acknowledged; calls {:?}",
            s.calls()
        )
    });
}

async fn lite_finish_grace(s: &Setup, chat: &SessionInfo) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !notices(&s.snapshot(&chat.id).await.entries)
            .iter()
            .any(|(_, text)| text.contains("background work ended"))
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("bridge did not observe work ending");
    lite_probe(s, "ended").await;
    lite_advance(590).await;
    lite_probe(s, "before-grace").await; // Still accepts a bridge request before ten minutes.
    assert!(s.logged("lite-closed").is_empty());
    assert!(s.logged("lite-early-eof").is_empty());
    // The probe is itself a stream line, so silence is measured from that ACK.
    lite_advance(601).await;
    s.settle(&chat.id).await;
    lite_log(s, "lite-closed").await;
}

#[tokio::test]
async fn lite_agent_and_bash_input_stays_open_until_reported_work_ends() {
    for kind in ["Agent", "Task", "Bash", "PowerShell", "Monitor", "wakeups"] {
        let (s, chat) = lite_started(kind, 1).await;
        lite_advance(11 * 60).await; // Beyond T1's idle-process retention.
        lite_probe(&s, "still-running").await;
        assert!(
            s.logged("lite-early-eof").is_empty(),
            "{kind} lost its input"
        );
        assert!(s.logged("lite-closed").is_empty());
        std::fs::write(s.work.join("lite-release"), "end").unwrap();
        lite_log(&s, "lite-ended").await;
        lite_finish_grace(&s, &chat).await;
    }
}

#[tokio::test]
async fn lite_eleven_background_agents_do_not_die_at_the_old_idle_limit() {
    let (s, chat) = lite_started("Agent", 11).await;
    lite_advance(11 * 60).await;
    assert!(s.logged("lite-early-eof").is_empty());
    s.send(&chat.id, "Are all eleven still running?").await;
    lite_log(&s, "lite-new-message").await;
    lite_log(&s, "lite-live").await;
    assert_eq!(s.logged("lite-live").last(), Some(&json!(11)));
    assert_eq!(s.runs().len(), 1, "message goes into the waiting child");
    assert_eq!(s.snapshot(&chat.id).await.children.len(), 11);
    std::fs::write(s.work.join("lite-follow-up"), "yes").unwrap();
    std::fs::write(s.work.join("lite-release"), "end").unwrap();
    s.settle(&chat.id).await;
    assert!(
        texts(&s.snapshot(&chat.id).await.entries)
            .iter()
            .any(|t| t.contains("Background follow-up is a normal turn"))
    );
    assert!(s.logged("lite-early-eof").is_empty());
}

#[tokio::test]
async fn lite_hard_cap_closes_input_with_a_plain_notice() {
    let (s, chat) = lite_started("Bash", 1).await;
    lite_advance(12 * 3600 + 1).await;
    s.settle(&chat.id).await;
    assert_eq!(
        notices(&s.snapshot(&chat.id).await.entries)
            .iter()
            .filter(|(_, text)| text.contains("Stopped waiting")
                && text.contains("12 hours")
                && text.contains("1 still reported running"))
            .count(),
        1,
        "exactly one cap notice"
    );
    lite_log(&s, "lite-early-eof").await;
}

#[tokio::test]
async fn lite_stop_during_background_wait_ends_it() {
    let (s, chat) = lite_started("Agent", 1).await;
    s.engine
        .handle(
            Request::Abort {
                session_id: chat.id.clone(),
            },
            None,
        )
        .await
        .unwrap();
    s.settle(&chat.id).await;
    assert!(!s.logged("lite-interrupted").is_empty());
    assert!(
        notices(&s.snapshot(&chat.id).await.entries)
            .iter()
            .any(|(_, text)| text == "Stopped.")
    );
    lite_log(&s, "lite-closed").await;
}

#[tokio::test]
async fn lite_missing_flags_starts_and_result_first_identity_keep_input_open() {
    for kind in ["Monitor", "Bash", "PowerShell", "Agent", "Task"] {
        for variant in ["missing-flag", "missing-start", "result-first"] {
            let (s, chat) = lite_started_variant(kind, 1, variant).await;
            lite_advance(11 * 60).await;
            lite_probe(&s, "running").await;
            assert!(s.logged("lite-early-eof").is_empty(), "{kind}/{variant}");
            std::fs::write(s.work.join("lite-release"), "end").unwrap();
            lite_finish_grace(&s, &chat).await;
        }
    }
}

#[tokio::test]
async fn lite_agent_id_only_completion_and_replayed_starts_do_not_resurrect_work() {
    for variant in ["agent-id", "replay"] {
        let (s, chat) = lite_started_variant("Agent", 1, variant).await;
        std::fs::write(s.work.join("lite-release"), "end").unwrap();
        lite_finish_grace(&s, &chat).await;
        let snap = s.snapshot(&chat.id).await;
        assert!(
            snap.children
                .iter()
                .all(|child| child.status == SessionStatus::Idle)
        );
        assert_eq!(
            notices(&snap.entries)
                .iter()
                .filter(|(_, text)| text.contains("background work (1 running)"))
                .count(),
            1
        );
    }
}

#[tokio::test]
async fn lite_denied_refused_or_failed_background_agents_do_not_become_phantom_work() {
    for variant in ["denied", "refused", "failed", "early-completion"] {
        let (s, chat) = lite_setup("Agent", 1, variant).await;
        s.settle(&chat.id).await;
        let snap = s.snapshot(&chat.id).await;
        let expected = if variant == "early-completion" {
            SessionStatus::Idle
        } else {
            SessionStatus::Error
        };
        assert!(snap.children.iter().all(|child| child.status == expected));
        assert_eq!(snap.children.len(), 1);
        assert!(
            !notices(&snap.entries)
                .iter()
                .any(|(_, text)| text.contains("Waiting on Claude Code's background work"))
        );
        assert_eq!(
            tool_results(&snap.entries).iter().any(|result| result.1),
            variant != "early-completion"
        );
    }
}

#[tokio::test]
async fn lite_child_stop_roster_cannot_clear_parent_and_parent_zero_gets_grace() {
    let (s, chat) = lite_started_variant("wakeups", 1, "child-stop").await;
    lite_log(&s, "lite-child-stop").await;
    lite_advance(11 * 60).await;
    lite_probe(&s, "parent-still-running").await;
    assert!(s.logged("lite-early-eof").is_empty());
    std::fs::write(s.work.join("lite-release"), "end").unwrap();
    lite_finish_grace(&s, &chat).await;
}

#[tokio::test]
async fn lite_repeated_wakeup_and_helper_cycles_accept_updates_then_end() {
    let (s, chat) = lite_started_variant("Agent", 1, "cycles").await;
    std::fs::write(s.work.join("lite-release"), "end").unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        while s.logged("lite-cycle").len() != 12 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("cycles did not finish through the bridge");
    lite_probe(&s, "cycles-complete").await;
    lite_finish_grace(&s, &chat).await;
    let snap = s.snapshot(&chat.id).await;
    assert_eq!(snap.children.len(), 13);
    assert!(
        snap.children
            .iter()
            .all(|child| child.status == SessionStatus::Idle)
    );
}

#[tokio::test]
async fn lite_foreground_agent_result_with_only_agent_id_has_no_background_wait() {
    for (kind, variant) in [
        ("Agent", "foreground-id"),
        ("Agent", "foreground-start"),
        ("Task", "foreground-id"),
        ("Task", "foreground-start"),
    ] {
        let (s, chat) = lite_setup(kind, 1, variant).await;
        s.settle(&chat.id).await;
        lite_log(&s, "lite-early-eof").await; // Parent's final result closes input, without a terminal notification.
        if variant == "foreground-start" {
            let starts = s.logged("lite-foreground-start");
            assert_eq!(starts.len(), 1);
            assert_eq!(starts[0]["is_backgrounded"], false);
        }
        let snap = s.snapshot(&chat.id).await;
        assert_eq!(snap.children.len(), 1);
        assert!(
            snap.children
                .iter()
                .all(|child| child.status == SessionStatus::Idle)
        );
        assert!(
            !notices(&snap.entries)
                .iter()
                .any(|(_, text)| text.contains("Waiting on Claude Code's background work"))
        );
        assert!(
            !s.calls()
                .iter()
                .any(|call| call["kind"] == "task-notification")
        );
    }
}

#[tokio::test]
async fn lite_completion_before_any_launch_or_after_many_results_cannot_resurrect_work() {
    for variant in ["completion-first", "completion-first-alias", "long-replay"] {
        let (s, chat) = lite_setup("Agent", 1, variant).await;
        s.settle(&chat.id).await;
        let snap = s.snapshot(&chat.id).await;
        assert_eq!(snap.children.len(), 1);
        assert!(
            snap.children
                .iter()
                .all(|child| child.status == SessionStatus::Idle),
            "{variant}"
        );
        assert!(
            !notices(&snap.entries)
                .iter()
                .any(|(_, text)| text.contains("Waiting on Claude Code's background work")),
            "{variant}"
        );
        if variant == "long-replay" {
            lite_log(&s, "lite-long-replay").await;
        }
    }
}

#[tokio::test]
async fn lite_lost_launch_result_retains_input_with_zero_confirmed_until_the_cap() {
    let (s, chat) = lite_setup("Agent", 1, "lost-launch").await;
    tokio::time::timeout(Duration::from_secs(8), async {
        while !notices(&s.snapshot(&chat.id).await.entries)
            .iter()
            .any(|(_, text)| text.contains("0 confirmed running"))
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("missing unresolved-request waiting notice");
    assert_eq!(s.snapshot(&chat.id).await.info.status, SessionStatus::Idle);
    lite_advance(11 * 60).await;
    lite_probe(&s, "unresolved-still-retained").await;
    assert!(s.logged("lite-early-eof").is_empty());
    lite_advance(12 * 3600 + 1).await;
    s.settle(&chat.id).await;
    let snap = s.snapshot(&chat.id).await;
    assert_eq!(
        notices(&snap.entries)
            .iter()
            .filter(|(_, text)| text.contains("Stopped waiting")
                && text.contains("12 hours")
                && text.contains("0 still reported running"))
            .count(),
        1
    );
    assert!(
        !notices(&snap.entries)
            .iter()
            .any(|(_, text)| text.contains("background work (1 running)")),
        "request is retention, not confirmed launch"
    );
    lite_log(&s, "lite-early-eof").await;
}

#[tokio::test]
async fn ordinary_stored_values_require_redacted_bridge_before_model_input() {
    let s = setup_with("[security]\nsandbox = 'off'\nredact_patterns = false\n").await;
    let value = "fixture-value-123456";
    let file = s.work.join("notes.txt");
    std::fs::write(&file, format!("{value}\nordinary line\n")).unwrap();
    s.engine.auth().set_secret("LAB_VALUE", value).unwrap();
    let chat = s.chat(PermissionMode::Auto).await;
    for tool in [
        "Read",
        "Grep",
        "Glob",
        "Bash",
        "PowerShell",
        "Monitor",
        "NotebookRead",
        "NotebookEdit",
        "Write",
        "Edit",
        "WebFetch",
        "LSP",
        "Skill",
        "Workflow",
        "ReadMcpResourceTool",
    ] {
        s.send(&chat.id, &builtin_actions(json!([{"tool":tool, "input":{"file_path":file, "path":file, "pattern":"ordinary", "command":"cat notes.txt"}}]))).await;
        s.settle(&chat.id).await;
    }
    assert!(s.logged("executed").is_empty());
    let run = s.logged("args").last().unwrap().as_array().unwrap().clone();
    let tools = run.windows(2).find(|pair| pair[0] == "--tools").unwrap()[1]
        .as_str()
        .unwrap()
        .split(',')
        .collect::<Vec<_>>();
    for tool in ["Read", "Grep", "Glob", "Bash", "Edit", "WebFetch", "LSP"] {
        assert!(!tools.contains(&tool));
    }
    assert!(run.iter().any(|arg| arg == "--strict-mcp-config"));
    for (name, args) in [
        ("read", json!({"path":file})),
        ("grep", json!({"path":file, "pattern":"fixture"})),
        ("bash", json!({"command":"cat notes.txt"})),
    ] {
        s.send(&chat.id, &format!("tessera {name} {args}")).await;
        s.settle(&chat.id).await;
        let called = s.logged("called").last().unwrap().clone();
        assert_eq!(called["error"], false);
        assert!(called["text"].as_str().unwrap().contains("[REDACTED]"));
        assert!(!called.to_string().contains(value));
    }
    // Every host-to-CLI message, including MCP results, is model-bound.
    assert!(!s.logged("in").iter().any(|m| m.to_string().contains(value)));
    assert!(
        !serde_json::to_string(&s.snapshot(&chat.id).await)
            .unwrap()
            .contains(value)
    );
}

#[tokio::test]
async fn native_edit_details_are_scrubbed_at_the_persistence_boundary() {
    let s = setup_with("[security]\nsandbox = 'off'\nredact_patterns = false\n").await;
    let value = "fixture-value-123456";
    std::fs::write(s.work.join("notes.txt"), format!("{value}\nold\n")).unwrap();
    s.engine.auth().set_secret("LAB_VALUE", value).unwrap();
    let chat = s.chat(PermissionMode::Auto).await;
    let mut events = s.engine.subscribe();
    s.send(&chat.id, "native edit context fixture").await;
    s.settle(&chat.id).await;
    let snapshot = s.snapshot(&chat.id).await;
    assert!(!serde_json::to_string(&snapshot).unwrap().contains(value));
    let diff = snapshot
        .entries
        .iter()
        .find_map(|entry| match &entry.body {
            EntryBody::ToolResult {
                details: Some(d), ..
            } => d["diff"].as_str(),
            _ => None,
        })
        .unwrap();
    assert!(diff.contains("[REDACTED]") && diff.contains("+new"));
    while let Ok(event) = events.try_recv() {
        assert!(!serde_json::to_string(&event).unwrap().contains(value));
    }
}
