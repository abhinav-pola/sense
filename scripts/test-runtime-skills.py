#!/usr/bin/env python3
"""Exercise on-disk skills through the built fx ACP server, with a local model.

No credentials or external network. All writes, including global skills, go
under a temporary HOME. Run after building vendor/fx with `zig build`.
"""
import json
import os
from pathlib import Path
import queue
import subprocess
import tempfile
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

ROOT = Path(__file__).resolve().parent.parent
BINARY = ROOT / "vendor/fx/zig-out/bin/fx"


def main():
    with tempfile.TemporaryDirectory(prefix="sense-skills-") as scratch:
        home = Path(scratch).resolve()
        project = home / "project"
        other = home / "other"
        project.mkdir()
        other.mkdir()
        names = ["project-live", "global-live"]
        paths = [project / ".agents/skills" / names[0] / "SKILL.md",
                 home / ".agents/skills" / names[1] / "SKILL.md"]
        contents = [f"---\nname: {name}\ndescription: Runtime skill smoke test\n---\n\n{ name } original marker\n" for name in names]
        locations = {}
        calls = []
        failures = []

        def tool(name, arguments):
            calls.append(name)
            return {"tool_calls": [{"id": f"call_{len(calls)}", "type": "function",
                                    "function": {"name": name, "arguments": json.dumps(arguments)}}]}

        def result(request):
            messages = request["messages"]
            last = messages[-1]
            assert last["role"] == "tool", last
            content = last["content"]
            if isinstance(content, list):
                content = "\n".join(block.get("text", "") for block in content)
            return content

        def discover(request, expected):
            found = json.loads(result(request))["skills"]
            assert {item["name"] for item in found} == set(expected), (found, result(request), [path.read_text() for path in paths])
            locations.update({item["name"]: item["location"] for item in found})

        def advance(request):
            step = len(calls)
            if step < 2:
                if step:
                    assert paths[0].read_text() == contents[0], result(request)
                return tool("write_file", {"path": str(paths[step]), "content": contents[step]})
            if step == 2:
                assert paths[1].read_text() == contents[1], result(request)
                return tool("capability_search", {"query": "live"})
            if step == 3:
                discover(request, names)
                return tool("skill", {"location": locations[names[0]]})
            if step == 4:
                assert f"{names[0]} original marker" in result(request), result(request)
                return tool("skill", {"location": locations[names[1]]})
            if step == 5:
                assert f"{names[1]} original marker" in result(request), result(request)
                return tool("edit_file", {"path": str(paths[0]), "old_string": "original marker", "new_string": "updated marker"})
            if step == 6:
                assert "updated marker" in paths[0].read_text(), result(request)
                return tool("skill", {"location": locations[names[0]]})
            if step == 7:
                assert f"{names[0]} updated marker" in result(request), result(request)
                calls.append("first-turn-finished")
                return {"content": "Skills verified."}
            if step == 8:
                return tool("capability_search", {"query": "live"})
            if step == 9:
                discover(request, [names[1]])
                return tool("skill", {"location": locations[names[1]]})
            assert step == 10, step
            assert f"{names[1]} original marker" in result(request), result(request)
            calls.append("second-turn-finished")
            return {"content": "Global skill verified in another project."}

        class Model(BaseHTTPRequestHandler):
            def log_message(self, *_):
                pass

            def do_POST(self):
                request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
                try:
                    message = advance(request)
                except Exception as error:
                    failures.append(error)
                    message = {"content": "Test failed."}
                # fx requests streaming OpenAI chat completions.
                self.send_response(200)
                self.send_header("Content-Type", "text/event-stream")
                self.end_headers()
                chunk = {"id": "smoke", "object": "chat.completion.chunk", "model": "test-model",
                         "choices": [{"index": 0, "delta": {"role": "assistant", **message}, "finish_reason": None}]}
                if "tool_calls" in message:
                    chunk["choices"][0]["delta"]["tool_calls"][0]["index"] = 0
                finish = {"id": "smoke", "object": "chat.completion.chunk", "choices": [
                    {"index": 0, "delta": {}, "finish_reason": "tool_calls" if "tool_calls" in message else "stop"}]}
                self.wfile.write(("data: " + json.dumps(chunk) + "\n\ndata: " + json.dumps(finish) + "\n\ndata: [DONE]\n\n").encode())

        server = ThreadingHTTPServer(("127.0.0.1", 0), Model)
        worker = threading.Thread(target=server.serve_forever)
        worker.start()
        env = {key: value for key, value in os.environ.items() if not key.startswith("FX_")}
        env.update(HOME=str(home), FX_SOUND="0", FX_PROVIDER="smoke", FX_MODEL="test-model",
                   FX_PERMISSION_MODE="full-access", FX_CONFIGURED_PROVIDERS=json.dumps({"smoke": {
                       "protocol": "openai-chat-completions", "base_url": f"http://127.0.0.1:{server.server_port}/v1",
                       "auth": {"type": "none"}, "model_metadata": {"test-model": {
                           "context_window": 1000000, "max_output_tokens": 4096, "supports_tool_use": True}}}}))
        with tempfile.TemporaryFile(mode="w+") as stderr:
            process = subprocess.Popen([BINARY, "acp"], cwd=project, env=env, stdin=subprocess.PIPE,
                                       stdout=subprocess.PIPE, stderr=stderr, text=True)
            replies = queue.Queue()

            def receive():
                for line in process.stdout:
                    replies.put(json.loads(line))

            reader = threading.Thread(target=receive)
            reader.start()
            request_id = 0

            def rpc(method, params):
                nonlocal request_id
                request_id += 1
                process.stdin.write(json.dumps({"jsonrpc": "2.0", "id": request_id, "method": method, "params": params}) + "\n")
                process.stdin.flush()
                while True:
                    response = replies.get(timeout=30)
                    if response.get("id") == request_id:
                        assert "error" not in response, response
                        return response["result"]

            try:
                rpc("initialize", {"protocolVersion": 1, "clientCapabilities": {}})
                first = rpc("session/new", {"cwd": str(project), "mcpServers": []})["sessionId"]
                # The process and session exist before the agent writes either skill.
                rpc("session/prompt", {"sessionId": first, "prompt": [{"type": "text", "text": "Exercise runtime skills."}]})
                assert not failures, failures
                second = rpc("session/new", {"cwd": str(other), "mcpServers": []})["sessionId"]
                rpc("session/prompt", {"sessionId": second, "prompt": [{"type": "text", "text": "Check global skills in another project."}]})
                assert not failures, failures
                assert calls[-1] == "second-turn-finished", calls
                assert "updated marker" in paths[0].read_text()
                print("ok: agent writes project/global skills, discovers and loads both, reads an edit without restart, and shares only the global skill with another project")
            finally:
                process.stdin.close()
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.terminate()
                    process.wait(timeout=5)
                reader.join()
                server.shutdown()
                server.server_close()
                worker.join()
                stderr.seek(0)
                errors = stderr.read()
                assert process.returncode == 0, (process.returncode, errors)
                assert not errors.strip(), errors


if __name__ == "__main__":
    main()
