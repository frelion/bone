"""Script work calls and answer summary calls separately, on loopback only."""
import argparse
import importlib.util
import json
from pathlib import Path
import re
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import threading
import time


ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("responses_fixture", ROOT / "tests/scripted_responses.py")
wire = importlib.util.module_from_spec(spec)
spec.loader.exec_module(wire)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--script", type=Path, required=True)
    parser.add_argument("--requests", type=Path, required=True)
    args = parser.parse_args()
    script = json.loads(args.script.read_text())
    turns = script.get("turns", [])
    state = {"used": set(), "sequence": 0}
    lock = threading.Lock()

    class Handler(BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.1"

        def log_message(self, *_args):
            pass

        def do_POST(self):
            size = int(self.headers.get("Content-Length", "0"))
            body = json.loads(self.rfile.read(size))
            summary = body.get("instructions", "").startswith("Summarize this job")
            with lock:
                if script.get("reload_script"):
                    current = json.loads(args.script.read_text())
                    turns[:] = current.get("turns", [])
                sequence = state["sequence"]
                state["sequence"] += 1
                with args.requests.open("a", encoding="utf-8") as stream:
                    stream.write(json.dumps({"body": body, "summary": summary}) + "\n")
                if summary:
                    turn = {"text": script.get("summary", "Preserve completed work, pending work, exact IDs and constraints."),
                            "contains": script.get("summary_contains", [])}
                else:
                    turn = None
                    for index, candidate in enumerate(turns):
                        if index in state["used"]:
                            continue
                        selector = candidate.get("match_last_user_contains")
                        if selector is not None and selector not in wire.last_user_text(body):
                            continue
                        title = candidate.get("match_job_title")
                        match = re.search(r"currently working inside job \S+ \(([^\n]+)\)", body.get("instructions", ""))
                        if title is not None and (match is None or match.group(1) != title):
                            continue
                        turn = candidate
                        state["used"].add(index)
                        break
            if turn is None:
                self.send_error(500, "fixture work script exhausted")
                return
            encoded = json.dumps(body, ensure_ascii=False)
            if any(term not in encoded for term in turn.get("contains", [])):
                self.send_error(422, "fixture request requirement missing")
                return
            time.sleep(turn.get("delay_seconds", 0))
            output = turn.get("output", [{"type": "message", "role": "assistant", "content": [
                {"type": "output_text", "text": turn.get("text", "done"), "annotations": []}]}])
            output = wire.expand_output(output, body)
            events = list(wire.response_events(output, sequence, {"input_tokens": 10, "output_tokens": 5, "total_tokens": 15}))
            for index, (_, event) in enumerate(events):
                event["sequence_number"] = index
            if body.get("stream"):
                payload = "".join("event: %s\ndata: %s\n\n" % (name, json.dumps(event)) for name, event in events).encode()
            else:
                payload = json.dumps(events[-1][1]["response"]).encode()
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream" if body.get("stream") else "application/json")
            self.send_header("Content-Length", str(len(payload)))
            self.end_headers()
            try:
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
