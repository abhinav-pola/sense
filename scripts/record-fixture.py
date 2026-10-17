#!/usr/bin/env python3
"""Records ACP fixtures against the real OpenRouter API through fx.

Each fixture is a prompt run once in a known folder; the app writes every
ACP line to fixtures/acp/<name>.jsonl. Re-record whenever the client changes
what it asks of fx (replay refuses fixtures whose `initialize` differs).
Needs OPENROUTER_API_KEY and a debug build with `--features automation`.

usage: scripts/record-fixture.py [name ...]
"""
import json
import os
import shutil
import socket
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
BINARY = ROOT / "target/debug/sense"
SOCKET = "/tmp/sense-record.sock"
CALC = "def add(a, b):\n    return a - b\n"


def scratch(name):
    path = Path("/tmp/sense-ws") if name == "edit-file" else Path("/tmp/sense-record") / name
    if path.exists():
        shutil.rmtree(path)
    path.mkdir(parents=True)
    (path / "calc.py").write_text(CALC)
    return path


FIXTURES = {
    # An edit through fx's edit tool, with the diff content it reports.
    "edit-file": (lambda: scratch("edit-file"), "calc.py has a bug in add(). Fix it, then reply with one sentence."),
    # A large real repository, where fx emits skill and context notices.
    "notices": (lambda: Path.home() / "openrouter-web", "Reply with just the word ok. Do not use any tools."),
    # A subagent doing the edit: the parent's announcement, the child's own
    # stream (text, tools, diff) under its session id, then idle.
    "subagent": (
        lambda: scratch("subagent"),
        "Use the subagent tool to have a subagent fix the bug in calc.py's add(). "
        "Don't edit anything yourself. When it's done, reply with one sentence.",
    ),
    # Several steps with the agent talking between them: text, tools, text,
    # tools, answer. Stopping it partway must not fold the middle text away
    # as if it were the answer.
    "steps": (
        lambda: scratch("steps"),
        "First read calc.py and say in one sentence what's wrong. Then fix add(). "
        "Then say in one sentence how you'll check it, and run "
        "`python3 -c 'import calc; print(calc.add(2, 3))'`. Finally reply with one sentence.",
    ),
    # A view in the side panel through the app's own show_view tool: a
    # table, a chart and buttons that answer the agent.
    # The second turn is the first button, clicked.
    "view": (
        lambda: scratch("view"),
        [
            "Compare bubble sort, merge sort and quicksort for me in a side panel view: "
            "a table of best/average/worst time and memory, a bar chart of roughly how many "
            "comparisons each makes on 1,000 random items, and buttons asking which one I'd "
            "like an implementation of. Each button's message must ask you to write that "
            "implementation to a new .py file next to calc.py. Keep the chat reply to one sentence.",
            {"click_action": 0},
        ],
    ),
    # One view using every kind of block, drawn by fx's own show_view tool.
    # The model may get a block wrong first: the check's answer is part of
    # the recording, and so is the corrected call.
    "view-gallery": (
        lambda: scratch("view-gallery"),
        "Without reading any files, call show_view to draw one view titled \"Checkout service\" "
        "about an imaginary web checkout service. Put these blocks in it: a graph (direction down) "
        "of its architecture with about 7 nodes, a few labeled edges and one retry loop; a "
        "line_chart of p95 latency in ms for two endpoints over 7 days; an area_chart of requests "
        "per hour for two regions; a pie_chart (donut) of payment methods; a radar_chart comparing "
        "two releases on 5 qualities; a sankey of how 1,000 sessions flow from landing to purchase "
        "or drop-off; a tree of the repository layout; steps for the deploy pipeline with done, "
        "active and pending steps; a diff of a small config change; tags; and key_value. Arrange "
        "them with tabs (Overview, Traffic, Code) and put the diff and key_value in an accordion. "
        "Keep the chat reply to one sentence.",
    ),
    # A turn on the default model, then one on a picked model: the session
    # is switched to it before the second prompt.
    "model": (
        lambda: scratch("model"),
        [
            {"cmd": "load_models", "path": str(ROOT / "fixtures/openrouter-models.json")},
            "Reply with just the word ok. Do not use any tools.",
            {"cmd": "pick_model", "id": "anthropic/claude-haiku-5.5"},
            "Reply with just the word again. Do not use any tools.",
        ],
    ),
    # A screenshot-sized image (2000x1300), which once never showed in its
    # message.
    "image-large": (
        lambda: scratch("image-large"),
        [
            {"cmd": "attach_image", "path": str(ROOT / "fixtures/images/red-wide.png")},
            "What color is the attached image? Reply with one word.",
        ],
    ),
    # A reply with fenced code blocks, each with its own copy button.
    "code-block": (
        lambda: scratch("code-block"),
        "Without using any tools, show a three-line Python hello world in one fenced "
        "code block, then the same in Rust in a second fenced code block. No other text.",
    ),
    # fx asking before it runs a command, then the command once allowed.
    "permission": (
        lambda: scratch("permission"),
        "Run `python3 calc.py` with the shell tool, then reply with one sentence.",
    ),
    # An image prompt: the image goes to the model and it answers about it.
    "image": (lambda: scratch("image"), ("What color is the attached square? Reply with one word.", ROOT / "fixtures/images/red-square.png")),
    # Three turns, kept as fx's own session files (fixtures/fx-home) for
    # history paging, which reads fx's store rather than a conversation.
    "three-turns": (
        lambda: scratch("three-turns"),
        ["Reply with just the word alpha.", "Reply with just the word beta.", "Reply with just the word gamma."],
    ),
}


