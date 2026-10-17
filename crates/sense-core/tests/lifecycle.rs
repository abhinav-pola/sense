//! Model-based tests of the lifecycle: random sequences of user actions run
//! against simulated fx processes that answer, fail, hang, crash and deliver
//! late, in any interleaving the real channels allow (each agent's events
//! stay in order; different agents' events interleave freely).
//!
//! The model keeps its own record of which agent and session belong to which
//! chat, built only from effects and events, and never asks the lifecycle:
//! an oracle that reused the code under test would agree with its bugs.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::PathBuf;

use proptest::prelude::*;
use sense_core::{AgentId, ChatId, Command, Effect, Image, Input, Lifecycle, Notice, Prompt, Turn};

#[derive(Clone, Debug)]
enum Op {
    NewChat,
    /// A past session listed by fx, shown as a chat.
    Restore,
    Focus(usize),
    SetFolder(usize),
    Submit(usize),
    Stop(usize),
    Delete(usize),
    SignIn,
    SignOut,
    /// A live agent handles its oldest request; `bool` makes it fail.
    Process(usize, bool),
    /// A live agent finishes its turn; `bool` makes it fail.
    FinishTurn(usize, bool),
    /// A live agent stops answering cancels and never finishes its turn.
    Hang(usize),
    Crash(usize),
    /// The oldest undelivered event of some agent reaches the app.
    Deliver(usize),
    Baseline(usize, bool),
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        2 => Just(Op::NewChat),
        1 => Just(Op::Restore),
        2 => (0..4usize).prop_map(Op::Focus),
        1 => (0..4usize).prop_map(Op::SetFolder),
        5 => (0..4usize).prop_map(Op::Submit),
        4 => (0..4usize).prop_map(Op::Stop),
        1 => (0..4usize).prop_map(Op::Delete),
        1 => Just(Op::SignIn),
        1 => Just(Op::SignOut),
        6 => (0..8usize, proptest::bool::weighted(0.1)).prop_map(|(a, f)| Op::Process(a, f)),
        4 => (0..8usize, proptest::bool::weighted(0.1)).prop_map(|(a, f)| Op::FinishTurn(a, f)),
        1 => (0..8usize).prop_map(Op::Hang),
        1 => (0..8usize).prop_map(Op::Crash),
        9 => (0..8usize).prop_map(Op::Deliver),
        2 => (0..4usize, proptest::bool::weighted(0.8)).prop_map(|(c, ok)| Op::Baseline(c, ok)),
    ]
}

/// A simulated fx process: one active session, one turn at a time.
#[derive(Default)]
struct Agent {
    chat: ChatId,
    credential: String,
    /// Requests received and not yet handled.
    requests: VecDeque<Command>,
    /// The session whose turn is in progress.
    turn: Option<String>,
    /// Ignores cancels and never finishes its turn.
    hung: bool,
    /// Events not yet delivered to the app, in send order.
    outbox: VecDeque<Input>,
    exited: bool,
}

#[derive(Default)]
struct World {
    life: Lifecycle,
    agents: BTreeMap<AgentId, Agent>,
    chats: Vec<ChatId>,
    next_chat: ChatId,
    next_session: u64,
    // The model's own bookkeeping, from effects and events only.
    /// Agents the app started and hasn't stopped or seen exit, and their chat.
    owner: BTreeMap<AgentId, ChatId>,
    /// Sessions the app accepted, by chat.
    session_of: BTreeMap<ChatId, String>,
    /// Folders snapshotted, by chat; and which of those succeeded.
    baseline_cwd: BTreeMap<ChatId, PathBuf>,
    baseline_ready: BTreeSet<ChatId>,
    pending_baselines: Vec<ChatId>,
    cwd: BTreeMap<ChatId, PathBuf>,
    /// Prompt texts the app accepted, and the ones it sent to an agent.
    accepted: BTreeMap<String, Prompt>,
    sent: Vec<String>,
    next_text: u64,
    /// The key from the latest sign-in, while signed in.
    credential: Option<String>,
    known_signed_out: bool,
    sign_ins: u32,
}

impl World {
    fn new() -> Self {
        Self {
            next_chat: 1,
            next_session: 1,
            ..Self::default()
        }
    }

    fn chat(&self, ix: usize) -> Option<ChatId> {
        (!self.chats.is_empty()).then(|| self.chats[ix % self.chats.len()])
    }

