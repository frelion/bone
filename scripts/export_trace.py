#!/usr/bin/env python3
"""Export persisted BONE sessions as one offline, browsable HTML record.

No model, tool, credential store or execution engine is opened. Source SQLite
records remain unchanged; shareable events omit opaque model and credential data.
"""

import argparse
import hashlib
import json
import re
import sqlite3
from collections import defaultdict
from datetime import datetime, timezone
from pathlib import Path


LABELS = {
    "input": "外部对话输入", "model_started": "模型调用开始",
    "model_message": "模型返回", "summary": "Job 内上下文压缩",
    "tool_started": "工具执行开始", "tool_result": "工具返回",
    "input_handled": "新指令已纳入工作", "question": "Agent 向外部提问",
    "delivery": "Agent 交付", "stopped": "Session 停止",
    "resumed": "Session 恢复", "cancelled": "调用被取消",
    "model_failed": "模型调用失败", "failure": "工作失败",
    "input_paused": "Agent 暂停工作", "input_resolved": "Agent 结算旧输入",
}
JOB_TOOLS = {"job_send", "job_wait", "job_handoff", "job_close", "job_control", "input_resolve", "pause_work"}


def sanitize(value, omissions):
    """Remove opaque model internals and structured transport credentials."""
    secret_fields = {"authorization", "proxy_authorization", "api_key", "access_token",
                     "refresh_token", "id_token", "client_secret", "password", "cookie", "token", "secret",
                     "set_cookie", "headers", "request_headers", "response_headers"}
    opaque_fields = {"reasoning", "encrypted", "encrypted_content", "reasoning_content"}
    if isinstance(value, dict):
        if value.get("type") in ("reasoning", "encrypted"):
            omissions["opaque_blocks"] += 1
            return None
        result = {}
        for key, item in value.items():
            normalized = re.sub(r"(?<!^)(?=[A-Z])", "_", key).lower().replace("-", "_")
            if normalized in secret_fields | opaque_fields:
                omissions["opaque_fields" if normalized in opaque_fields else "credential_transport_fields"] += 1
                continue
            result[key] = sanitize(item, omissions)
        return result
    if isinstance(value, list):
        result = []
        for item in value:
            cleaned = sanitize(item, omissions)
            if cleaned is not None or item is None:
                result.append(cleaned)
        return result
    if isinstance(value, str):
        try:
            decoded = json.loads(value)
        except ValueError:
            decoded = None
        if isinstance(decoded, (dict, list)):
            before = dict(omissions)
            cleaned = sanitize(decoded, omissions)
            if dict(omissions) != before:
                value = json.dumps(cleaned, ensure_ascii=False)
        patterns = (
            r"\b(?:sk-[A-Za-z0-9_-]{20,}|ghp_[A-Za-z0-9]{30,}|AKIA[A-Z0-9]{16})\b",
            r"\beyJ[A-Za-z0-9_-]{15,}\.[A-Za-z0-9_-]{15,}\.[A-Za-z0-9_-]{15,}\b",
            r"(?i)\bBearer\s+[A-Za-z0-9._~+/=-]{12,}",
            r"-----BEGIN (?:RSA |EC |OPENSSH )?PRIVATE KEY-----[\s\S]*?-----END (?:RSA |EC |OPENSSH )?PRIVATE KEY-----",
        )
        for pattern in patterns:
            value, count = re.subn(pattern, "[redacted credential]", value)
            omissions["credential_strings"] += count
        return value
    return value


def native_text(content):
    """Read native text/tool-result blocks; never interpret encrypted reasoning."""
    if isinstance(content, dict):
        kind = content.get("type")
        if kind in ("encrypted", "reasoning", "toolcall"):
            return ""
        if kind == "text":
            return content.get("text", "")
        return native_text(content.get("content", []))
    if isinstance(content, list):
        return "\n\n".join(t for item in content if (t := native_text(item)))
    return ""


def tool_key(event_id, call_id):
    return event_id + ":" + json.dumps(call_id, sort_keys=True, ensure_ascii=False, separators=(",", ":"))


