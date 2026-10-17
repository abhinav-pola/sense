--------------------------- MODULE WorktreeThread ---------------------------
(***************************************************************************)
(* One "new thread in a worktree" (ThreadMode::Worktree) from creation to *)
(* deletion.  Citations are crates/sense/src/...; "current" lines are    *)
(* as first read, the rest as of the working tree on 2026-10-16.          *)
(*                                                                         *)
(* Mode = "current"  : the code as first read: deleting never touches   *)
(*                     its worktree; the folder button stays enabled.     *)
(* Mode = "intended" : the announced change, implemented the obvious way: *)
(*                     forget_chat removes the thread's worktree (and its *)
(*                     branch if it has no unpushed commits).             *)
(* Mode = "wip"      : the engineer's working tree (2026-10-16):          *)
(*                     forget_chat removes the view's cwd if it is under  *)
(*                     the worktree root (workspace.rs:1411-1415,         *)
(*                     remove_worktree 110-117, worktree.rs:84-107 remove *)
(*                     --force, branch -D only if rev-list --not --remotes*)
(*                     is 0), and the creation callback removes the       *)
(*                     worktree if the view is gone and creation succeeded*)
(*                     (workspace.rs:1093-1099).  Folder picker still     *)
(*                     enabled (chat.rs:1430-1436).                       *)
(* Mode = "fixed"    : wip, plus: the folder picker is disabled for       *)
(*                     worktree threads and removal uses the recorded     *)
(*                     worktree path rather than the chat's current cwd.  *)
(***************************************************************************)
EXTENDS Naturals

CONSTANT Mode
ASSUME Mode \in {"current", "intended", "wip", "fixed"}

Folders == {"checkout", "worktree", "other"}

VARIABLES
    chat,       \* "none" | "live" | "deleted"
    folder,     \* lifecycle chat.cwd (sense-core lib.rs Chat.cwd)
    preparing,  \* ChatView.preparing: "no" | "creating" | "failed"
    task,       \* worktree::create task: "none"|"running"|"ok"|"err"|"handled"
    wt,         \* worktree dir on disk: "absent" | "present"
    branch,     \* the thread's branch: "absent" | "present"
    unpushed,   \* the branch has commits not on any remote
    bound,      \* baseline taken / session exists: SetFolder now ignored
    sent,       \* folders a prompt (or NewSession) was sent with
    picker,     \* folder dialog: "closed" | "open"
    removal     \* worktree removal task: "none" | "pending" | "done"

vars == <<chat, folder, preparing, task, wt, branch, unpushed, bound, sent,
          picker, removal>>

Init ==
    /\ chat = "none" /\ folder = "checkout" /\ preparing = "no"
    /\ task = "none" /\ wt = "absent" /\ branch = "absent"
    /\ unpushed = FALSE /\ bound = FALSE /\ sent = {}
    /\ picker = "closed" /\ removal = "none"

(* workspace.rs:1045-1086 new_thread: new_chat_in(project.path) (1059,   *)
(* 1024-1040) so cwd starts as the checkout, preparing set (1066-1068),  *)
(* worktree::create spawned on the background executor (1078-1080).      *)
NewThread ==
    /\ chat = "none"
    /\ chat' = "live" /\ folder' = "checkout" /\ preparing' = "creating"
    /\ task' = "running"
    /\ UNCHANGED <<wt, branch, unpushed, bound, sent, picker, removal>>

(* worktree.rs:22-41: fetch, then `git worktree add -b branch path`.      *)
CreateOk ==
    /\ task = "running"
    /\ task' = "ok" /\ wt' = "present" /\ branch' = "present"
    /\ UNCHANGED <<chat, folder, preparing, unpushed, bound, sent, picker, removal>>

(* worktree.rs:23 (fetch fails) or :28-40 (add fails): nothing created.  *)
CreateErr ==
    /\ task = "running"
    /\ task' = "err"
    /\ UNCHANGED <<chat, folder, preparing, wt, branch, unpushed, bound, sent, picker, removal>>

(* workspace.rs:1081-1104: back on the main thread.  If the view is gone *)
(* (1082-1084) the current code just returns.  Otherwise Ok dispatches   *)
(* Input::SetFolder (1088-1095), which sense-core lib.rs:309-316 applies *)
(* only before session/creating/baseline, then clears preparing         *)
(* (1096-1099); Err leaves preparing = "No worktree" (1101-1104).        *)
Callback ==
    /\ task \in {"ok", "err"}
    /\ task' = "handled"
    /\ IF chat = "live"
         THEN /\ IF task = "ok"
                   THEN /\ folder' = IF bound THEN folder ELSE "worktree"
                        /\ preparing' = "no"
                   ELSE /\ preparing' = "failed"
                        /\ UNCHANGED folder
              /\ UNCHANGED removal
         ELSE /\ UNCHANGED <<folder, preparing>>
              \* wip/fixed: the chat was deleted mid-creation; clean up here.
              /\ removal' = IF Mode \in {"wip", "fixed"} /\ task = "ok"
                               THEN "pending" ELSE removal
    /\ UNCHANGED <<chat, wt, branch, unpushed, bound, sent, picker>>

(* chat.rs:626-630 submit() returns while preparing.is_some(); otherwise *)
(* ChatEvent::Submit -> Input::Submit (workspace.rs:1325-1328) ->        *)
(* lib.rs:318-341: baseline taken, NewSession/Prompt carry chat.cwd      *)
(* (lib.rs:586-607).                                                     *)
Submit ==
    /\ chat = "live" /\ preparing = "no"
    /\ bound' = TRUE
    /\ sent' = sent \cup {folder}
    /\ UNCHANGED <<chat, folder, preparing, task, wt, branch, unpushed, picker, removal>>

(* The agent commits on the thread's branch; later the user may push.    *)
AgentCommits ==
    /\ chat = "live" /\ bound /\ folder = "worktree" /\ wt = "present"
    /\ unpushed' = TRUE
    /\ UNCHANGED <<chat, folder, preparing, task, wt, branch, bound, sent, picker, removal>>

Push ==
    /\ chat = "live" /\ unpushed /\ branch = "present"
    /\ unpushed' = FALSE
    /\ UNCHANGED <<chat, folder, preparing, task, wt, branch, bound, sent, picker, removal>>

(* chat.rs:1411-1420: the folder button is disabled only when a session  *)
(* exists or a turn is busy, not while preparing or for worktree threads.*)
OpenPicker ==
    /\ Mode # "fixed"
    /\ chat = "live" /\ ~bound /\ picker = "closed"
    /\ picker' = "open"
    /\ UNCHANGED <<chat, folder, preparing, task, wt, branch, unpushed, bound, sent, removal>>

(* chat.rs:713-727 -> ChatEvent::FolderPicked -> workspace.rs:1330-1333  *)
(* Input::SetFolder, applied under the same lib.rs:309-316 guard.        *)
PickerReturns(f) ==
    /\ picker = "open"
    /\ picker' = "closed"
    /\ folder' = IF chat = "live" /\ ~bound THEN f ELSE folder
    /\ UNCHANGED <<chat, preparing, task, wt, branch, unpushed, bound, sent, removal>>

(* workspace.rs:1403-1411 delete_chat -> lib.rs:370-380 DeleteChat ->    *)
(* Effect::ForgetChat -> forget_chat (1415-1452), which today removes    *)
(* only the snapshot and fx session dirs.                                *)
(* intended: also start removing the worktree.  fixed: only if creation  *)
(* has settled; otherwise the creation callback does it.                 *)
Delete ==
    /\ chat = "live"
    /\ chat' = "deleted"
    /\ removal' = CASE Mode = "current"  -> removal
                    [] Mode = "intended" -> "pending"
                    [] Mode = "wip"      -> IF folder = "worktree"
                                              THEN "pending" ELSE removal
                    [] Mode = "fixed"    -> IF task \in {"running", "ok", "err"}
                                              THEN removal ELSE "pending"
    /\ UNCHANGED <<folder, preparing, task, wt, branch, unpushed, bound, sent, picker>>

(* Intended background removal: `git worktree remove --force <path>`,    *)
(* then `git branch -d` unless the branch has unpushed commits.  Acts on *)
(* whatever is on disk when it runs.                                     *)
RemoveRun ==
    /\ removal = "pending"
    /\ removal' = "done"
    /\ wt' = "absent"
    /\ branch' = IF unpushed THEN branch ELSE "absent"
    /\ UNCHANGED <<chat, folder, preparing, task, unpushed, bound, sent, picker>>

Next ==
    \/ NewThread \/ CreateOk \/ CreateErr \/ Callback \/ Submit
    \/ AgentCommits \/ Push \/ OpenPicker \/ \E f \in Folders : PickerReturns(f)
    \/ Delete \/ RemoveRun

Spec == Init /\ [][Next]_vars
        /\ WF_vars(CreateOk \/ CreateErr) /\ WF_vars(Callback) /\ WF_vars(RemoveRun)

TypeOK ==
    /\ chat \in {"none", "live", "deleted"} /\ folder \in Folders
    /\ preparing \in {"no", "creating", "failed"}
    /\ task \in {"none", "running", "ok", "err", "handled"}
    /\ wt \in {"absent", "present"} /\ branch \in {"absent", "present"}
    /\ sent \subseteq Folders /\ removal \in {"none", "pending", "done"}

(* No prompt is ever sent while the thread's folder is not its worktree. *)
PromptsOnlyInWorktree == sent \subseteq {"worktree"}

(* Once a thread can act (not preparing) or has acted, its folder is not *)
(* the user's checkout.                                                  *)
CheckoutNeverThreadFolder ==
    (chat = "live" /\ (preparing = "no" \/ bound)) => folder # "checkout"

(* After a deleted thread's work settles no worktree it made remains,    *)
(* and its branch remains only if it holds unpushed commits.             *)
Settled == chat = "deleted" /\ task \in {"none", "handled"} /\ removal # "pending"
NoOrphans ==
    Settled => (wt = "absent" /\ (branch = "absent" \/ unpushed))

(* Liveness: a deleted thread's worktree is eventually gone.             *)
DeletedWorktreeRemoved == (chat = "deleted") ~> (wt = "absent")
(* Liveness: creation never leaves the thread stuck in "Creating".       *)
CreationResolves == (preparing = "creating") ~> (preparing # "creating" \/ chat = "deleted")
=============================================================================