    fn live_agent(&self, ix: usize) -> Option<AgentId> {
        let live: Vec<_> = self
            .agents
            .iter()
            .filter(|(_, a)| !a.exited)
            .map(|(id, _)| *id)
            .collect();
        (!live.is_empty()).then(|| live[ix % live.len()])
    }

    fn agent_with_mail(&self, ix: usize) -> Option<AgentId> {
        let ready: Vec<_> = self
            .agents
            .iter()
            .filter(|(_, a)| !a.outbox.is_empty())
            .map(|(id, _)| *id)
            .collect();
        (!ready.is_empty()).then(|| ready[ix % ready.len()])
    }

    fn turn(&self, chat: ChatId) -> Option<Turn> {
        self.life.chat(chat).map(|c| c.turn.clone())
    }

    /// Feeds an input to the lifecycle, checks it, and plays out its effects.
    fn input(&mut self, input: Input) -> Result<(), TestCaseError> {
        let before = self.life.clone();
        let stale = match &input {
            Input::SessionCreated { agent, .. }
            | Input::SessionFailed { agent, .. }
            | Input::TurnEnded { agent, .. }
            | Input::ToolFinished { agent, .. }
            | Input::AgentExited { agent } => !self.owner.contains_key(agent),
            _ => false,
        };
        let effects = self.life.step(input.clone());
        self.life
            .check_invariants()
            .map_err(|err| TestCaseError::fail(format!("after {input:?}: {err}")))?;

        if stale {
            prop_assert!(effects.is_empty(), "stale {input:?} produced {effects:?}");
            prop_assert_eq!(&self.life, &before, "stale {:?} changed state", input);
        }
        self.check_input(&input, &before, &effects)?;
        for effect in effects {
            self.effect(effect)?;
        }

        for (chat, state) in self.life.chats() {
            if let Some(agent) = state.agent {
                prop_assert_eq!(
                    self.owner.get(&agent),
                    Some(&chat),
                    "chat {}'s agent was never started for it",
                    chat
                );
                let started_with = Some(&self.agents[&agent].credential);
                prop_assert_eq!(
                    started_with,
                    self.credential.as_ref(),
                    "chat {}'s agent uses an old key",
                    chat
                );
            }
        }
        for (id, agent) in &self.agents {
            if !self.owner.contains_key(id) {
                prop_assert!(agent.exited, "agent {id} was dropped but never stopped");
            }
        }
        Ok(())
    }

    /// What each input must do, beyond the invariants.
    fn check_input(
        &mut self,
        input: &Input,
        before: &Lifecycle,
        effects: &[Effect],
    ) -> Result<(), TestCaseError> {
        match input {
            Input::Submit { chat, prompt } => {
                let text = &prompt.text;
                let was_idle = before.chat(*chat).is_some_and(|c| !c.is_busy());
                if was_idle {
                    self.accepted.insert(text.clone(), prompt.clone());
                    if self.known_signed_out {
                        prop_assert!(
                            effects.contains(&Effect::Notify {
                                chat: *chat,
                                message: Notice::NeedsSignIn
                            }),
                            "a prompt sent while signed out must say so"
                        );
                    }
                }
            }
            Input::Stop { chat } => {
                let Some(was) = before.chat(*chat).map(|c| c.turn.clone()) else {
                    return Ok(());
                };
                let now = self.turn(*chat).unwrap();
                match was {
                    Turn::Running => {
                        let session = self.session_of[chat].clone();
                        let agent = before.chat(*chat).unwrap().agent.unwrap();
                        prop_assert!(
                            effects.contains(&Effect::Send {
                                agent,
                                command: Command::Cancel { session }
                            }),
                            "stopping a running turn must cancel it in fx"
                        );
                        prop_assert_eq!(now, Turn::Cancelling);
                    }
                    _ => {
                        prop_assert_eq!(now, Turn::Idle, "Stop must free a chat that isn't running")
                    }
                }
            }
            Input::SignedOut => {
                for (chat, state) in before.chats() {
                    if matches!(state.turn, Turn::Running | Turn::Cancelling) {
                        let told = effects.contains(&Effect::Notify {
                            chat,
                            message: Notice::SignedOut,
                        });
                        prop_assert!(told, "signing out mid-turn must say so");
                    }
                }
            }
            Input::TurnEnded {
                agent,
                error: Some(error),
                ..
            } if self.owner.contains_key(agent) => {
                let chat = self.owner[agent];
                if before.chat(chat).is_some_and(|c| c.turn == Turn::Running) {
                    let failed = Effect::Notify {
                        chat,
                        message: Notice::TurnFailed(error.clone()),
                    };
                    prop_assert!(effects.contains(&failed), "a failed turn must be reported");
                }
            }
            _ => {}
        }
        if let Input::TurnEnded { agent, .. } | Input::ToolFinished { agent, .. } = input
            && let Some(chat) = self.owner.get(agent)
            && self.baseline_ready.contains(chat)
        {
            prop_assert!(
                effects.contains(&Effect::RefreshDiff { chat: *chat }),
                "files may have changed; the diff must refresh"
            );
        }
        Ok(())
    }