def short_text(value, limit=220):
    text = " ".join(value.split())
    return text if len(text) <= limit else text[:limit] + "…"


def read_session(database, session_id=None, immutable=False):
    path = Path(database).resolve(strict=True)
    if immutable and Path(str(path) + "-wal").exists():
        raise ValueError("immutable export refuses a database with a WAL; use a normal read-only snapshot")
    uri = path.as_uri() + "?mode=ro" + ("&immutable=1" if immutable else "")
    with sqlite3.connect(uri, uri=True) as connection:
        connection.execute("PRAGMA query_only=ON")
        connection.execute("BEGIN")
        if session_id is None:
            ids = [r[0] for r in connection.execute("SELECT id FROM sessions")]
            if len(ids) != 1:
                raise ValueError("database must have exactly one session or supply session_id")
            session_id = ids[0]
        row = connection.execute("SELECT snapshot FROM sessions WHERE id=?", (session_id,)).fetchone()
        if row is None:
            raise ValueError(f"session not found: {session_id}")
        state = json.loads(row[0])
        entries = [{"sequence": seq, "raw": json.loads(payload)} for seq, payload in connection.execute(
            "SELECT sequence,payload FROM events WHERE session_id=? ORDER BY sequence", (session_id,)
        )]
    if len({x["raw"]["id"] for x in entries}) != len(entries):
        raise ValueError("duplicate event identities")
    if any(x["raw"]["session_id"] != session_id for x in entries):
        raise ValueError("mismatched session identity")
    return state, entries, str(path)


