#!/usr/bin/env python3
"""Local HTTP Responses fixture, never a real model or credential endpoint.

Prints its loopback port once. The script is a JSON object with a `turns` array.
Each turn may require `contains` strings in the request JSON, pause via
`delay_seconds`, return `http_status`, or provide `output` Responses items.
Optional `delta_chunk_chars` splits text fragments; `event_delay_seconds`
or an `event_delays` mapping flushes SSE frames with per-event delays.
`response_status` and `incomplete_reason` produce native non-success endings;
`omit_terminal` ends the stream without a response terminal. Output items marked
`status: in_progress` keep their partial deltas without sending done frames.
`match_summary` selects work or compaction requests without guessing call order.
`release_file` holds a recorded request until that file exists beside the script.
Only request JSON bodies are recorded, never authorization headers.
"""

import argparse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
from pathlib import Path
import re
import threading
import time


def last_user_text(body):
    inputs = body.get("input", [])
    if not isinstance(inputs, list):
        return ""
    for item in reversed(inputs):
        if isinstance(item, dict) and item.get("role") == "user":
            return json.dumps(item.get("content", ""), ensure_ascii=False)
    return ""


def expand_output(output, body):
    """Resolve fixture IDs from actual job_send results, never predicted UUIDs."""
    input_ids = []
    job_ids = []

    def collect(value):
        if isinstance(value, str):
            try:
                decoded = json.loads(value)
            except ValueError:
                return
            if not isinstance(decoded, str):
                collect(decoded)
        elif isinstance(value, list):
            for entry in value:
                collect(entry)
        elif isinstance(value, dict):
            if isinstance(value.get("input_id"), str):
                input_ids.append(value["input_id"])
            if isinstance(value.get("job_id"), str):
                job_ids.append(value["job_id"])
            for entry in value.values():
                collect(entry)

    collect(body.get("input", []))

    def replace(value):
        if value == "$input_ids":
            return list(dict.fromkeys(input_ids))
        if value == "$latest_input_id":
            return input_ids[-1] if input_ids else "missing-fixture-input"
        if value == "$latest_job_id":
            return job_ids[-1] if job_ids else "missing-fixture-job"
        if isinstance(value, list):
            return [replace(entry) for entry in value]
        if isinstance(value, dict):
            return {name: replace(entry) for name, entry in value.items()}
        return value

    return replace(output)