    fn effect(&mut self, effect: Effect) -> Result<(), TestCaseError> {
        match effect {
            Effect::StartAgent {
                agent,
                chat,
                credential,
            } => {
                prop_assert!(
                    !self.agents.contains_key(&agent),
                    "agent {agent} started twice"
                );
                prop_assert!(
                    self.owner.values().all(|c| *c != chat),
                    "chat {chat} got a second agent"
                );
                self.owner.insert(agent, chat);
                self.agents.insert(
                    agent,
                    Agent {
                        chat,
                        credential,
                        ..Agent::default()
                    },
                );
            }
            Effect::StopAgent { agent } => {
                self.owner.remove(&agent);
                self.exit(agent);
            }
            Effect::Send { agent, command } => {
                let owner = self.owner.get(&agent).copied();
                prop_assert!(
                    owner.is_some(),
                    "sent {command:?} to an agent the app doesn't own"
                );
                let chat = owner.unwrap();
                match &command {
                    Command::NewSession { chat: target, cwd } => {
                        prop_assert_eq!(*target, chat, "NewSession sent to another chat's agent");
                        prop_assert_eq!(
                            Some(cwd),
                            self.baseline_cwd.get(&chat),
                            "session folder isn't the snapshotted one"
                        );
                    }
                    Command::Prompt {
                        session,
                        cwd,
                        prompt,
                    } => {
                        let text = &prompt.text;
                        prop_assert_eq!(
                            Some(session),
                            self.session_of.get(&chat),
                            "prompt for another chat's session"
                        );
                        prop_assert_eq!(
                            Some(cwd),
                            self.baseline_cwd.get(&chat),
                            "prompt folder isn't the snapshotted one"
                        );
                        prop_assert_eq!(
                            self.accepted.get(text),
                            Some(prompt),
                            "sent a prompt the user never submitted, or changed it (images included)"
                        );
                        prop_assert!(!self.sent.contains(text), "prompt {text:?} sent twice");
                        let target = &self.agents[&agent];
                        let busy = target.turn.is_some()
                            || target
                                .requests
                                .iter()
                                .any(|r| matches!(r, Command::Prompt { .. }));
                        prop_assert!(
                            !busy,
                            "prompt sent to agent {agent} while it was still running a turn"
                        );
                        self.sent.push(text.clone());
                    }
                    Command::Cancel { session } => {
                        prop_assert_eq!(
                            Some(session),
                            self.session_of.get(&chat),
                            "cancel for another chat's session"
                        );
                    }
                }
                let target = self.agents.get_mut(&agent).unwrap();
                if !target.exited {
                    target.requests.push_back(command);
                }
            }
            Effect::TakeBaseline { chat, cwd } => {
                prop_assert!(
                    !self.baseline_cwd.contains_key(&chat),
                    "chat {chat} snapshotted twice"
                );
                prop_assert_eq!(Some(&cwd), self.cwd.get(&chat));
                self.baseline_cwd.insert(chat, cwd);
                self.pending_baselines.push(chat);
            }
            Effect::ForgetChat { chat, session } => {
                prop_assert_eq!(
                    session.as_ref(),
                    self.session_of.get(&chat),
                    "deleted chat's session isn't the one it had"
                );
                self.chats.retain(|c| *c != chat);
                self.session_of.remove(&chat);
            }
            Effect::RefreshDiff { chat } => {
                prop_assert!(
                    self.baseline_ready.contains(&chat),
                    "diff refreshed without a snapshot"
                );
            }
            Effect::Notify { .. } => {}
        }
        Ok(())
    }

    /// The agent stops: unhandled work is lost, then it reports its exit.
    fn exit(&mut self, agent: AgentId) {
        if let Some(target) = self.agents.get_mut(&agent)
            && !target.exited
        {
            target.exited = true;
            target.requests.clear();
            target.turn = None;
            target.outbox.push_back(Input::AgentExited { agent });
        }
    }