def add_views(entries):
    by_id = {x["raw"]["id"]: x for x in entries}
    calls = defaultdict(list)
    tools = {}
    for entry in entries:
        e = entry["raw"]
        if e.get("call_id"):
            calls[e["call_id"]].append(e)
        for choice in e["data"].get("response", {}).get("choice", []):
            if choice.get("type") == "toolcall":
                tools[tool_key(e["id"], choice["id"])] = (e["id"], choice["function"])

    for entry in entries:
        e, d = entry["raw"], entry["raw"]["data"]
        kind = e["kind"]
        response = d.get("response", {})
        text = native_text(d.get("message", {})) or native_text(response.get("choice", []))
        links = []

        def link(event_id, label):
            if event_id in by_id and event_id != e["id"] and not any(x["id"] == event_id for x in links):
                target = by_id[event_id]
                tool_name = target["raw"]["data"].get("tool_name")
                label += " · #" + str(target["sequence"])
                if tool_name:
                    label += " · " + tool_name
                links.append({"id": event_id, "label": label})

        category = "lifecycle"
        actor = "kernel"
        if kind in ("input", "question", "delivery"):
            category = "dialogue"
            actor = "external" if kind == "input" else "agent"
        if kind in ("model_started", "model_message", "summary"):
            category = "model"
            actor = "agent" if kind != "model_started" else "kernel"
        if kind in ("tool_started", "tool_result"):
            category = "tool"
            actor = "agent"
        if kind in ("failure", "model_failed", "cancelled"):
            category = "failure"
        if kind in ("input_resolved", "input_paused") or (kind == "input" and d.get("source") == "job"):
            category, actor = "job", "agent"
        label = LABELS.get(kind, kind)
        if kind == "input" and d.get("source") == "job":
            label = "Agent 给 Job 发送输入"
            link(d.get("sender_input"), "来源输入")
            # The kernel does not persist a separate job_created event. This is
            # the actual first input, not a fabricated creation timestamp.
        if kind == "question":
            text = d.get("question", "")
        if kind == "delivery":
            response_id = d.get("response_event")
            if response_id in by_id:
                text = native_text(by_id[response_id]["raw"]["data"].get("response", {}).get("choice", []))
            link(response_id, "原始交付正文")
        if kind in ("failure", "model_failed", "cancelled", "stopped", "input_resolved", "input_paused"):
            text = d.get("error") or d.get("reason") or d.get("text") or text
        if kind == "input_resolved":
            text = d.get("outcome", "") + "\n" + text
            link(d.get("actor_input"), "执行结算的输入")
        if kind == "input_handled":
            text = "输入已被所属 Job 纳入当前工作；指令版本 " + str(d.get("instructions_revision", e["revision"]))
        if kind == "resumed":
            text = "Session 恢复记录。恢复由外部调用触发；不代表 Agent 自行重启了进程。"
        if kind == "summary":
            label += " · " + str(len(d.get("covered_ids", []))) + " 个历史事件"
            link(d.get("previous_summary"), "先前摘要")
            for event_id in d.get("covered_ids", []):
                link(event_id, "摘要覆盖的原文")

        for related in calls.get(e.get("call_id"), []):
            link(related["id"], LABELS.get(related["kind"], related["kind"]))
        link(e.get("reply_to"), "对应输入")
        link(e.get("root_input"), "根输入 / 共享预算")
        key = d.get("tool_key")
        native_tool = tools.get(key)
        tool = None
        if kind in ("tool_started", "tool_result"):
            name = d.get("tool_name", "unknown")
            function = native_tool[1] if native_tool else {}
            started_tool = next((x for x in calls.get(e.get("call_id"), []) if x["kind"] == "tool_started"), None)
            effect = d.get("effect") or (started_tool["data"].get("effect") if started_tool else None)
            if name in JOB_TOOLS or name in {"job_inspect", "ask_user"}:
                effect = effect or "internal"
            tool = {"name": name, "arguments": function.get("arguments"), "effect": effect}
            label += " · " + name
            if native_tool:
                link(native_tool[0], "模型提出的原生调用")
            if name in JOB_TOOLS:
                category = "job"
            try:
                result = json.loads(text)
                if isinstance(result, dict):
                    args = function.get("arguments", {})
                    if (name == "job_send" and kind == "tool_result" and args.get("title")
                            and not args.get("job_id") and result.get("job_id") and result.get("input_id")):
                        label = "创建 Job 并发送输入 · job_send"
                    for field in ("input_id", "event_id", "response_event"):
                        link(result.get(field), "工具引用的 " + field)
            except (ValueError, TypeError):
                pass
        if native_tool:
            for sibling in entries:
                if sibling["raw"]["data"].get("tool_key") == key:
                    link(sibling["raw"]["id"], LABELS.get(sibling["raw"]["kind"], sibling["raw"]["kind"]))
        if kind == "model_message":
            names = [x["function"]["name"] for x in response.get("choice", []) if x.get("type") == "toolcall"]
            if names:
                label += " · " + " / ".join(names)
                if not text:
                    text = "本次模型返回原生工具调用，没有可读正文。参数与执行结果见关联事件。"
            for name_key, (model_id, _) in tools.items():
                if model_id == e["id"]:
                    for sibling in entries:
                        if sibling["raw"]["data"].get("tool_key") == name_key:
                            link(sibling["raw"]["id"], LABELS.get(sibling["raw"]["kind"], sibling["raw"]["kind"]))
        purpose = d.get("purpose")
        if kind == "model_started":
            label += " · " + ("摘要" if purpose == "summary" else "工作")
            text = "所属 Job 的" + ("摘要" if purpose == "summary" else "工作") + "调用；配置身份 " + d.get("profile", "未知")
        started = next((x for x in calls.get(e.get("call_id"), []) if x["kind"] in ("model_started", "tool_started")), None)
        elapsed = None
        if started is not None and started["id"] != e["id"]:
            elapsed = max(0, int(e["timestamp"]) - int(started["timestamp"]))
        entry["view"] = {
            **{k: e.get(k) for k in ("id", "kind", "job_id", "call_id", "reply_to", "root_input", "revision", "timestamp")},
            "category": category, "actor": actor, "source": d.get("source"),
            "label": label, "summary": short_text(text), "text": text,
            "tool": tool, "purpose": purpose, "elapsed_ms": elapsed,
            "usage": response.get("usage"), "links": links,
            "status": "failure" if category == "failure" else None,
        }


