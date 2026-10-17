#!/usr/bin/env python3
"""Plants known bugs in the lifecycle one at a time and checks the tests
catch each. A surviving mutant means a behavior the tests don't prove.

usage: scripts/mutants-core.py
"""
import shutil
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
LIB = ROOT / "crates/sense-core/src/lib.rs"

# name -> (original text, buggy replacement). Each original must occur once.
MUTANTS = {
    "events from replaced agents act": (
        "        if !self.is_current(&input) {",
        "        if false && !self.is_current(&input) {",
    ),
    "Stop leaves a waiting prompt": (
        "                    _ => chat.turn = Turn::Idle,",
        "                    _ => {}",
    ),
    "Stop on a running turn skips the cancel": (
        "                        chat.turn = Turn::Cancelling;",
        "                        effects.pop();\n                        chat.turn = Turn::Idle;",
    ),
    "second Stop doesn't kill the agent": (
        "                        effects.push(Effect::StopAgent { agent });\n                        chat.agent = None;\n                        chat.creating = false;\n                        chat.turn = Turn::Idle;",
        "                        let _ = agent;\n                        chat.turn = Turn::Idle;",
    ),
    "second Stop does nothing": (
        "                        effects.push(Effect::StopAgent { agent });\n                        chat.agent = None;\n                        chat.creating = false;\n                        chat.turn = Turn::Idle;",
        "                        let _ = agent;",
    ),
    "sign-in keeps old agents": (
        "                self.stop_agents(Notice::AgentStopped, &mut effects);\n                self.credential = Some(credential);",
        "                self.credential = Some(credential);",
    ),
    "a prompt can be sent twice": (
        "                chat.turn = Turn::Running;\n            }\n            None if",
        "            }\n            None if",
    ),
    "delete keeps the agent": (
        "                if let Some(agent) = chat.agent {\n                    effects.push(Effect::StopAgent { agent });\n                }\n                effects.push(Effect::ForgetChat",
        "                effects.push(Effect::ForgetChat",
    ),
    "delete forgets the session": (
        "                    session: chat.session,\n                });",
        "                    session: None,\n                });",
    ),
    "no baseline requested": (
        "                    effects.push(Effect::TakeBaseline {\n                        chat: id,\n                        cwd: chat.cwd.clone(),\n                    });",
        "",
    ),
    "failed baseline counts as ready": (
        "                        chat.baseline = Baseline::Unavailable;",
        "                        chat.baseline = Baseline::Ready;",
    ),
    "folder changes after the baseline": (
        "                    && chat.baseline == Baseline::NotTaken\n                {\n                    chat.cwd = cwd;",
        "                {\n                    chat.cwd = cwd;",
    ),
    "turn end doesn't refresh the diff": (
        "                if chat.baseline == Baseline::Ready {\n                    effects.push(Effect::RefreshDiff { chat: id });\n                }\n            }\n            Input::ToolFinished",
        "            }\n            Input::ToolFinished",
    ),
    "tool end doesn't refresh the diff": (
        "                if self.chats[&id].baseline == Baseline::Ready {\n                    effects.push(Effect::RefreshDiff { chat: id });\n                }",
        "",
    ),
    "signed-out prompt isn't flagged": (
        "                if self.signed_out {",
        "                if false {",
    ),
    "failed turn isn't reported": (
        "                    if let Some(error) = error {\n                        effects.push(Effect::Notify {\n                            chat: id,\n                            message: Notice::TurnFailed(error),\n                        });\n                    }",
        "                    let _ = error;",
    ),
    "sign-out reported as a crash": (
        "                self.stop_agents(Notice::SignedOut, &mut effects);",
        "                self.stop_agents(Notice::AgentStopped, &mut effects);",
    ),
    "chats share an agent": (
        "        if chat.agent.is_some() {\n            return;\n        }\n        self.next_agent += 1;",
        "        if chat.agent.is_some() {\n            return;\n        }\n        if self.next_agent > 0 && self.chats.values().any(|c| c.agent == Some(self.next_agent)) {\n            chat.agent = Some(self.next_agent);\n            return;\n        }\n        self.next_agent += 1;",
    ),
    "exit keeps a waiting prompt": (
        "                if chat.is_busy() {\n                    chat.turn = Turn::Idle;\n                    effects.push(Effect::Notify {\n                        chat: id,\n                        message: Notice::AgentStopped,\n                    });\n                }\n            }\n            Input::SessionCreated",
        "                if matches!(chat.turn, Turn::Running | Turn::Cancelling) {\n                    chat.turn = Turn::Idle;\n                }\n            }\n            Input::SessionCreated",
    ),
}


def tests_pass():
    result = subprocess.run(
        ["cargo", "test", "-q", "-p", "sense-core"],
        cwd=ROOT, capture_output=True, text=True,
    )
    return result.returncode == 0


def main():
    original = LIB.read_text()
    backup = LIB.with_suffix(".rs.orig")
    shutil.copy(LIB, backup)
    survived = []
    try:
        for name, (find, replace) in MUTANTS.items():
            count = original.count(find)
            if count != 1:
                print(f"STALE  {name}: pattern found {count} times; update this mutant")
                survived.append(name)
                continue
            LIB.write_text(original.replace(find, replace))
            caught = not tests_pass()
            print(f"{'caught' if caught else 'SURVIVED'}  {name}")
            if not caught:
                survived.append(name)
    finally:
        shutil.move(backup, LIB)
        # The restored file keeps its old timestamp, older than the last
        # mutant's build; touch it or cargo keeps testing that mutant.
        LIB.touch()
    sys.exit(1 if survived else 0)


if __name__ == "__main__":
    main()