    fn process(&mut self, agent: AgentId, fail: bool) {
        let session = format!("s{}", self.next_session);
        let target = self.agents.get_mut(&agent).unwrap();
        if target.exited {
            return;
        }
        let Some(request) = target.requests.pop_front() else {
            return;
        };
        let chat = target.chat;
        match request {
            Command::NewSession { .. } if fail => target.outbox.push_back(Input::SessionFailed {
                agent,
                chat,
                error: "boom".into(),
            }),
            Command::NewSession { .. } => {
                self.next_session += 1;
                target.outbox.push_back(Input::SessionCreated {
                    agent,
                    chat,
                    session,
                });
            }
            Command::Prompt { session, .. } if fail => target.outbox.push_back(Input::TurnEnded {
                agent,
                session,
                error: Some("couldn't resume".into()),
            }),
            Command::Prompt { session, .. } => target.turn = Some(session),
            Command::Cancel { session } => {
                if !target.hung && target.turn.as_ref() == Some(&session) {
                    target.turn = None;
                    target.outbox.push_back(Input::TurnEnded {
                        agent,
                        session,
                        error: None,
                    });
                }
            }
        }
    }

    fn finish_turn(&mut self, agent: AgentId, fail: bool) {
        let target = self.agents.get_mut(&agent).unwrap();
        if target.hung || target.exited {
            return;
        }
        if let Some(session) = target.turn.take() {
            target.outbox.push_back(Input::ToolFinished {
                agent,
                session: session.clone(),
            });
            target.outbox.push_back(Input::TurnEnded {
                agent,
                session,
                error: fail.then(|| "model error".into()),
            });
        }
    }

    fn deliver(&mut self, agent: AgentId) -> Result<(), TestCaseError> {
        let Some(event) = self.agents.get_mut(&agent).unwrap().outbox.pop_front() else {
            return Ok(());
        };
        let current = self.owner.contains_key(&agent);
        // A session the app is about to accept: the prompt it triggers is
        // checked against it.
        if current
            && let Input::SessionCreated { chat, session, .. } = &event
            && !self.session_of.contains_key(chat)
            && self.life.chat(*chat).is_some()
        {
            self.session_of.insert(*chat, session.clone());
        }
        let exited = matches!(event, Input::AgentExited { .. });
        self.input(event)?;
        if current && exited {
            self.owner.remove(&agent);
        }
        Ok(())
    }