def export_session(spec):
    state, entries, path = read_session(spec["database"], spec.get("session_id"), spec.get("immutable", False))
    native_bytes = json.dumps([x["raw"] for x in entries], ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode()
    omissions = defaultdict(int)
    entries = sanitize(entries, omissions)
    state = sanitize(state, omissions)
    add_views(entries)
    exported_bytes = json.dumps([x["raw"] for x in entries], ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode()
    meta = sanitize(dict(spec.get("meta", {})), omissions)
    project = {"name": Path(state["workspace"]).name, "workspace": state["workspace"], **meta.pop("project", {})}
    for milestone in meta.get("milestones", []):
        if milestone["event_id"] not in {x["raw"]["id"] for x in entries}:
            raise ValueError("milestone is not backed by a native event: " + milestone["event_id"])
    return {
        "title": project["name"], "description": "持久 Session 执行记录的脱敏阅读视图。",
        "model": "未记录", "outcome": {"label": "读取快照", "description": "任务质量须结合原始记录判断。"},
        "caveats": [], "milestones": [], "annotations": [], **meta,
        "id": state["id"], "state": state, "project": project,
        "provenance": {"source_artifact": Path(path).name, "source_session_id": state["id"], "event_count": len(entries),
            "first_time": entries[0]["raw"]["timestamp"] if entries else None,
            "last_time": entries[-1]["raw"]["timestamp"] if entries else None,
            "source_events_sha256": hashlib.sha256(native_bytes).hexdigest(),
            "exported_events_sha256": hashlib.sha256(exported_bytes).hexdigest(),
            "omissions": dict(omissions),
            "export_policy": "Opaque reasoning and credential/transport fields omitted; source SQLite unchanged; source and exported payload fingerprints are distinct.",
            "hash_format": "event array; UTF-8 JSON; sorted keys; compact separators",
            "active_seconds": spec.get("active_seconds"), "read_only": True},
        "events": entries,
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    inputs = parser.add_mutually_exclusive_group(required=True)
    inputs.add_argument("--manifest", type=Path, help="JSON with title, subtitle and sessions[] source specifications")
    inputs.add_argument("--database", type=Path, help="a BONE sessions.sqlite, opened read-only")
    parser.add_argument("--source-root", type=Path, help="Local root for relative manifest database paths")
    parser.add_argument("--session", help="session ID, for --database")
    parser.add_argument("--immutable", action="store_true", help="only for a frozen database with no WAL")
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--template", type=Path, default=Path(__file__).parent / "templates" / "trace.html")
    args = parser.parse_args()
    manifest = json.loads(args.manifest.read_text()) if args.manifest else {"sessions": [{
        "database": str(args.database), "session_id": args.session, "immutable": args.immutable,
    }]}
    if args.manifest:
        for spec in manifest["sessions"]:
            path = Path(spec["database"])
            if not path.is_absolute():
                if args.source_root is None:
                    parser.error("relative manifest databases require --source-root")
                spec["database"] = str(args.source_root / path)
    data = {"version": 2, "generated_at": datetime.now(timezone.utc).isoformat(),
        "title": manifest.get("title", "BONE · 真实任务行动记录"),
        "subtitle": manifest.get("subtitle", "对话、Job、模型与工具的持久记录，已省略不透明模型与认证内容。"),
        "sessions": [export_session(spec) for spec in manifest["sessions"]]}
    template = args.template.read_text()
    if template.count("__BONE_TRACE_DATA__") != 1:
        raise ValueError("template must contain exactly one data placeholder")
    # Escape the HTML parser's script terminator, including in tool output or
    # hostile source code. Rendering uses textContent in the viewer.
    payload = json.dumps(data, ensure_ascii=False, separators=(",", ":")).replace("<", "\\u003c").replace("&", "\\u0026").replace("\u2028", "\\u2028").replace("\u2029", "\\u2029")
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(template.replace("__BONE_TRACE_DATA__", payload), encoding="utf-8")
    print(json.dumps({"output": str(args.output.resolve()), "bytes": args.output.stat().st_size,
        "sessions": [{"id": s["id"], "events": len(s["events"]), "exported_events_sha256": s["provenance"]["exported_events_sha256"]} for s in data["sessions"]]}))


if __name__ == "__main__":
    main()
