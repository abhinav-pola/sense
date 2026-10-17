--------------------------- MODULE HistoryPaging ---------------------------
(***************************************************************************)
(* Paging one past session's transcript in through the librarian.         *)
(* Citations are crates/sense/src/..., line numbers as of the working    *)
(* tree on 2026-10-16 (the history code itself was unchanged).           *)
(*                                                                         *)
(* The session has N turns, numbered 1..N oldest to newest.  Pages hold   *)
(* one turn: page 0 (cursor None) is turn N, page k is turn N-k, and its  *)
(* `older` cursor is k+1 while k+1 < N.                                   *)
(*                                                                         *)
(* Fixed = FALSE models the current code; Fixed = TRUE the recommended    *)
(* fixes (see README).  Dup lets the librarian deliver a response twice   *)
(* (fx does not do this; it checks how much the code relies on that).     *)
(***************************************************************************)
EXTENDS Naturals, Sequences, FiniteSets

CONSTANTS N, MaxOpens, MaxRestarts, Fixed, Dup, AllowFail

Pages == 0..(N - 1)
Turn(p) == N - p
None == 99     \* "no page" / "no tag"
NoCache == [page |-> None, tag |-> None]

VARIABLES
    slot,       \* sidebar: "past" | "live" | "gone"
    inst,       \* ghost: which materialization the live view is (0 = never)
    hist,       \* ChatView.history: [st, k]; st in NotLoaded, LoadingFirst,
                \*   LoadingOlder, More(k = cursor), Complete, Failed (fixed only)
    shown,      \* turns the view shows, oldest first
    requested,  \* history_requested contains the session
    cache,      \* history_cache entry: [page, tag] or None
    inflight,   \* requests at the librarian: set of [page, tag, dup]
    lib,        \* librarian: "up" | "down"
    restarts,
    dirGone,    \* forget_chat's background removal of fx's session dir ran
    wrong       \* ghost: a page requested by an earlier view was shown

vars == <<slot, inst, hist, shown, requested, cache, inflight, lib, restarts,
          dirGone, wrong>>

H(st, k) == [st |-> st, k |-> k]
Loading == {"LoadingFirst", "LoadingOlder"}

Init ==
    /\ slot = "past" /\ inst = 0 /\ hist = H("NotLoaded", 0) /\ shown = <<>>
    /\ requested = FALSE /\ cache = NoCache /\ inflight = {}
    /\ lib = "up" /\ restarts = 0 /\ dirGone = FALSE /\ wrong = FALSE

(* workspace.rs:579-593 request_history: a no-op without a librarian,    *)
(* otherwise send and mark the session requested.  `tag` is a ghost: the *)
(* view instance that asked (0 = a hover preload).                       *)
Request(p, tag) ==
    IF lib = "up"
      THEN /\ inflight' = inflight \cup {[page |-> p, tag |-> tag, dup |-> FALSE]}
           /\ requested' = TRUE
      ELSE UNCHANGED <<inflight, requested>>

(* chat.rs:560-610 add_history_page: prepend, then More/Complete.        *)
Apply(p, tag) ==
    /\ shown' = <<Turn(p)>> \o shown
    /\ hist' = IF p + 1 < N THEN H("More", p + 1) ELSE H("Complete", 0)
    /\ wrong' = (wrong \/ (tag # 0 /\ tag # inst))

(* workspace.rs:597-606 preload, on hovering the row (1740).             *)
Hover ==
    /\ slot = "past" /\ cache = NoCache /\ ~requested
    /\ Request(0, 0)
    /\ UNCHANGED <<slot, inst, hist, shown, cache, lib, restarts, dirGone, wrong>>

(* select -> materialize (1340-1364) -> show_history (630-643):          *)
(* next_history_page(true) (chat.rs:625-629) sets LoadingFirst, then the *)
(* cache is used, or an in-flight request awaited, or a request sent.    *)
(* Fixed: without a librarian the view stays NotLoaded (and is requested *)
(* when one starts).                                                     *)
Open ==
    /\ slot = "past" /\ inst < MaxOpens
    /\ slot' = "live" /\ inst' = inst + 1
    /\ IF cache # NoCache
         THEN /\ shown' = <<Turn(cache.page)>>
              /\ hist' = IF cache.page + 1 < N THEN H("More", cache.page + 1)
                                               ELSE H("Complete", 0)
              /\ wrong' = (wrong \/ (cache.tag # 0 /\ cache.tag # inst + 1))
              /\ cache' = NoCache
              /\ UNCHANGED <<inflight, requested>>
         ELSE /\ shown' = <<>>
              /\ UNCHANGED <<cache, wrong>>
              /\ IF Fixed /\ lib = "down"
                   THEN /\ hist' = H("NotLoaded", 0)
                        /\ UNCHANGED <<inflight, requested>>
                   ELSE /\ hist' = H("LoadingFirst", 0)
                        /\ IF requested THEN UNCHANGED <<inflight, requested>>
                                        ELSE Request(0, inst + 1)
    /\ UNCHANGED <<lib, restarts, dirGone>>

(* ChatEvent::NeedOlderHistory, from add_history_page (chat.rs:608-610)  *)
(* or render (chat.rs:1341-1346) -> workspace.rs:1323-1329:              *)
(* next_history_page(false) moves More(k) to LoadingOlder, then request. *)
NeedOlder ==
    /\ slot = "live" /\ hist.st = "More"
    /\ (Fixed => lib = "up")
    /\ hist' = H("LoadingOlder", hist.k)
    /\ Request(hist.k, inst)
    /\ UNCHANGED <<slot, inst, shown, cache, lib, restarts, dirGone, wrong>>

(* workspace.rs:560-564 -> history_arrived (608-626): unmark; a view of  *)
(* the session that is Loading takes the page (whatever page it is);     *)
(* otherwise the page is cached.  Fixed: cache only for a past row.      *)
Deliver(r) ==
    /\ r \in inflight
    /\ inflight' = IF Dup /\ ~r.dup
                     THEN (inflight \ {r}) \cup {[r EXCEPT !.dup = TRUE]}
                     ELSE inflight \ {r}
    /\ requested' = FALSE
    /\ IF slot = "live" /\ hist.st \in Loading
         THEN /\ Apply(r.page, r.tag) /\ UNCHANGED cache
         ELSE /\ cache' = IF ~Fixed \/ slot = "past" THEN [page |-> r.page, tag |-> r.tag]
                          ELSE cache
              /\ UNCHANGED <<shown, hist, wrong>>
    /\ UNCHANGED <<slot, inst, lib, restarts, dirGone>>

(* workspace.rs:565-567 HistoryFailed: unmark only; a waiting view stays *)
(* Loading.  Fixed: the waiting view goes to Failed (notice + retry).    *)
Fail(r) ==
    /\ AllowFail /\ r \in inflight
    /\ inflight' = inflight \ {r}
    /\ requested' = FALSE
    /\ hist' = IF Fixed /\ slot = "live" /\ hist.st \in Loading
                 THEN H("Failed", IF hist.st = "LoadingFirst" THEN 0 ELSE hist.k)
                 ELSE hist
    /\ UNCHANGED <<slot, inst, shown, cache, lib, restarts, dirGone, wrong>>

(* Fixed only: the user retries a failed page.                           *)
Retry ==
    /\ Fixed /\ slot = "live" /\ hist.st = "Failed" /\ lib = "up" /\ ~requested
    /\ hist' = IF hist.k = 0 /\ shown = <<>> THEN H("LoadingFirst", 0)
                                             ELSE H("LoadingOlder", hist.k)
    /\ Request(hist.k, inst)
    /\ UNCHANGED <<slot, inst, shown, cache, lib, restarts, dirGone, wrong>>

(* delete_chat (1382-1388) -> core lib.rs:370-380 -> forget_chat (1394-):*)
(* the slot goes; history_cache/history_requested are left as they are.  *)
(* Fixed: forget_chat also drops the session from both.                  *)
Delete ==
    /\ slot = "live"
    /\ slot' = "gone"
    /\ IF Fixed THEN /\ cache' = NoCache /\ requested' = FALSE
                ELSE UNCHANGED <<cache, requested>>
    /\ UNCHANGED <<inst, hist, shown, inflight, lib, restarts, dirGone, wrong>>

(* forget_chat's detached remove_dir_all of fx's session dir (1421-1425).*)
RemoveDir ==
    /\ slot = "gone" /\ ~dirGone
    /\ dirGone' = TRUE
    /\ UNCHANGED <<slot, inst, hist, shown, requested, cache, inflight, lib, restarts, wrong>>

(* sign_out (1478-1480) or a new start_librarian (540-543): the old fx   *)
(* process is shut down; its in-flight history tasks die with it         *)
(* (sense-agent lib.rs:306, 404-430 spawn them on the connection).       *)
(* history_requested is not cleared.  Fixed: it is.                      *)
LibStop ==
    /\ lib = "up" /\ restarts < MaxRestarts
    /\ lib' = "down" /\ restarts' = restarts + 1
    /\ inflight' = {}
    /\ requested' = IF Fixed THEN FALSE ELSE requested
    /\ UNCHANGED <<slot, inst, hist, shown, cache, dirGone, wrong>>

(* start_librarian (540-576): ListSessions -> add_past_sessions (647-)  *)
(* re-adds a session that is neither a chat nor a row, if fx still has   *)
(* it on disk.  Fixed: a view left Loading/NotLoaded is requested again. *)
LibStart ==
    /\ lib = "down"
    /\ lib' = "up"
    /\ slot' = IF slot = "gone" /\ ~dirGone THEN "past" ELSE slot
    /\ IF Fixed /\ slot = "live" /\ hist.st \in Loading \cup {"NotLoaded"}
         THEN LET p == IF hist.st = "LoadingOlder" THEN hist.k ELSE 0
              IN /\ inflight' = {[page |-> p, tag |-> inst, dup |-> FALSE]}
                 /\ requested' = TRUE
                 /\ hist' = IF hist.st = "NotLoaded" THEN H("LoadingFirst", 0) ELSE hist
         ELSE UNCHANGED <<inflight, requested, hist>>
    /\ UNCHANGED <<inst, shown, cache, restarts, dirGone, wrong>>

Next ==
    \/ Hover \/ Open \/ NeedOlder \/ Delete \/ RemoveDir \/ Retry
    \/ LibStop \/ LibStart
    \/ \E r \in inflight : Deliver(r) \/ Fail(r)

Spec == Init /\ [][Next]_vars
        /\ WF_vars(\E r \in inflight : Deliver(r) \/ Fail(r))
        /\ WF_vars(LibStart) /\ WF_vars(RemoveDir)

TypeOK ==
    /\ slot \in {"past", "live", "gone"}
    /\ hist.st \in {"NotLoaded", "LoadingFirst", "LoadingOlder", "More", "Complete", "Failed"}
    /\ requested \in BOOLEAN

(* Shown turns are the newest Len(shown) turns, in order, no dups/gaps.  *)
ContiguousSuffix ==
    shown = [i \in 1..Len(shown) |-> N - Len(shown) + i]

(* A page asked for by one view is never shown in another.               *)
NoWrongChat == ~wrong

(* An open chat waiting for history gets an answer (a page, or in the    *)
(* fixed model a Failed state the user can retry); it is never stuck.    *)
WaitingResolves ==
    (slot = "live" /\ hist.st \in Loading) ~> (slot # "live" \/ hist.st \notin Loading)
=============================================================================