    fn run(&mut self, op: Op) -> Result<(), TestCaseError> {
        match op {
            Op::NewChat => {
                let chat = self.next_chat;
                self.next_chat += 1;
                self.chats.push(chat);
                self.cwd.insert(chat, PathBuf::from("/work"));
                self.input(Input::NewChat {
                    chat,
                    cwd: PathBuf::from("/work"),
                })
            }
            Op::Restore => {
                let chat = self.next_chat;
                self.next_chat += 1;
                let session = format!("past{chat}");
                self.chats.push(chat);
                self.cwd.insert(chat, PathBuf::from("/work"));
                // The model knows the session the chat was restored with.
                self.session_of.insert(chat, session.clone());
                self.input(Input::RestoreChat {
                    chat,
                    cwd: PathBuf::from("/work"),
                    session,
                })
            }
            Op::Focus(ix) => match self.chat(ix) {
                Some(chat) => self.input(Input::Focus { chat }),
                None => Ok(()),
            },
            Op::SetFolder(ix) => match self.chat(ix) {
                Some(chat) => {
                    let cwd = PathBuf::from(format!("/elsewhere/{}", self.next_text));
                    let was = self.life.chat(chat).unwrap().cwd.clone();
                    self.input(Input::SetFolder {
                        chat,
                        cwd: cwd.clone(),
                    })?;
                    let now = self.life.chat(chat).unwrap().cwd.clone();
                    // The folder may only change before anything used it.
                    if now != was {
                        prop_assert_eq!(&now, &cwd);
                        prop_assert!(
                            !self.baseline_cwd.contains_key(&chat),
                            "folder changed after it was snapshotted"
                        );
                        prop_assert!(
                            !self.session_of.contains_key(&chat),
                            "folder changed after the session started"
                        );
                        self.cwd.insert(chat, cwd);
                    }
                    Ok(())
                }
                None => Ok(()),
            },
            Op::Submit(ix) => match self.chat(ix) {
                Some(chat) => {
                    self.next_text += 1;
                    let text = format!("prompt {}", self.next_text);
                    // Every third prompt carries images, so a lifecycle that
                    // dropped or swapped them would be caught.
                    let images = (0..self.next_text % 3)
                        .map(|n| Image {
                            mime: "image/png".into(),
                            bytes: format!("{text} image {n}").into_bytes().into(),
                        })
                        .collect();
                    // Prompts name different models (or none), so one sent
                    // with another's model would be caught too.
                    let model = (!self.next_text.is_multiple_of(4))
                        .then(|| format!("model-{}", self.next_text % 3));
                    self.input(Input::Submit {
                        chat,
                        prompt: Prompt {
                            text,
                            images,
                            model,
                        },
                    })
                }
                None => Ok(()),
            },
            Op::Stop(ix) => match self.chat(ix) {
                Some(chat) => self.input(Input::Stop { chat }),
                None => Ok(()),
            },
            Op::Delete(ix) => match self.chat(ix) {
                Some(chat) => self.input(Input::DeleteChat { chat }),
                None => Ok(()),
            },
            Op::SignIn => {
                // Each sign-in brings a different key, so an agent left over
                // from an earlier one would be caught using a stale key.
                self.sign_ins += 1;
                let credential = format!("key{}", self.sign_ins);
                self.credential = Some(credential.clone());
                self.known_signed_out = false;
                self.input(Input::SignedIn { credential })
            }
            Op::SignOut => {
                self.credential = None;
                self.known_signed_out = true;
                self.input(Input::SignedOut)
            }
            Op::Process(ix, fail) => {
                if let Some(agent) = self.live_agent(ix) {
                    self.process(agent, fail);
                }
                Ok(())
            }
            Op::FinishTurn(ix, fail) => {
                if let Some(agent) = self.live_agent(ix) {
                    self.finish_turn(agent, fail);
                }
                Ok(())
            }
            Op::Hang(ix) => {
                if let Some(agent) = self.live_agent(ix) {
                    self.agents.get_mut(&agent).unwrap().hung = true;
                }
                Ok(())
            }
            Op::Crash(ix) => {
                if let Some(agent) = self.live_agent(ix) {
                    self.exit(agent);
                }
                Ok(())
            }
            Op::Deliver(ix) => match self.agent_with_mail(ix) {
                Some(agent) => self.deliver(agent),
                None => Ok(()),
            },
            Op::Baseline(ix, ok) => {
                if self.pending_baselines.is_empty() {
                    return Ok(());
                }
                let chat = self
                    .pending_baselines
                    .remove(ix % self.pending_baselines.len());
                self.finish_baseline(chat, ok)
            }
        }
    }

    fn finish_baseline(&mut self, chat: ChatId, ok: bool) -> Result<(), TestCaseError> {
        if ok && self.life.chat(chat).is_some() {
            self.baseline_ready.insert(chat);
        }
        let error = (!ok).then(|| "too large".into());
        self.input(Input::BaselineTaken { chat, error })
    }

    /// Lets every agent that isn't hung finish its work and every event
    /// arrive.
    fn settle(&mut self) -> Result<(), TestCaseError> {
        for _ in 0..1000 {
            let mut progressed = false;
            for agent in self.agents.keys().copied().collect::<Vec<_>>() {
                let target = &self.agents[&agent];
                if !target.exited && !target.requests.is_empty() {
                    self.process(agent, false);
                    progressed = true;
                } else if !target.exited && !target.hung && target.turn.is_some() {
                    self.finish_turn(agent, false);
                    progressed = true;
                } else if !target.outbox.is_empty() {
                    self.deliver(agent)?;
                    progressed = true;
                }
            }
            for chat in std::mem::take(&mut self.pending_baselines) {
                self.finish_baseline(chat, true)?;
                progressed = true;
            }
            if !progressed {
                return Ok(());
            }
        }
        Err(TestCaseError::fail("the world never settled"))
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 3000, ..ProptestConfig::default() })]

    /// Safety after every step (see `World::input`), then liveness once the
    /// world settles: two Stops free any chat, even one whose agent hangs,
    /// and a signed-in chat can always get a new prompt through, once.
    #[test]
    fn lifecycle_holds_under_any_interleaving(ops in proptest::collection::vec(op(), 0..100)) {
        let mut world = World::new();
        for op in ops {
            world.run(op)?;
        }
        world.settle()?;

        for chat in world.chats.clone() {
            for _ in 0..2 {
                if world.life.chat(chat).unwrap().is_busy() {
                    world.input(Input::Stop { chat })?;
                    world.settle()?;
                }
            }
            prop_assert_eq!(world.turn(chat), Some(Turn::Idle), "two Stops didn't free chat {}", chat);
        }

        if world.life.signed_in() {
            for chat in world.chats.clone() {
                // A prompt gets through; if the agent has hung, two Stops kill
                // it and the same prompt, sent again, gets through on a fresh
                // one.
                world.next_text += 1;
                let text = format!("final {}", world.next_text);
                world.input(Input::Submit { chat, prompt: Prompt::text(text.clone()) })?;
                world.settle()?;
                if world.life.chat(chat).unwrap().is_busy() {
                    let agent = world.life.chat(chat).unwrap().agent;
                    prop_assert!(agent.is_some_and(|a| world.agents[&a].hung), "chat {} stuck on a healthy agent", chat);
                    for _ in 0..2 {
                        world.input(Input::Stop { chat })?;
                        world.settle()?;
                    }
                    world.next_text += 1;
                    let retry = format!("final {}", world.next_text);
                    world.input(Input::Submit { chat, prompt: Prompt::text(retry.clone()) })?;
                    world.settle()?;
                    prop_assert!(world.sent.contains(&retry), "chat {}'s retry never reached fx", chat);
                } else {
                    prop_assert!(world.sent.contains(&text), "chat {}'s final prompt never reached fx", chat);
                }
                prop_assert_eq!(world.turn(chat), Some(Turn::Idle), "chat {}'s final prompt never finished", chat);
            }
        }
    }
}