# Fixtures recorded with fx asking before it runs tools.
ASKING = {"permission"}


def send(command):
    with socket.socket(socket.AF_UNIX) as sock:
        sock.connect(SOCKET)
        sock.sendall(json.dumps(command).encode() + b"\n")
        return json.loads(sock.makefile().readline())


def record(name):
    make_folder, prompt = FIXTURES[name]
    folder = make_folder()
    out = ROOT / "fixtures/acp" / f"{name}.jsonl"
    out.unlink(missing_ok=True)
    if os.path.exists(SOCKET):
        os.remove(SOCKET)
    # A scratch HOME: recording drives the real app, which keeps its
    # library (projects, picked model) and fx its sessions under HOME. The
    # user's own must never see a recording's.
    home = Path("/tmp/sense-record-home") / name
    if home.exists():
        shutil.rmtree(home)
    home.mkdir(parents=True)
    if name in ASKING:
        (home / ".fx").mkdir()
        (home / ".fx/settings.json").write_text(json.dumps({"permission_mode": "ask"}))
    env = dict(
        os.environ,
        HOME=str(home),
        SENSE_RECORD=str(out),
        SENSE_CWD=str(folder),
        SENSE_BACKGROUND="1",
        SENSE_AUTOMATION=SOCKET,
        SENSE_KEYCHAIN_SERVICE="dev.sense.record",
    )
    app = subprocess.Popen([BINARY], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        deadline = time.monotonic() + 10
        while not os.path.exists(SOCKET):
            if time.monotonic() > deadline:
                sys.exit("the app never opened its automation socket")
            time.sleep(0.05)
        if isinstance(prompt, tuple):
            prompt, image = prompt
            send({"cmd": "attach_image", "path": str(image)})
            while not send({"cmd": "state"})["chat"]["attachments"]:
                time.sleep(0.05)
        for text in [prompt] if isinstance(prompt, str) else prompt:
            if isinstance(text, dict) and "cmd" in text:
                send(text)
                # Images attach once decoded, off the main thread.
                while text["cmd"] == "attach_image" and not send({"cmd": "state"})["chat"]["attachments"]:
                    time.sleep(0.05)
                continue
            if isinstance(text, dict):
                # A click on a button the agent drew, where it was drawn.
                send({"cmd": "frame", "timed": True})
                action = send({"cmd": "state"})["panel"]["actions"][text["click_action"]]
                send({"cmd": "click", "x": action["center"][0], "y": action["center"][1]})
            else:
                send({"cmd": "type", "text": text})
                send({"cmd": "submit"})
            time.sleep(0.5)
            deadline = time.monotonic() + 120
            while time.monotonic() < deadline:
                chat = send({"cmd": "state"})["chat"]
                if chat["session"] and not chat["busy"]:
                    break
                # In fixtures that ask (fx's "ask" mode), each ask is shown for
                # a moment, then allowed.
                if any(e.get("pending") for e in chat["entries"] if "permission" in e):
                    time.sleep(1.0)
                    send({"cmd": "answer_permission", "choice": 0})
                time.sleep(0.5)
            else:
                sys.exit(f"{name}: the turn never finished")
        notices = [e["notice"] for e in chat["entries"] if "notice" in e]
        if notices:
            sys.exit(f"{name}: the recorded turn had errors: {notices}")
    finally:
        app.terminate()
        app.wait(timeout=5)
    print(f"recorded {out.relative_to(ROOT)} ({sum(1 for _ in out.open())} lines)")
    if isinstance(prompt, list) and all(isinstance(p, str) for p in prompt):
        # fx's own files for the session, as a store a test can point HOME at.
        store = ROOT / "fixtures/fx-home/.fx/sessions"
        if store.exists():
            shutil.rmtree(store)
        store.mkdir(parents=True)
        shutil.copytree(home / ".fx/sessions" / chat["session"], store / chat["session"])
        print(f"saved fx session {chat['session']} to {store.relative_to(ROOT)}")


if __name__ == "__main__":
    if not os.environ.get("OPENROUTER_API_KEY"):
        sys.exit("OPENROUTER_API_KEY is required: fixtures come from the real API")
    for name in sys.argv[1:] or FIXTURES:
        record(name)