def response_events(output, number, usage, delta_chunk_chars=0,
                    response_status="completed", incomplete_reason=None,
                    omit_terminal=False):
    response_id = "resp_fixture_%d" % number
    base = {"id": response_id, "object": "response", "created_at": 1,
            "model": "fixture", "status": "in_progress", "output": []}
    yield "response.created", {"type": "response.created", "response": dict(base, output=[])}
    for index, item in enumerate(output):
        item = dict(item)
        item.setdefault("id", "item_fixture_%d_%d" % (number, index))
        item.setdefault("status", "completed")
        finished = item["status"] == "completed"
        if item["type"] == "function_call" and not isinstance(item.get("arguments", "{}"), str):
            item["arguments"] = json.dumps(item["arguments"])
        if item["type"] == "message":
            item.setdefault("role", "assistant")
        added = dict(item, status="in_progress")
        if item["type"] == "function_call":
            added["arguments"] = ""
        if item["type"] == "message":
            added["content"] = []
        yield "response.output_item.added", {"type": "response.output_item.added",
                                              "output_index": index, "item": added}
        if item["type"] == "function_call":
            arguments = item.get("arguments", "{}")
            if not isinstance(arguments, str):
                arguments = json.dumps(arguments)
                item["arguments"] = arguments
            yield "response.function_call_arguments.delta", {
                "type": "response.function_call_arguments.delta", "item_id": item["id"],
                "output_index": index, "delta": arguments,
            }
            if finished:
                yield "response.function_call_arguments.done", {
                    "type": "response.function_call_arguments.done", "item_id": item["id"],
                    "output_index": index, "arguments": arguments,
                }
        elif item["type"] == "message":
            item.setdefault("role", "assistant")
            for content_index, content in enumerate(item.get("content", [])):
                yield "response.content_part.added", {
                    "type": "response.content_part.added", "item_id": item["id"],
                    "output_index": index, "content_index": content_index,
                    "part": {"type": "output_text", "text": "", "annotations": []},
                }
                text = content.get("text", "")
                chunks = [text] if not delta_chunk_chars else [text[offset:offset + delta_chunk_chars]
                    for offset in range(0, len(text), delta_chunk_chars)]
                for chunk in chunks:
                    yield "response.output_text.delta", {
                        "type": "response.output_text.delta", "item_id": item["id"],
                        "output_index": index, "content_index": content_index,
                        "delta": chunk,
                    }
                yield "response.output_text.done", {
                    "type": "response.output_text.done", "item_id": item["id"],
                    "output_index": index, "content_index": content_index,
                    "text": content.get("text", ""),
                }
                yield "response.content_part.done", {
                    "type": "response.content_part.done", "item_id": item["id"],
                    "output_index": index, "content_index": content_index,
                    "part": content,
                }
        if finished:
            yield "response.output_item.done", {"type": "response.output_item.done",
                                                 "output_index": index, "item": item}
        base["output"].append(item)
    if omit_terminal:
        return
    base["status"] = response_status
    base["usage"] = usage
    if incomplete_reason is not None:
        base["incomplete_details"] = {"reason": incomplete_reason}
    terminal = "response.incomplete" if response_status == "incomplete" else "response.completed"
    yield terminal, {"type": terminal, "response": base}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--script", type=Path, required=True)
    parser.add_argument("--requests", type=Path, required=True)
    args = parser.parse_args()
    turns = json.loads(args.script.read_text(encoding="utf-8"))["turns"]
    lock = threading.Lock()
    state = {"next": 0, "used": set()}

    class Handler(BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.1"

        def log_message(self, *_args):
            pass

        def do_POST(self):
            length = int(self.headers.get("Content-Length", "0"))
            if length > 8 * 1024 * 1024:
                self.send_error(413)
                return
            body = json.loads(self.rfile.read(length))
            encoded = json.dumps(body, ensure_ascii=False)
            with lock:
                number = state["next"]
                state["next"] += 1
                with args.requests.open("a", encoding="utf-8") as stream:
                    stream.write(json.dumps({"call": number, "body": body}) + "\n")
                turn = None
                for index, candidate in enumerate(turns):
                    if index in state["used"]:
                        continue
                    summary = body.get("instructions", "").startswith("Summarize this job")
                    if "match_summary" in candidate and candidate["match_summary"] != summary:
                        continue
                    selector = candidate.get("match_last_user_contains")
                    if selector is not None and selector not in last_user_text(body):
                        continue
                    title = candidate.get("match_job_title")
                    if title is not None:
                        match = re.search(r"currently working inside job \S+ \(([^\n]+)\)", body.get("instructions", ""))
                        if match is None or match.group(1) != title:
                            continue
                    turn = candidate
                    state["used"].add(index)
                    break
            if turn is None:
                self.send_error(500, "script exhausted")
                return
            if not all(fragment in encoded for fragment in turn.get("contains", [])):
                self.send_error(422, "fixture request expectation failed")
                return
            if "release_file" in turn:
                release = args.script.parent / turn["release_file"]
                while not release.exists():
                    time.sleep(0.01)
            time.sleep(turn.get("delay_seconds", 0))
            status = turn.get("http_status", 200)
            if status != 200:
                payload = json.dumps({"error": {"message": "scripted fixture error", "type": "fixture"}}).encode()
                self.send_response(status)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(payload)))
                self.end_headers()
                self.wfile.write(payload)
                return
            output = turn.get("output", [{"type": "message", "role": "assistant",
                                           "content": [{"type": "output_text", "text": turn.get("text", "done"), "annotations": []}]}])
            output = expand_output(output, body)
            usage = turn.get("usage", {"input_tokens": 10, "output_tokens": 5, "total_tokens": 15})
            events = list(response_events(output, number, usage, turn.get("delta_chunk_chars", 0),
                turn.get("response_status", "completed"), turn.get("incomplete_reason"),
                turn.get("omit_terminal", False)))
            for sequence, (_name, event) in enumerate(events):
                event["sequence_number"] = sequence
            streaming = body.get("stream", False)
            if streaming:
                payload = "".join("event: %s\ndata: %s\n\n" % (name, json.dumps(event))
                                  for name, event in events).encode()
            else:
                payload = json.dumps(events[-1][1]["response"]).encode()
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream" if streaming else "application/json")
            self.send_header("Content-Length", str(len(payload)))
            self.end_headers()
            try:
                if streaming and (turn.get("event_delay_seconds") or turn.get("event_delays") or turn.get("delta_chunk_chars")):
                    # Keep Content-Length framing but flush each SSE frame separately:
                    # clients receive genuine deltas before response.completed.
                    for name, event in events:
                        time.sleep(turn.get("event_delays", {}).get(name, turn.get("event_delay_seconds", 0)))
                        frame = "event: %s\ndata: %s\n\n" % (name, json.dumps(event))
                        self.wfile.write(frame.encode())
                        self.wfile.flush()
                else:
                    self.wfile.write(payload)
                    self.wfile.flush()
            except (BrokenPipeError, ConnectionResetError):
                pass

    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    server.daemon_threads = True
    print(server.server_address[1], flush=True)
    server.serve_forever()


if __name__ == "__main__":
    main()