/// The bug from the first OAuth sign-in: the old agent's exit arrives after
/// the new agent started and must not affect it.
#[test]
fn late_exit_of_a_replaced_agent_is_ignored() {
    let mut life = Lifecycle::new();
    life.step(Input::NewChat {
        chat: 1,
        cwd: "/w".into(),
    });
    life.step(Input::SignedIn {
        credential: "env".into(),
    });
    life.step(Input::Focus { chat: 1 });
    life.step(Input::SignedOut);
    life.step(Input::SignedIn {
        credential: "oauth".into(),
    });
    life.step(Input::Focus { chat: 1 });
    let effects = life.step(Input::AgentExited { agent: 1 });
    assert!(effects.is_empty());
    assert_eq!(life.chat(1).unwrap().agent, Some(2));
    let effects = life.step(Input::Submit {
        chat: 1,
        prompt: Prompt::text("hi"),
    });
    assert!(effects.contains(&Effect::Send {
        agent: 2,
        command: Command::NewSession {
            chat: 1,
            cwd: "/w".into()
        },
    }));
}

/// Two chats never share an agent: fx runs every prompt in its one active
/// session, so a shared agent would put one chat's prompt in the other's.
#[test]
fn chats_get_their_own_agents() {
    let mut life = Lifecycle::new();
    life.step(Input::SignedIn {
        credential: "key".into(),
    });
    for chat in [1, 2] {
        life.step(Input::NewChat {
            chat,
            cwd: "/w".into(),
        });
        life.step(Input::Submit {
            chat,
            prompt: Prompt::text("hi"),
        });
    }
    let (a, b) = (life.chat(1).unwrap().agent, life.chat(2).unwrap().agent);
    assert!(a.is_some() && b.is_some() && a != b);
}

/// Found by review: Stop, Stop, Submit used to send a new prompt while fx was
/// still running the first one.
#[test]
fn a_second_stop_never_leaves_a_turn_running_behind_an_idle_chat() {
    let mut life = Lifecycle::new();
    life.step(Input::SignedIn {
        credential: "key".into(),
    });
    life.step(Input::NewChat {
        chat: 1,
        cwd: "/w".into(),
    });
    life.step(Input::Submit {
        chat: 1,
        prompt: Prompt::text("one"),
    });
    life.step(Input::BaselineTaken {
        chat: 1,
        error: None,
    });
    life.step(Input::SessionCreated {
        agent: 1,
        chat: 1,
        session: "s".into(),
    });
    life.step(Input::Stop { chat: 1 });
    let effects = life.step(Input::Stop { chat: 1 });
    assert!(
        effects.contains(&Effect::StopAgent { agent: 1 }),
        "{effects:?}"
    );
    let effects = life.step(Input::Submit {
        chat: 1,
        prompt: Prompt::text("two"),
    });
    assert!(effects.contains(&Effect::StartAgent {
        agent: 2,
        chat: 1,
        credential: "key".into()
    }));
    assert!(
        !effects
            .iter()
            .any(|e| matches!(e, Effect::Send { agent: 1, .. }))
    );
}
