#!/usr/bin/env python3
"""End-to-end flows against the real app, offline.

Each flow launches a debug build with `--features automation` in background
mode, replays a fixture recorded from the real API in place of fx, drives the
UI over the automation socket and checks the resulting state. Screenshots land
in target/e2e/ for review.

usage: scripts/e2e.py [flow ...]
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
OUT = ROOT / "target/e2e"
SOCKET = "/tmp/sense-e2e.sock"


CALC_BEFORE = "def add(a, b):\n    return a - b\n"


def fresh_workspace(name):
    """A scratch folder holding the file the recorded session edits."""
    path = Path("/tmp/sense-e2e") / name
    if path.exists():
        shutil.rmtree(path)
    path.mkdir(parents=True)
    (path / "calc.py").write_text(CALC_BEFORE)
    return str(path)


class App:
    def __init__(self, fixture, cwd, pace="0.25", env=None):
        if os.path.exists(SOCKET):
            os.remove(SOCKET)
        # A scratch HOME unless the flow brings its own: the app keeps its
        # library and worktrees under HOME, and tests must never touch the
        # user's.
        home = Path("/tmp/sense-e2e/home") / (fixture or "live")
        if not (env or {}).get("HOME"):
            if home.exists():
                shutil.rmtree(home)
            home.mkdir(parents=True)
        env = dict(
            {**os.environ, "HOME": str(home), **(env or {})},
            SENSE_BACKGROUND="1",
            SENSE_AUTOMATION=SOCKET,
            **({"SENSE_CWD": cwd} if cwd else {}),
            **({"SENSE_REPLAY": str(ROOT / "fixtures/acp" / fixture)} if fixture else {}),
            SENSE_REPLAY_PACE=pace,
            # Never the user's real credential: sign-out flows delete it.
            SENSE_KEYCHAIN_SERVICE="dev.sense.e2e",
        )
        self.log = open(OUT / f"{fixture or 'live'}.log", "w")
        self.process = subprocess.Popen([BINARY], env=env, stdout=self.log, stderr=self.log)
        wait_for(lambda: os.path.exists(SOCKET), "automation socket")

    def send(self, **command):
        with socket.socket(socket.AF_UNIX) as sock:
            sock.connect(SOCKET)
            sock.sendall(json.dumps(command).encode() + b"\n")
            reply = json.loads(sock.makefile().readline())
        if isinstance(reply, dict) and "error" in reply:
            raise AssertionError(f"{command}: {reply['error']}")
        return reply

    def state(self):
        return self.send(cmd="state")

    def close(self):
        self.process.terminate()
        self.process.wait(timeout=5)
        self.log.close()


def wait_for(predicate, what, timeout=20.0):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            value = predicate()
        except (OSError, ConnectionError):
            value = None
        if value:
            return value
        time.sleep(0.05)
    raise AssertionError(f"timed out waiting for {what}")


def flow_edit_file():
    """Prompt, tool calls stream in, the diff panel opens with the edit."""
    workspace = fresh_workspace("edit-file")
    app = App("edit-file.jsonl", workspace)
    try:
        state = app.state()
        assert state["chat"]["entries"] == [], state
        assert not state["panel_open"]
        app.send(cmd="screenshot", path=str(OUT / "edit-file-empty.png"))

        app.send(cmd="type", text="calc.py has a bug in add(). Fix it, then reply with one sentence.")
        app.send(cmd="submit")
        state = wait_for(
            lambda: (s := app.state())["chat"]["session"] and not s["chat"]["busy"] and s,
            "turn to end",
        )
        kinds = [next(iter(entry)) for entry in state["chat"]["entries"]]
        assert kinds[0] == "user" and kinds[-1] == "agent", kinds
        tools = [e for e in state["chat"]["entries"] if "tool" in e]
        assert tools and all(t["status"] == "Completed" for t in tools), tools
        assert "a + b" in state["chat"]["entries"][-1]["agent"]
        # The working steps fold into one row; the answer stays out of it.
        folds = state["chat"]["folds"]
        last = len(state["chat"]["entries"]) - 1
        assert len(folds) == 1 and not folds[0]["expanded"], folds
        assert folds[0]["first"] == 1 and folds[0]["last"] < last, (folds, last)
        assert folds[0]["steps"] >= 3, folds
        app.send(cmd="toggle_fold", index=0)
        assert app.state()["chat"]["folds"][0]["expanded"]
        app.send(cmd="toggle_fold", index=0)
        assert not [e for e in state["chat"]["entries"] if "notice" in e], state["chat"]["entries"]
        state = wait_for(lambda: (s := app.state())["panel_open"] and s, "diff panel to open")
        assert "+    return a + b" in state["chat"]["patch"], state["chat"]["patch"]
        assert Path(workspace, "calc.py").read_text() != CALC_BEFORE
        assert state["chats"][0].startswith("calc.py has a bug")
        app.send(cmd="screenshot", path=str(OUT / "edit-file-done.png"))

        app.send(cmd="new_chat")
        state = app.state()
        assert state["chat"]["entries"] == [] and not state["panel_open"], state
        app.send(cmd="select_chat", index=1)
        assert app.state()["panel_open"], "panel should come back with the chat that has edits"

        snapshot = Path(app.state()["chat"]["snapshot_dir"])
        assert snapshot.is_dir(), snapshot
        app.send(cmd="delete_chat", index=1)
        state = app.state()
        assert len(state["chats"]) == 1 and state["chat"]["entries"] == [], state
        wait_for(lambda: not snapshot.exists(), "snapshot to be removed")
    finally:
        app.close()


# Window chrome no app content may paint over: the traffic lights sit at
# (9, 9) in the transparent title bar.
TRAFFIC_LIGHTS = (0, 0, 80, 32)
SIDEBAR_WIDTH = 260
WINDOW = (1280, 820)
# Elements that open an overlay when clicked, as (name, x, y) in logical px.
# Clicking anything else would have side effects, so triggers are listed.
OVERLAY_TRIGGERS = [("account menu", 100, 795)]


def sidebar_spill(app):
    """Sidebar content cut off by the sidebar's edge, as [x, y, width,
    visible width]. Hairlines (the sidebar's own 1px border) don't count."""
    # Cut off where the sidebar ends, not inside it: a text field clipping
    # its own scrolled content is fine.
    width = app.state()["layout"]["sidebar"]
    return [q for q in app.send(cmd="clipped")["clipped"]
            if q[0] < width and q[3] > 2 and abs(q[0] + q[3] - width) <= 2]


def flow_text_size():
    """cmd + and cmd − change the size of all text, one point a step within
    10–28, and cmd 0 restores 16. Each change shows the new size in the
    middle of the window for about a second; the size is kept for next time."""
    home = Path("/tmp/sense-e2e/home/text-size")
    if home.exists():
        shutil.rmtree(home)
    home.mkdir(parents=True)
    workspace = fresh_workspace("text-size")
    app = App("edit-file.jsonl", workspace, env={"HOME": str(home)})
    try:
        center = (WINDOW[0] / 2, WINDOW[1] / 2)

        def badge_drawn():
            return [q for q in app.send(cmd="quads")["quads"]
                    if abs(q[0] + q[2] / 2 - center[0]) <= 2 and abs(q[1] + q[3] / 2 - center[1]) <= 2
                    and q[2] < 200]

        def row_height():
            # The thread row's highlight: rem-sized, so it grows with the text.
            for y in range(40, 200, 2):
                rows = [r for r in app.send(cmd="probe", x=100, y=y)["appeared"] if contains(r, 100, y) and r[2] > 150]
                if rows:
                    return rows[0][3]

        assert app.state()["text_size"] == 16
        before = row_height()
        assert not badge_drawn()
        app.send(cmd="key", keys="cmd-=")
        assert app.state()["text_size"] == 17
        time.sleep(0.4)
        assert badge_drawn(), "the new size shows in the middle of the window"
        app.send(cmd="screenshot", path=str(OUT / "text-size-badge.png"))
        wait_for(lambda: not badge_drawn() and app.state()["text_size_badge"] is None, "the size to fade away", timeout=3)
        assert row_height() > before, (row_height(), before)

        app.send(cmd="key", keys="cmd--")
        app.send(cmd="key", keys="cmd--")
        assert app.state()["text_size"] == 15
        app.send(cmd="key", keys="cmd-0")
        assert app.state()["text_size"] == 16
        for _ in range(20):
            app.send(cmd="key", keys="cmd-=")
        assert app.state()["text_size"] == 28, "capped at 28"
        library = home / "Library/Application Support/Sense/library.json"
        wait_for(lambda: library.exists() and json.loads(library.read_text()).get("text_size") == 28, "the size to be saved")
    finally:
        app.close()
    app = App("edit-file.jsonl", workspace, env={"HOME": str(home)})
    try:
        assert app.state()["text_size"] == 28, "kept for next time"
        assert app.state()["text_size_badge"] is None, "no badge for the size it opens with"
    finally:
        app.close()


def flow_appearance():
    """Light and dark: chosen in the account menu, or following the system.
    Switching keeps the text size and the glass sidebar, and the choice is
    kept for next time."""
    home = Path("/tmp/sense-e2e/home/appearance")
    if home.exists():
        shutil.rmtree(home)
    home.mkdir(parents=True)
    workspace = fresh_workspace("appearance")
    app = App("edit-file.jsonl", workspace, env={"HOME": str(home)})
    try:
        app.send(cmd="appearance", choice="dark")
        app.send(cmd="key", keys="cmd-= cmd-=")
        assert app.state()["dark"] and app.state()["text_size"] == 18
        app.send(cmd="screenshot", path=str(OUT / "appearance-dark.png"))
        app.send(cmd="appearance", choice="light")
        state = app.state()
        assert not state["dark"] and state["text_size"] == 18, (state["dark"], state["text_size"])
        app.send(cmd="screenshot", path=str(OUT / "appearance-light.png"))
        library = home / "Library/Application Support/Sense/library.json"
        wait_for(lambda: library.exists() and json.loads(library.read_text()).get("appearance") == "light", "the choice to be saved")
    finally:
        app.close()
    app = App("edit-file.jsonl", workspace, env={"HOME": str(home)})
    try:
        assert not app.state()["dark"], "light is kept for next time"
    finally:
        app.close()


def recorded_views(fixture):
    """The views the agent drew in a recording, straight from its show_view
    calls: what the panel should show, read independently of the app."""
    views = []
    for line in (ROOT / "fixtures/acp" / fixture).open():
        message = json.loads(json.loads(line)["line"])
        update = message.get("params", {}).get("update", {})
        # fx marks its own show_view calls; it reports nothing else as one.
        call = update.get("_meta", {}).get("fx", {}).get("toolCall", {})
        if update.get("sessionUpdate") == "tool_call" and call.get("view") is True:
            raw = update["rawInput"]
            # Models sometimes wrap it the way fx's own tools take input.
            if set(raw) == {"request"}:
                raw = raw["request"] if isinstance(raw["request"], dict) else json.loads(raw["request"])
            views.append(raw)
    return views


def block_kind(block):
    """A block as the panel's snapshot names it: `bar_chart` is BarChart."""
    if block["type"] == "columns":
        return {"columns": [[block_kind(b) for b in column] for column in block["columns"]]}
    return "".join(part.capitalize() for part in block["type"].split("_"))


def settled_screenshot(app, name):
    """A screenshot once charts have drawn in and tab underlines slid over:
    the offscreen render shows the frame it is asked for, mid-animation or
    not."""
    # A background window draws only when asked, and springs (a tab's
    # underline sliding over) only move on drawn frames: draw them as a
    # window on screen would while the animations run.
    for _ in range(40):
        app.send(cmd="frame")
        time.sleep(0.03)
    app.send(cmd="screenshot", path=str(OUT / name))


def flow_view_gallery():
    """One view with every kind of block, drawn by fx's own show_view tool:
    fx asks the app to check it, the app answers, and the panel draws it
    from the tool call. Each pane of its tabs shows, the accordion opens and
    closes, and it reads in dark and light."""
    recorded = recorded_views("view-gallery.jsonl")
    assert recorded, "the recording drew no view"
    view = recorded[-1]
    used = set()

    def collect(blocks):
        for block in blocks:
            used.add(block["type"])
            for key in ("columns",):
                for column in block.get(key, []):
                    collect(column)
            for pane in block.get("tabs", []):
                collect(pane["blocks"])
            for section in block.get("sections", []):
                collect(section["blocks"])

    collect(view["blocks"])
    wanted = {"graph", "line_chart", "area_chart", "pie_chart", "radar_chart", "sankey", "tree",
              "steps", "diff", "tags", "key_value", "tabs", "accordion"}
    assert wanted <= used, f"the recording's view lacks {wanted - used}"
    app = App("view-gallery.jsonl", fresh_workspace("view-gallery"))
    try:
        app.send(cmd="appearance", choice="dark")
        app.send(cmd="type", text="Draw the checkout service view.")
        app.send(cmd="submit")
        state = wait_for(lambda: (s := app.state())["chat"]["session"] and not s["chat"]["busy"] and s, "the turn")
        assert not [e for e in state["chat"]["entries"] if "notice" in e], state["chat"]["entries"]
        panel = state["panel"]
        assert state["panel_open"] and panel["tab"] == view["id"], panel
        assert panel["views"] == [{"id": view["id"], "title": view["title"],
                                   "blocks": [block_kind(b) for b in view["blocks"]]}], panel["views"]
        tabs = [c for c in panel["controls"] if c["kind"] == "tabs"]
        assert tabs, panel["controls"]
        for index, label in enumerate(tabs[0]["labels"]):
            app.send(cmd="pick_pane", block=tabs[0]["block"], index=index)
            assert app.state()["panel"]["controls"][0]["picked"] == index
            app.send(cmd="scroll_view", y=0)
            settled_screenshot(app, f"gallery-{label.lower()}-dark.png")
            # And the rest of the pane, a screen further down.
            app.send(cmd="scroll_view", y=700)
            settled_screenshot(app, f"gallery-{label.lower()}-more-dark.png")
            app.send(cmd="scroll_view", y=0)
            # Accordions in this pane: every section open, then shot again.
            pane = view["blocks"][0]["tabs"][index]["blocks"] if view["blocks"][0]["type"] == "tabs" else []
            if not any(b["type"] == "accordion" for b in pane):
                continue
            for control in app.state()["panel"]["controls"]:
                if control["kind"] != "accordion":
                    continue
                closed = [i for i in range(len(control["labels"])) if i not in control["open"]]
                for section in closed:
                    app.send(cmd="toggle_section", block=control["block"], index=section)
                after = [c for c in app.state()["panel"]["controls"] if c["block"] == control["block"]][0]
                assert sorted(after["open"]) == list(range(len(control["labels"]))), after
                settled_screenshot(app, f"gallery-{label.lower()}-open-dark.png")
        app.send(cmd="appearance", choice="light")
        app.send(cmd="pick_pane", block=tabs[0]["block"], index=0)
        settled_screenshot(app, "gallery-overview-light.png")
        app.send(cmd="pick_pane", block=tabs[0]["block"], index=1)
        settled_screenshot(app, "gallery-traffic-light.png")
    finally:
        app.close()


def flow_view():
    """The agent draws a view in the side panel with its show_view tool: it
    opens in front, block for block as the agent sent it. Clicking one of
    its buttons answers the agent with that button's message, and the
    agent's next turn goes ahead; the changes it makes get their own tab."""
    recorded = recorded_views("view.jsonl")
    assert len(recorded) == 1, recorded
    view = recorded[0]
    app = App("view.jsonl", fresh_workspace("view"))
    try:
        app.send(cmd="type", text="Compare bubble sort, merge sort and quicksort in a side panel view.")
        app.send(cmd="submit")
        state = wait_for(lambda: (s := app.state())["chat"]["session"] and not s["chat"]["busy"] and s, "the first turn")
        panel = state["panel"]
        assert state["panel_open"] and panel["tab"] == view["id"], panel
        assert panel["views"] == [{"id": view["id"], "title": view["title"],
                                   "blocks": [block_kind(b) for b in view["blocks"]]}], panel["views"]
        app.send(cmd="screenshot", path=str(OUT / "view.png"))

        buttons = [b for block in view["blocks"] if block["type"] == "actions" for b in block["buttons"]]
        drawn = app.state()["panel"]["actions"]
        assert [a["label"] for a in drawn] == [b["label"] for b in buttons], drawn
        # A real click where the button is drawn, through the window's own
        # hit testing.
        app.send(cmd="click", x=drawn[0]["center"][0], y=drawn[0]["center"][1])
        state = wait_for(
            lambda: (s := app.state()) and [e["user"] for e in s["chat"]["entries"] if "user" in e][-1:] == [buttons[0]["message"]] and s,
            "the button's message to go to the agent",
        )
        state = wait_for(lambda: (s := app.state()) and not s["chat"]["busy"] and s, "the second turn")
        assert [e["user"] for e in state["chat"]["entries"] if "user" in e][-1] == buttons[0]["message"]
        assert not [e for e in state["chat"]["entries"] if "notice" in e], state["chat"]["entries"]
        # The view stays; the changes from the second turn sit beside it.
        panel = wait_for(lambda: (p := app.state()["panel"])["files"] > 0 and p, "the changes tab")
        assert [v["id"] for v in panel["views"]] == [view["id"]], panel
        app.send(cmd="select_tab", tab="changes")
        assert app.state()["panel"]["tab"] == "changes"
        app.send(cmd="screenshot", path=str(OUT / "view-changes.png"))
    finally:
        app.close()


def flow_model_picker():
    """Picking a model in the composer switches that chat to it for its next
    turn: fx is told before the prompt goes (the replay checks the switch
    names the recording's model, or stops). The default, set in settings,
    is where new threads start, and is kept for next time."""
    home = Path("/tmp/sense-e2e/home/model")
    if home.exists():
        shutil.rmtree(home)
    home.mkdir(parents=True)
    workspace = fresh_workspace("model")
    app = App("model.jsonl", workspace, env={"HOME": str(home)})
    try:
        # The real list, saved: the same parsing a refresh does.
        loaded = app.send(cmd="load_models", path=str(ROOT / "fixtures/openrouter-models.json"))
        assert loaded["models"] >= 5, loaded
        assert app.state()["chat"]["model"] == "anthropic/claude-sonnet-5.5"
        app.send(cmd="type", text="Reply with just the word ok.")
        app.send(cmd="submit")
        wait_for(lambda: (s := app.state())["chat"]["session"] and not s["chat"]["busy"], "the first turn")

        # Picked as a person would: open the picker, type, Enter.
        app.send(cmd="open_model_picker")
        app.send(cmd="frame", timed=True)
        app.send(cmd="key", keys="h a i k u")
        picker = app.state()["chat"]["picker"]
        assert picker["highlighted"] == "anthropic/claude-haiku-5.5", picker
        def in_order(query, text):
            rest = iter(text.lower())
            return all(c in rest for c in query)
        listed = json.loads((ROOT / "fixtures/openrouter-models.json").read_text())["data"]
        names = {m["id"]: m["name"] for m in listed}
        assert picker["matches"] and all(in_order("haiku", names[m] + m) for m in picker["matches"]), picker
        app.send(cmd="key", keys="enter")
        state = app.state()
        assert state["chat"]["model"] == "anthropic/claude-haiku-5.5" and state["chat"]["picker"] is None, state["chat"]["picker"]
        app.send(cmd="type", text="Reply with just the word again.")
        app.send(cmd="submit")
        state = wait_for(
            lambda: (s := app.state()) and not s["chat"]["busy"] and len([e for e in s["chat"]["entries"] if "agent" in e]) == 2 and s,
            "the second turn, on the picked model",
        )
        assert not [e for e in state["chat"]["entries"] if "notice" in e], state["chat"]["entries"]
        assert state["chat"]["entries"][-1]["agent"].strip().lower().startswith("again"), state["chat"]["entries"][-1]

        # A pick in the composer is that thread's alone.
        app.send(cmd="new_chat")
        assert app.state()["chat"]["model"] == "anthropic/claude-sonnet-5.5", "the default is unchanged"

        # The default, set in settings: the empty thread follows it, the
        # one already talking keeps its own, new threads start on it.
        # Set from the account menu: its first item opens the same search.
        opened = app.send(cmd="probe", x=100, y=795, click=True)["appeared"]
        menu = max(opened, key=lambda r: r[2] * r[3])
        app.send(cmd="click", x=menu[0] + 40, y=menu[1] + 18)
        app.send(cmd="frame", timed=True)
        app.send(cmd="key", keys="g p t space 6 . 1 space s o l")
        app.send(cmd="frame", timed=True)
        app.send(cmd="key", keys="enter")
        assert app.state()["chat"]["model"] == "openai/gpt-6.1-sol", app.state()["chat"]["model"]
        app.send(cmd="select_chat", index=1)
        assert app.state()["chat"]["model"] == "anthropic/claude-haiku-5.5"
        app.send(cmd="new_chat")
        assert app.state()["chat"]["model"] == "openai/gpt-6.1-sol"
        library = wait_for(
            lambda: (l := json.loads((home / "Library/Application Support/Sense/library.json").read_text()))
            .get("model") == "openai/gpt-6.1-sol" and l,
            "the default to be saved",
        )
        assert len(library["models"]) == loaded["models"]
    finally:
        app.close()
    # Kept for next time.
    app = App("model.jsonl", fresh_workspace("model"), env={"HOME": str(home)})
    try:
        assert app.state()["chat"]["model"] == "openai/gpt-6.1-sol"
    finally:
        app.close()


def flow_code_copy():
    """Each fenced code block in a reply has its own copy button in its top
    corner, as well as the message's own. (Clicking would write to the
    system clipboard, which tests leave alone: the button is checked by
    hovering it.)"""
    app = App("code-block.jsonl", fresh_workspace("code-copy"))
    try:
        app.send(cmd="type", text="Show hello world in Python and Rust.")
        app.send(cmd="submit")
        state = wait_for(lambda: (s := app.state())["chat"]["session"] and not s["chat"]["busy"] and s, "the reply")
        fences = state["chat"]["entries"][-1]["agent"].count("```") // 2
        assert fences == 2, state["chat"]["entries"][-1]
        # Code blocks: wide panels in the transcript, above the composer.
        composer_top = WINDOW[1] - 160
        blocks = [q for q in app.send(cmd="quads")["quads"] if q[2] > 500 and q[1] + q[3] < composer_top and q[3] > 40]
        assert len(blocks) == fences, blocks
        for block in blocks:
            x, y = block[0] + block[2] - 18, block[1] + 18
            buttons = [r for r in app.send(cmd="probe", x=x, y=y)["appeared"] if contains(r, x, y)]
            assert buttons and all(contains(block, r[0], r[1]) for r in buttons), (block, buttons)
    finally:
        app.close()


# Every formula in math.jsonl, as (LaTeX, display style). The thought's is
# last; one-line `$$…$$` spans are display style like the fenced block.
MATH = {
    ("T", False), ("f = 1/T", False), (r"e^{i\pi} + 1 = 0", False),
    (r"\text{1 Hz} = \frac{1}{\text{second}} = \text{1 s}^{-1}", True),
    (r"R(\theta) = \begin{pmatrix} \cos\theta & -\sin\theta \\ \sin\theta & \cos\theta \end{pmatrix}", True),
    (r"\sum_{n=1}^{\infty} \frac{1}{n^2} = \frac{\pi^2}{6}", True),
    (r"\omega = 2\pi f", False),
}


def flow_math():
    """LaTeX in replies and thoughts is drawn as images: `$…$` inline, `$$…$$`
    display. Prose dollars ("$5 and $10") stay text, and a `$$` block is only
    typeset once its closing fence arrives. Showing the chat again reuses the
    images; a new appearance draws every formula in the new ink."""
    app = App("math.jsonl", fresh_workspace("math"))
    try:
        def drawn():
            report = app.send(cmd="math")
            return {(d["latex"].strip(), d["display"]) for d in report["drawn"]}, report

        app.send(cmd="type", text="What is a hertz?")
        app.send(cmd="submit")
        wait_for(lambda: (e := app.state()["chat"]["entries"]) and e[-1].get("agent", "").endswith(r"\frac{1}{n^2} = "),
                 "the reply to stop inside an open $$ block")
        assert not any(r"\sum" in latex for latex, _ in drawn()[0]), "an unclosed $$ block was typeset"

        state = wait_for(lambda: (s := app.state())["chat"]["session"] and not s["chat"]["busy"] and s, "the reply")
        # The last text is parsed in the background: wait for it to be drawn.
        wait_for(lambda: drawn()[0] == MATH, "every formula drawn")
        formulas, report = drawn()
        assert formulas == MATH, ("missing", MATH - formulas, "extra", formulas - MATH)
        app.send(cmd="screenshot", path=str(OUT / "math.png"))

        typeset = report["rasterized"]
        app.send(cmd="new_chat")
        app.send(cmd="select_chat", index=1)
        assert app.state()["chat"]["entries"], "back on the chat with the formulas"
        assert app.send(cmd="math")["rasterized"] == typeset, "showing the chat again typeset its formulas again"

        app.send(cmd="appearance", choice="light" if state["dark"] else "dark")
        inks = {}
        for d in app.send(cmd="math")["drawn"]:
            inks.setdefault(d["latex"].strip(), set()).add(d["color"])
        # The reply's only: the thought is folded away once the turn ends.
        stale = [latex for latex, _ in MATH if latex != r"\omega = 2\pi f" and len(inks.get(latex, ())) < 2]
        assert not stale, f"not drawn in the new appearance's ink: {stale}"
    finally:
        app.close()


def flow_queue():
    """Enter while the agent works queues the message (one at most) and it
    goes when the turn ends. Option+Up brings it back to the composer, with
    its images and ahead of anything typed since. A stopped turn hands the
    queued message back instead of sending it."""
    app = App("three-turns.jsonl", fresh_workspace("queue"), pace="3")
    try:
        def users():
            return [e["user"] for e in app.state()["chat"]["entries"] if "user" in e]

        app.send(cmd="type", text="Reply with just the word alpha.")
        app.send(cmd="submit")
        wait_for(lambda: app.state()["chat"]["busy"], "the first turn to start")
        app.send(cmd="type", text="Reply with just the word beta.")
        app.send(cmd="submit")
        chat = app.state()["chat"]
        assert chat["queued"] == {"text": "Reply with just the word beta.", "images": 0}, chat["queued"]
        assert chat["draft"] == "" and users() == ["Reply with just the word alpha."]
        # One queued at most: another Enter leaves the draft where it is.
        app.send(cmd="type", text="extra")
        app.send(cmd="submit")
        chat = app.state()["chat"]
        assert chat["queued"]["text"] == "Reply with just the word beta." and chat["draft"] == "extra", chat
        app.send(cmd="screenshot", path=str(OUT / "queue.png"))

        # The first turn ends; the queued message is the next one sent.
        wait_for(lambda: len(users()) == 2, "the queued message to be sent")
        chat = app.state()["chat"]
        assert users()[1] == "Reply with just the word beta." and chat["queued"] is None, chat
        assert chat["draft"] == "extra", "the draft typed meanwhile stays"
        wait_for(lambda: app.state()["chat"]["busy"], "the second turn")

        # Queued with an image, then taken back with Option+Up.
        app.send(cmd="type", text="")
        app.send(cmd="paste_image", path=str(ROOT / "fixtures/images/red-square.png"))
        wait_for(lambda: app.state()["chat"]["attachments"] == ["image/png"], "the image")
        app.send(cmd="type", text="what about this")
        app.send(cmd="submit")
        chat = app.state()["chat"]
        assert chat["queued"] == {"text": "what about this", "images": 1} and chat["attachments"] == [], chat
        app.send(cmd="type", text="and also")
        app.send(cmd="key", keys="alt-up")
        chat = app.state()["chat"]
        assert chat["queued"] is None, chat["queued"]
        assert chat["draft"] == "what about this\nand also", chat["draft"]
        assert chat["attachments"] == ["image/png"], "its image comes back with it"

        # Queued again, then Stop: back to the composer, not sent.
        app.send(cmd="submit")
        assert app.state()["chat"]["queued"]["images"] == 1
        app.send(cmd="stop")
        wait_for(lambda: not app.state()["chat"]["busy"], "the turn to stop")
        chat = wait_for(lambda: (c := app.state()["chat"])["queued"] is None and c, "the queued message to come back")
        assert chat["draft"] == "what about this\nand also" and chat["attachments"] == ["image/png"], chat
        assert len(users()) == 2, users()
    finally:
        app.close()


def flow_thread_status():
    """Hovering a thread shows what it's doing in the sidebar's top corner,
    in the states fx reports to terminals (vercel-labs/fx#1181): asking for
    permission, working, done. Moving away hides it."""
    app = App("permission.jsonl", fresh_workspace("status"), pace="1")
    try:
        def hovered_status(y):
            app.send(cmd="probe", x=100, y=y)
            return app.state()["sidebar_status"]

        # The thread's row: the first height where hovering shows a status.
        row = next((y for y in range(40, 300, 4) if hovered_status(y)), None)
        assert row is not None, "no sidebar row shows a status"
        assert hovered_status(row) == "Idle"

        app.send(cmd="type", text="Run python3 calc.py with the shell tool.")
        app.send(cmd="submit")
        wait_for(lambda: any(e.get("pending") for e in app.state()["chat"]["entries"] if "permission" in e), "fx to ask")
        status = hovered_status(row)
        assert status == "Needs permission · shell.run python3 calc.py", status
        app.send(cmd="screenshot", path=str(OUT / "status-permission.png"))

        app.send(cmd="answer_permission", choice=0)
        status = hovered_status(row)
        assert status.startswith("Working · ") and status.endswith("s"), status
        wait_for(lambda: not app.state()["chat"]["busy"], "the turn to end")
        status = hovered_status(row)
        assert status.startswith("Done · ") and status.endswith(" ago"), status
        app.send(cmd="probe", x=WINDOW[0] // 2, y=WINDOW[1] // 2)
        assert app.state()["sidebar_status"] is None, "the status stays only while hovering"
    finally:
        app.close()


def flow_threads_live_in_projects():
    """With no project yet, the window offers to add one and holds no
    thread; adding one opens a thread in it. Deleting the last thread
    opens a fresh one in the same project: there's never a thread outside
    a project."""
    app = App("edit-file.jsonl", None)
    try:
        state = app.state()
        assert state["chat"] is None and state["sidebar"]["projects"] == [], state
        app.send(cmd="screenshot", path=str(OUT / "no-project.png"))
        workspace = fresh_workspace("first-project")
        app.send(cmd="add_project", path=workspace)
        state = app.state()
        assert state["chat"] is not None and state["cwd"] == workspace, state
        assert state["sidebar"]["projects"][0]["threads"] == ["New chat"], state["sidebar"]
        first = state["chats"]
        app.send(cmd="delete_chat", index=state["active"])
        state = app.state()
        assert state["chat"] is not None and state["cwd"] == workspace, state
        assert len(state["chats"]) == len(first), (state["chats"], first)
        assert state["sidebar"]["chats"] == [], state["sidebar"]
    finally:
        app.close()


def flow_resize_panes():
    """The sidebar and the changes panel resize by dragging their dividers
    with the mouse, within limits that leave the chat room; the sidebar's
    rows follow its new width."""
    app = App("edit-file.jsonl", fresh_workspace("resize"))
    try:
        app.send(cmd="type", text="calc.py has a bug in add(). Fix it, then reply with one sentence.")
        app.send(cmd="submit")
        wait_for(lambda: (s := app.state())["panel_open"] and not s["chat"]["busy"], "the changes panel")
        width, height = WINDOW

        def row_right_edge(x, y):
            """The right edge of the sidebar row highlighted on hovering
            (x, y), or None when that isn't over one."""
            rows = [r for r in app.send(cmd="probe", x=x, y=y)["appeared"] if contains(r, x, y)]
            return max((r[0] + r[2] for r in rows), default=None)

        # A thread row: the first height down the sidebar where hovering
        # lights up a row reaching the sidebar's padding.
        row = next(
            y for y in range(40, 300, 4)
            if (edge := row_right_edge(SIDEBAR_WIDTH // 2, y)) and abs(edge - (SIDEBAR_WIDTH - 8)) <= 1
        )
        new_chat = row

        assert row_right_edge(SIDEBAR_WIDTH + 100, new_chat) is None, "the chat area isn't sidebar before resizing"
        app.send(cmd="drag", **{"from": [SIDEBAR_WIDTH, height // 2], "to": [SIDEBAR_WIDTH + 140, height // 2]})
        layout = app.state()["layout"]
        assert layout["sidebar"] == SIDEBAR_WIDTH + 140, layout
        # Measured on screen, not from the app's own number: the row now
        # reaches across the wider sidebar (inside its 8px padding).
        edge = row_right_edge(SIDEBAR_WIDTH + 100, new_chat)
        assert edge is not None and abs(edge - (SIDEBAR_WIDTH + 140 - 8)) <= 1, edge
        assert not sidebar_spill(app), sidebar_spill(app)
        app.send(cmd="screenshot", path=str(OUT / "resize-wide-sidebar.png"))

        # Limits: the sidebar never shrinks past usable, the chat never
        # gets squeezed out by either side.
        app.send(cmd="drag", **{"from": [layout["sidebar"], height // 2], "to": [10, height // 2]})
        assert app.state()["layout"]["sidebar"] == 180, app.state()["layout"]
        panel_edge = width - layout["panel"]
        app.send(cmd="drag", **{"from": [panel_edge, height // 2], "to": [panel_edge - 100, height // 2]})
        layout = app.state()["layout"]
        assert layout["panel"] == 480 + 100, layout
        app.send(cmd="drag", **{"from": [width - layout["panel"], height // 2], "to": [0, height // 2]})
        layout = app.state()["layout"]
        assert width - layout["sidebar"] - layout["panel"] == 360, layout
        app.send(cmd="drag", **{"from": [layout["sidebar"], height // 2], "to": [width, height // 2]})
        layout = app.state()["layout"]
        assert layout["sidebar"] == 180, ("no room to widen; it stays", layout)
        app.send(cmd="drag", **{"from": [width - layout["panel"], height // 2], "to": [width, height // 2]})
        assert app.state()["layout"]["panel"] == 280, app.state()["layout"]
        app.send(cmd="screenshot", path=str(OUT / "resize-narrow-panel.png"))
    finally:
        app.close()


def intersects(a, b):
    ax, ay, aw, ah = a
    bx, by, bw, bh = b
    return ax < bx + bw and bx < ax + aw and ay < by + bh and by < ay + ah


def contains(rect, x, y):
    rx, ry, rw, rh = rect
    return rx <= x < rx + rw and ry <= y < ry + rh


def flow_layout():
    """Structural layout rules, checked over the whole window.

    - Hover: nothing that appears under the cursor may paint over the traffic
      lights.
    - Overlays: a menu opened by clicking a trigger must not cover the trigger.
    """
    app = App("edit-file.jsonl", fresh_workspace("layout"))
    try:
        app.send(cmd="type", text="calc.py has a bug in add(). Fix it, then reply with one sentence.")
        app.send(cmd="submit")
        wait_for(lambda: (s := app.state())["chat"]["session"] and not s["chat"]["busy"], "turn to end")

        violations = []
        step = 16
        for y in range(step // 2, WINDOW[1], step):
            for x in range(step // 2, WINDOW[0], step):
                for rect in app.send(cmd="probe", x=x, y=y)["appeared"]:
                    if intersects(rect, TRAFFIC_LIGHTS):
                        violations.append(f"hover at ({x},{y}) paints {rect} over the traffic lights")
        app.send(cmd="probe", x=WINDOW[0] // 2, y=WINDOW[1] // 2)

        for name, x, y in OVERLAY_TRIGGERS:
            hovered = app.send(cmd="probe", x=x, y=y)["appeared"]
            candidates = [r for r in hovered if contains(r, x, y)]
            if not candidates:
                violations.append(f"{name}: nothing highlights on hover at ({x},{y}); is the trigger still there?")
                continue
            trigger = max(candidates, key=lambda r: r[2] * r[3])
            opened = app.send(cmd="probe", x=x, y=y, click=True)["appeared"]
            if not opened:
                violations.append(f"{name}: clicking ({x},{y}) opened nothing")
            for rect in opened:
                inside_trigger = (
                    rect[0] >= trigger[0] and rect[1] >= trigger[1]
                    and rect[0] + rect[2] <= trigger[0] + trigger[2]
                    and rect[1] + rect[3] <= trigger[1] + trigger[3]
                )
                if intersects(rect, trigger) and not inside_trigger:
                    violations.append(f"{name}: overlay {rect} covers its trigger {trigger}")
            app.send(cmd="key", keys="escape")
            # Closing it must take the menu with it: none of what opened
            # outside the trigger may still be drawn.
            menu = [r for r in opened if not intersects(r, trigger)]
            drawn = app.send(cmd="quads")["quads"]
            lingering = [r for r in menu if r in drawn]
            if not menu:
                violations.append(f"{name}: clicking opened nothing outside the trigger")
            elif lingering:
                violations.append(f"{name}: escape left the menu open: {lingering}")
        assert not violations, "\n  " + "\n  ".join(violations[:10])
    finally:
        app.close()


def flow_sign_out_and_in():
    """Signing out stops the agent cleanly; signing back in works, and Stop
    always gets a chat out of "busy"."""
    app = App("edit-file.jsonl", fresh_workspace("relogin"))
    try:
        wait_for(lambda: app.state()["agent_running"], "agent to start")
        app.send(cmd="sign_out")
        assert not app.state()["agent_running"]

        app.send(cmd="type", text="queued while signed out")
        app.send(cmd="submit")
        assert app.state()["chat"]["busy"], "a prompt sent while signed out waits"
        app.send(cmd="stop")
        assert not app.state()["chat"]["busy"], "Stop must clear a prompt that never reached an agent"

        app.send(cmd="new_chat")
        app.send(cmd="sign_in")
        # Back to back, so the old agent's exit lands after the new one starts.
        app.send(cmd="sign_out")
        app.send(cmd="sign_in")
        time.sleep(0.5)  # long enough for the replaced agents' exits to arrive
        app.send(cmd="type", text="calc.py has a bug in add(). Fix it, then reply with one sentence.")
        app.send(cmd="submit")
        state = wait_for(
            lambda: (s := app.state())["chat"]["session"] and not s["chat"]["busy"] and s,
            "turn after signing back in",
        )
        assert state["agent_running"], "the new agent must survive the old one's exit"
        notices = [e["notice"] for e in state["chat"]["entries"] if "notice" in e]
        assert not any("stopped" in n for n in notices), notices
        assert "a + b" in state["chat"]["entries"][-1].get("agent", ""), state["chat"]["entries"]
    finally:
        app.close()


def flow_notices():
    """fx's diagnostics (recorded in a large real repo) become collapsed
    diagnostic rows, never part of the assistant's reply."""
    app = App("notices.jsonl", fresh_workspace("notices"))
    try:
        app.send(cmd="type", text="Reply with just the word ok. Do not use any tools.")
        app.send(cmd="submit")
        state = wait_for(
            lambda: (s := app.state())["chat"]["session"] and not s["chat"]["busy"] and s,
            "turn to end",
        )
        entries = state["chat"]["entries"]
        diagnostics = [e["diagnostic"] for e in entries if "diagnostic" in e]
        replies = [e["agent"] for e in entries if "agent" in e]
        assert any("skill catalog" in d for d in diagnostics), entries
        assert all(not e["expanded"] for e in entries if "diagnostic" in e), "diagnostics start collapsed"
        assert not [e for e in entries if "notice" in e], entries
        assert replies and not any("skill" in r for r in replies), replies
        app.send(cmd="screenshot", path=str(OUT / "notices.png"))
    finally:
        app.close()


def flow_two_chats_at_once():
    """Two chats run at the same time without crossing: each gets its own
    agent and session, and each transcript holds only its own prompt and
    reply. (fx runs every prompt in its one active session, so a shared agent
    would put one chat's prompt into the other's conversation.)"""
    app = App("edit-file.jsonl", fresh_workspace("two-chats"))

    def settled(index):
        app.send(cmd="select_chat", index=index)
        state = app.state()["chat"]
        return state["session"] and not state["busy"] and state

    try:
        app.send(cmd="type", text="first chat: fix calc.py")
        app.send(cmd="submit")
        app.send(cmd="new_chat")
        # Its own folder: two agents editing one file would conflict.
        app.send(cmd="set_folder", path=fresh_workspace("two-chats-second"))
        app.send(cmd="type", text="second chat: fix calc.py")
        app.send(cmd="submit")
        second = wait_for(lambda: settled(0), "second chat to finish")
        first = wait_for(lambda: settled(1), "first chat to finish")

        for state, prompt in ((first, "first chat: fix calc.py"), (second, "second chat: fix calc.py")):
            users = [e["user"] for e in state["entries"] if "user" in e]
            assert users == [prompt], f"expected only {prompt!r}, got {users}"
            replies = [e["agent"] for e in state["entries"] if "agent" in e]
            assert replies and "a + b" in replies[-1], state["entries"]
            notices = [e["notice"] for e in state["entries"] if "notice" in e]
            assert not notices, notices
        assert first["session"] != second["session"], "the two chats share a session"
    finally:
        app.close()


def flow_stop_mid_turn():
    """Stop while the agent is working: fx is told to cancel, the chat goes
    idle without an error, nothing more arrives, and the edit the agent would
    have made later never happens."""
    workspace = fresh_workspace("stop")
    app = App("edit-file.jsonl", workspace, pace="1")
    try:
        app.send(cmd="type", text="calc.py has a bug in add(). Fix it, then reply with one sentence.")
        app.send(cmd="submit")
        wait_for(
            lambda: any("agent" in e for e in app.state()["chat"]["entries"]),
            "the agent to start replying",
        )
        app.send(cmd="stop")
        state = wait_for(lambda: (s := app.state()) and not s["chat"]["busy"] and s, "the turn to stop")
        entries = state["chat"]["entries"]
        assert not [e for e in entries if "notice" in e], entries
        time.sleep(1.0)
        assert app.state()["chat"]["entries"] == entries, "the transcript kept growing after Stop"
        assert Path(workspace, "calc.py").read_text() == CALC_BEFORE, "the agent edited after Stop"
    finally:
        app.close()

    # Stopped after it talked, worked, talked and went on working: the turn
    # has no answer, so its middle text mustn't fold the work away as if
    # it were one.
    app = App("steps.jsonl", fresh_workspace("steps"), pace="1")
    try:
        app.send(cmd="type", text="First read calc.py and say in one sentence what's wrong.")
        app.send(cmd="submit")

        def talked_twice_then_worked(entries):
            kinds = [next(iter(e)) for e in entries]
            agents = [i for i, kind in enumerate(kinds) if kind == "agent"]
            return len(agents) >= 2 and "tool" in kinds[agents[0]:agents[1]] and "tool" in kinds[agents[1]:]

        wait_for(lambda: talked_twice_then_worked(app.state()["chat"]["entries"]), "talk, work, talk, work")
        app.send(cmd="stop")
        state = wait_for(lambda: (s := app.state()) and not s["chat"]["busy"] and s, "the turn to stop")
        assert state["chat"]["folds"] == [], state["chat"]["folds"]
    finally:
        app.close()


def flow_history_paging():
    """A past session from fx's store opens with its whole transcript, in
    order, fetched newest page first. Pages of two turns (gamma+beta, then
    alpha) exercise both the order within a page and older pages going
    above. Reading history never calls a model."""
    home = Path("/tmp/sense-e2e/home")
    if home.exists():
        shutil.rmtree(home)
    shutil.copytree(ROOT / "fixtures/fx-home", home)
    app = App(None, fresh_workspace("history"), env={
        "HOME": str(home),
        "SENSE_HISTORY_PAGE": "2",
        # The librarian is a real fx; the key is never used for a model.
        "OPENROUTER_API_KEY": "sk-unused-for-history",
    })
    try:
        chats = wait_for(lambda: (c := app.state()["chats"]) and len(c) > 1 and c, "past sessions to be listed")
        past = len(chats) - 1
        app.send(cmd="select_chat", index=past)

        def loaded():
            entries = app.state()["chat"]["entries"]
            return len([e for e in entries if "user" in e]) == 3 and entries

        entries = wait_for(loaded, "all three turns")
        users = [e["user"] for e in entries if "user" in e]
        assert users == [
            "Reply with just the word alpha.",
            "Reply with just the word beta.",
            "Reply with just the word gamma.",
        ], users
        replies = [e["agent"].strip().lower() for e in entries if "agent" in e]
        assert [r.rstrip(".") for r in replies] == ["alpha", "beta", "gamma"], replies
        assert next(iter(entries[0])) == "user" and next(iter(entries[-1])) == "agent", entries
        assert not app.state()["chat"]["busy"]
    finally:
        app.close()


def flow_subagents():
    """A subagent shows as a child tab under its chat while it works: a
    read-only transcript that opens with the parent's delegation, its own
    tool calls, and a changes panel scoped to its own edit. The first click
    on the parent opens it, later ones fold its subagents; hiding a child
    takes it off the sidebar."""
    workspace = fresh_workspace("subagent")
    app = App("subagent.jsonl", workspace)
    try:
        # In a folder of a project, the deepest the sidebar nests.
        app.send(cmd="add_project", path=workspace)
        app.send(cmd="new_folder", project=0)
        app.send(cmd="rename_folder", folder=0, name="Fixes")
        app.send(cmd="type", text="Use the subagent tool to fix calc.py.")
        app.send(cmd="submit")
        wait_for(lambda: (s := app.state()["subagents"]) and s["children"], "the subagent to be announced")
        state = wait_for(
            lambda: (s := app.state())["chat"]["session"] and not s["chat"]["busy"] and s,
            "the parent turn to end",
        )
        children = state["subagents"]["children"]
        assert len(children) == 1 and not children[0]["running"] and not children[0]["hidden"], children
        assert children[0]["title"].startswith("Subagent"), children

        app.send(cmd="select_child", index=0)
        child = app.state()["chat"]
        assert child["read_only"], "a subagent's transcript takes no input"
        kinds = [next(iter(e)) for e in child["entries"]]
        assert kinds[0] == "user" and "calc.py" in child["entries"][0]["user"], child["entries"][:2]
        tools = [e["tool"] for e in child["entries"] if "tool" in e]
        assert "Editing" in tools, tools
        assert "+    return a + b" in child["patch"] and "-    return a - b" in child["patch"], child["patch"]
        assert app.state()["panel_open"], "the child's changes panel"
        app.send(cmd="screenshot", path=str(OUT / "subagent-child.png"))

        # Viewing the child, the parent's row opens the parent; then folds.
        app.send(cmd="click_chat", index=0)
        state = app.state()
        assert not state["chat"]["read_only"] and not state["subagents"]["collapsed"], state["subagents"]
        app.send(cmd="click_chat", index=0)
        assert app.state()["subagents"]["collapsed"], "second click folds the subagents"
        app.send(cmd="click_chat", index=0)
        assert not app.state()["subagents"]["collapsed"], "third click unfolds them"
        assert app.send(cmd="drop_thread", index=0, project=0, folder=0)["moved"]
        app.send(cmd="screenshot", path=str(OUT / "subagent-tree.png"))
        # Nested under a project and a folder, the subagent row still fits
        # (it once overflowed by its indent, cutting off its button).
        assert app.state()["sidebar"]["projects"][0]["folders"][0]["threads"], app.state()["sidebar"]
        # Selected, so its pill paints its full width.
        app.send(cmd="select_child", index=0)
        assert not sidebar_spill(app), f"sidebar content cut off: {sidebar_spill(app)}"
        app.send(cmd="click_chat", index=0)
        assert not app.state()["chat"]["read_only"] and not app.state()["subagents"]["collapsed"]

        app.send(cmd="hide_child", index=0)
        assert app.state()["subagents"]["children"][0]["hidden"]
        # With nothing left to fold, the open chat's row stays open.
        app.send(cmd="click_chat", index=0)
        assert not app.state()["subagents"]["collapsed"]
        assert not [e for e in app.state()["chat"]["entries"] if "notice" in e]
    finally:
        app.close()


def flow_images():
    """Images reach the model: a pasted image attaches (text still pastes as
    text), goes out with the message, shows in its bubble, and the reply
    (recorded from the real model) is about it. The picker path attaches
    image files and skips others."""
    workspace = fresh_workspace("images")
    square = str(ROOT / "fixtures/images/red-square.png")
    app = App("image.jsonl", workspace)
    try:
        assert not app.send(cmd="paste_text", text="just words")["consumed"]
        assert app.send(cmd="paste_image", path=square)["consumed"]
        wait_for(lambda: app.state()["chat"]["attachments"] == ["image/png"], "the image to attach")
        # Drawn in the very next frame, not left blank until something else
        # happens to redraw it (pasted screenshots once never showed).
        assert app.send(cmd="count_color", rgb=RED)["count"] > 0, "the attached image isn't on screen"

        app.send(cmd="type", text="What color is the attached square? Reply with one word.")
        app.send(cmd="submit")
        state = wait_for(
            lambda: (s := app.state())["chat"]["session"] and not s["chat"]["busy"] and s,
            "the reply",
        )
        chat = state["chat"]
        assert not [e for e in chat["entries"] if "notice" in e], chat["entries"]
        assert chat["attachments"] == [], "sending takes the attachments"
        assert chat["entries"][0]["images"] == 1, chat["entries"][0]
        assert "red" in chat["entries"][-1]["agent"].lower(), chat["entries"]
        assert app.send(cmd="count_color", rgb=RED)["count"] > 0, "the sent image isn't in its bubble"

        app.send(cmd="new_chat")
        app.send(cmd="attach_image", path=square)
        app.send(cmd="attach_image", path=str(Path(workspace, "calc.py")))
        wait_for(lambda: app.state()["chat"]["attachments"] == ["image/png"], "the picked image")
        assert app.state()["chat"]["attachments"] == ["image/png"], "only images attach"

        # A screenshot-sized image shows in one frame too, shrunk to fit.
        big = Path(workspace, "big.png")
        big.write_bytes(solid_png(2000, 1300, RED))
        app.send(cmd="attach_image", path=str(big))
        wait_for(lambda: len(app.state()["chat"]["attachments"]) == 2, "the big image")
        red = app.send(cmd="count_color", rgb=RED)["count"]
        # Both chips, 56 points tall at 2x: the small square and the wide one.
        assert red > 2 * 56 * 56, red
        app.send(cmd="screenshot", path=str(OUT / "images-attached.png"))
    finally:
        app.close()

    # A screenshot-sized image in its sent message: on screen once the turn
    # is over, at the bubble's size (240 by 156 points, drawn at 2x).
    app = App("image-large.jsonl", fresh_workspace("images-large"))
    try:
        app.send(cmd="attach_image", path=str(ROOT / "fixtures/images/red-wide.png"))
        wait_for(lambda: app.state()["chat"]["attachments"] == ["image/png"], "the image to attach")
        app.send(cmd="type", text="What color is the attached image? Reply with one word.")
        app.send(cmd="submit")
        wait_for(lambda: (s := app.state())["chat"]["session"] and not s["chat"]["busy"], "the reply")
        red = app.send(cmd="count_color", rgb=RED)["count"]
        assert red >= 0.9 * 480 * 312, f"{red} red pixels: the image isn't drawn in its message"
        # And in the real window, where it once stayed blank.
        time.sleep(0.5)
        red = count_reddish(window_capture(app, OUT / "images-large-window.png"))
        assert red >= 0.8 * 480 * 312, f"{red} red pixels on screen: the image isn't shown in its message"
    finally:
        app.close()


RED = [220, 30, 30]


def window_capture(app, path):
    """The app's real window as the screen shows it, captured without
    activating it. Offscreen renders (the screenshot command) draw every
    image afresh and can hide what the real window leaves blank."""
    tool = ROOT / "target/window-id"
    source = ROOT / "scripts/window-id.swift"
    if not tool.exists() or tool.stat().st_mtime < source.stat().st_mtime:
        subprocess.run(["swiftc", "-O", str(source), "-o", str(tool)], check=True, capture_output=True)
    # The window list can lag the window by a moment.
    window = wait_for(
        lambda: subprocess.run([str(tool), str(app.process.pid)], capture_output=True, text=True).stdout.strip(),
        "the window to be listed",
        timeout=5,
    )
    subprocess.run(["screencapture", "-x", "-o", "-l", window, str(path)], check=True)
    return path


def count_reddish(png):
    """Pixels of a strong red in a capture. The screen's color profile
    shifts exact colors, so the test is loose."""
    import struct
    bmp = Path(str(png) + ".bmp")
    subprocess.run(["sips", "-s", "format", "bmp", str(png), "--out", str(bmp)], check=True, capture_output=True)
    data = bmp.read_bytes()
    offset, = struct.unpack_from("<I", data, 10)
    width, height = struct.unpack_from("<ii", data, 18)
    bits, = struct.unpack_from("<H", data, 28)
    step = bits // 8
    row = (width * step + 3) & ~3
    count = 0
    for y in range(abs(height)):
        line = data[offset + y * row: offset + y * row + width * step]
        for x in range(0, len(line), step):
            b, g, r = line[x], line[x + 1], line[x + 2]
            if r > 150 and g < 100 and b < 100:
                count += 1
    return count


def solid_png(width, height, rgb):
    """A PNG of one color, written by hand (no imaging library needed)."""
    import struct
    import zlib
    row = b"\x00" + bytes(rgb) * width
    raw = zlib.compress(row * height)

    def chunk(kind, data):
        return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data))
    header = struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0)
    return b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", header) + chunk(b"IDAT", raw) + chunk(b"IEND", b"")


def git(cwd, *args):
    return subprocess.run(
        ["git", "-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false", *args],
        cwd=cwd, check=True, capture_output=True, text=True,
    ).stdout.strip()


def flow_projects():
    """Projects and folders: a thread on the current branch works in the
    repository itself; a worktree thread works in a new worktree branched
    from origin/main without touching the checkout; folders are made,
    renamed and deleted; a thread dropped on a folder moves there and is
    remembered by session; it can't be dropped into another project."""
    root = Path("/tmp/sense-e2e/projects")
    if root.exists():
        shutil.rmtree(root)
    home = root / "home"
    home.mkdir(parents=True)
    origin = root / "origin"
    origin.mkdir()
    git(origin, "init", "--quiet", "-b", "main")
    (origin / "calc.py").write_text(CALC_BEFORE)
    git(origin, "add", "--all")
    git(origin, "commit", "--quiet", "-m", "seed")
    git(root, "clone", "--quiet", str(origin), "repo")
    repo = root / "repo"
    git(repo, "checkout", "--quiet", "-b", "local-work")
    # origin moves on after the clone: a worktree must start from the fetched
    # origin/main, not the clone's stale main.
    (origin / "later.txt").write_text("after the clone\n")
    git(origin, "add", "--all")
    git(origin, "commit", "--quiet", "-m", "later")
    other = root / "other"
    other.mkdir()

    # Opens with a thread in the repository: its folder becomes a project.
    app = App("edit-file.jsonl", str(repo), env={"HOME": str(home)})
    try:
        assert app.state()["sidebar"]["projects"] == [{"name": "repo", "folders": [], "threads": ["New chat"]}]
        app.send(cmd="add_project", path=str(repo))
        app.send(cmd="add_project", path=str(other))
        app.send(cmd="new_thread", project=0, mode="current")
        state = app.state()
        assert state["cwd"] == str(repo), state["cwd"]
        assert state["sidebar"]["projects"][0]["threads"] == ["New chat", "New chat"], state["sidebar"]

        # A new folder's name is ready to be typed over: no select-all and
        # delete first. Real keystrokes, into whatever has focus.
        app.send(cmd="new_folder", project=0)
        app.send(cmd="key", keys="shift-f i x e s enter")
        assert app.state()["sidebar"]["projects"][0]["folders"][0]["name"] == "Fixes", app.state()["sidebar"]

        app.send(cmd="type", text="calc.py has a bug in add(). Fix it, then reply with one sentence.")
        app.send(cmd="submit")
        state = wait_for(lambda: (s := app.state())["chat"]["session"] and not s["chat"]["busy"] and s, "the turn")
        session = state["chat"]["session"]
        title = state["chats"][0]

        assert app.send(cmd="drop_thread", index=0, project=0, folder=0)["moved"]
        assert not app.send(cmd="drop_thread", index=0, project=1, folder=None)["moved"], \
            "a thread can't move to another repository's project"
        sidebar = app.state()["sidebar"]
        assert sidebar["projects"][0]["folders"] == [{"name": "Fixes", "threads": [title]}], sidebar
        assert sidebar["projects"][0]["threads"] == ["New chat"], sidebar
        assert sidebar["chats"] == [], "every thread is in a project"
        library = wait_for(
            lambda: (p := home / "Library/Application Support/Sense/library.json").exists()
            and session in (l := json.loads(p.read_text()))["threads"] and l,
            "the library to be saved",
        )
        folder_id = library["folders"][0]["id"]
        assert library["threads"][session]["folder"] == folder_id, library

        app.send(cmd="screenshot", path=str(OUT / "projects.png"))
        # Nothing in the sidebar is too wide for it (indented rows once were,
        # cutting off their buttons).
        # (Ignoring hairlines: the sidebar's own 1px border.)
        spill = sidebar_spill(app)
        assert not spill, f"sidebar content cut off at its edge ([x, y, width, visible width]): {spill}"
        app.send(cmd="new_thread", project=0, mode="worktree")
        state = wait_for(lambda: (s := app.state())["preparing"] is None and s, "the worktree", timeout=30)
        worktree = Path(state["cwd"])
        assert str(worktree).startswith(str(home / "Library/Application Support/Sense/worktrees/repo")), worktree
        assert git(worktree, "rev-parse", "--abbrev-ref", "HEAD").startswith("sense/thread-")
        assert git(worktree, "rev-parse", "HEAD") == git(origin, "rev-parse", "main")
        assert (worktree / "later.txt").exists(), "the worktree has origin's latest commit"
        # Named by its branch where the composer says where the agent works,
        # not by the long path under Application Support.
        assert state["chat"]["place"] == git(worktree, "rev-parse", "--abbrev-ref", "HEAD"), state["chat"]["place"]
        app.send(cmd="screenshot", path=str(OUT / "projects-worktree.png"))
        assert git(repo, "rev-parse", "--abbrev-ref", "HEAD") == "local-work", "the checkout stays on its branch"

        app.send(cmd="delete_folder", folder=0)
        sidebar = app.state()["sidebar"]
        assert sidebar["projects"][0]["folders"] == [] and title in sidebar["projects"][0]["threads"], sidebar

        # Deleting a worktree thread removes its worktree and (having no
        # commits of its own) its branch; the checkout is untouched.
        branch = git(worktree, "rev-parse", "--abbrev-ref", "HEAD")
        app.send(cmd="delete_chat", index=app.state()["active"])
        wait_for(lambda: not worktree.exists(), "the worktree to be removed")
        assert str(worktree) not in git(repo, "worktree", "list"), git(repo, "worktree", "list")
        assert git(repo, "branch", "--list", branch) == "", f"{branch} should be deleted"
        assert git(repo, "rev-parse", "--abbrev-ref", "HEAD") == "local-work"

        # Deleted while its worktree is still being made: none is left
        # behind once the creation finishes.
        roots = home / "Library/Application Support/Sense/worktrees/repo"
        app.send(cmd="new_thread", project=0, mode="worktree")
        state = app.state()
        assert state["preparing"], "the worktree is still being made"
        app.send(cmd="delete_chat", index=state["active"])
        time.sleep(1.5)
        wait_for(lambda: not any(roots.iterdir()), "no worktree left behind")
        assert git(repo, "worktree", "list").count("\n") == 0, git(repo, "worktree", "list")
        assert git(repo, "branch", "--list", "sense/*") == "", git(repo, "branch", "--list", "sense/*")
    finally:
        app.close()


FLOWS = {"edit-file": flow_edit_file, "projects": flow_projects, "images": flow_images, "subagents": flow_subagents, "history-paging": flow_history_paging, "stop-mid-turn": flow_stop_mid_turn, "two-chats": flow_two_chats_at_once, "notices": flow_notices, "layout": flow_layout, "resize-panes": flow_resize_panes, "text-size": flow_text_size, "appearance": flow_appearance, "view": flow_view, "view-gallery": flow_view_gallery, "model": flow_model_picker, "code-copy": flow_code_copy, "math": flow_math, "queue": flow_queue, "status": flow_thread_status, "projects-only": flow_threads_live_in_projects, "sign-out-and-in": flow_sign_out_and_in}


def main():
    OUT.mkdir(parents=True, exist_ok=True)
    if not BINARY.exists():
        sys.exit(f"build first: cargo build -p sense --features automation ({BINARY} missing)")
    selected = sys.argv[1:] or list(FLOWS)
    failed = 0
    for name in selected:
        start = time.monotonic()
        try:
            FLOWS[name]()
            print(f"ok    {name} ({time.monotonic() - start:.1f}s)")
        except Exception as err:  # one broken flow mustn't stop the rest
            failed += 1
            print(f"FAIL  {name}: {type(err).__name__}: {err}")
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
