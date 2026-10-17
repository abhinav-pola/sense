------------------------- MODULE LibraryPersistence -------------------------
(***************************************************************************)
(* The library (projects, folders, thread places) loaded after the first  *)
(* frame while the user may already be editing it, and saved by detached  *)
(* background tasks.  Citations are crates/sense/src/workspace.rs as     *)
(* first read (that load/save code has since been replaced, see below).  *)
(*                                                                         *)
(* The library is abstracted to the set of edits it reflects; every user  *)
(* edit here only adds, so "lacks something" is set inclusion.            *)
(*   "base"   : what library.json held at startup                         *)
(*   "proj"   : add_project (workspace.rs:1127-1137)                      *)
(*   "folder" : new_folder (1163-1169); stands for every edit that is not *)
(*              replayed after load: rename_folder (1185-1190), drop_thread*)
(*              /move_thread (1206-1246), delete_folder (1197-1207)       *)
(*   "thread" : a project thread whose fx session got its place           *)
(*              (SessionCreated, 766-774); "misfiled" is the same thread  *)
(*              recorded under a stale ProjectId.                          *)
(*                                                                         *)
(* Fixed = FALSE models the code as first read (load after the first     *)
(* frame, early_projects/early_places replay, one detached save task per  *)
(* save_library call).  Fixed = TRUE serializes saves and drops stale     *)
(* ones; with SyncLoad = FALSE it also replays every early edit and remaps*)
(* pending_place ids.  SyncLoad = TRUE models the engineer's working tree *)
(* (2026-10-16): Library::load before the first frame and one saver task  *)
(* that drains a channel to the newest library (workspace.rs new()).      *)
(***************************************************************************)
EXTENDS Naturals, FiniteSets, Sequences

CONSTANTS Fixed, SyncLoad, MaxSaves
ASSUME SyncLoad => Fixed

Items   == {"base", "proj", "folder", "thread", "misfiled"}
Inodes  == 0..MaxSaves
NoInode == 99   \* "no such file"
NotRead == {"#unread"}
Empty   == {"#empty"}     \* a truncated file
Corrupt == {"#corrupt"}   \* not valid JSON
Content == SUBSET Items \cup {Empty, Corrupt}

VARIABLES
    loading,     \* workspace.loading_library
    readVal,     \* what the background Library::load returned (NotRead = not yet)
    mem,         \* workspace.library
    early,       \* edits made while loading (early_projects/early_places, or all)
    made,        \* ghost: edits the user made
    threadEpoch, \* when new_thread ran: "none" | "loading" | "loaded"
    sessionDone, \* SessionCreated for that thread handled
    saves,       \* background save tasks: Seq of [snap, gen, pc, fd]
    content,     \* inode -> content
    libName,     \* inode named library.json
    tmpName,     \* inode named library.json.tmp, or NoInode
    lastGen,     \* fixed: generation last written
    snaps        \* ghost: every value mem has had

vars == <<loading, readVal, mem, early, made, threadEpoch, sessionDone, saves,
          content, libName, tmpName, lastGen, snaps>>

Disk == content[libName]
DiskSet(c) == IF c \in SUBSET Items THEN c ELSE {}

Init ==
    /\ loading = ~SyncLoad /\ readVal = NotRead
    /\ mem = IF SyncLoad THEN {"base"} ELSE {}
    /\ early = {} /\ made = {}
    /\ threadEpoch = "none" /\ sessionDone = FALSE
    /\ saves = <<>>
    /\ content = [i \in Inodes |-> IF i = 0 THEN {"base"} ELSE Empty]
    /\ libName = 0 /\ tmpName = NoInode /\ lastGen = 0
    /\ snaps = {{}, {"base"}}

(* workspace.rs:1301-1311 save_library: nothing while loading; otherwise *)
(* clone the library and spawn a detached background save.              *)
SaveLibrary(m) ==
    IF loading \/ Len(saves) >= MaxSaves
      THEN UNCHANGED saves
      ELSE saves' = Append(saves, [snap |-> m, gen |-> Len(saves) + 1,
                                   pc |-> "open", fd |-> NoInode])

(* A user edit: memory first, recorded for replay if loading, then saved. *)
Edit(item, replayed) ==
    /\ mem' = mem \cup {item}
    /\ made' = made \cup {item}
    /\ early' = IF loading /\ (replayed \/ Fixed) THEN early \cup {item} ELSE early
    /\ snaps' = snaps \cup {mem'}
    /\ SaveLibrary(mem')

AddProject ==                                  \* 1127-1137
    /\ "proj" \notin made
    /\ Edit("proj", TRUE)
    /\ UNCHANGED <<loading, readVal, threadEpoch, sessionDone, content, libName, tmpName, lastGen>>

NewFolder ==                                   \* 1163-1169: not replayed
    /\ ("proj" \in mem \/ ~loading) /\ "folder" \notin made
    /\ Edit("folder", FALSE)
    /\ UNCHANGED <<loading, readVal, threadEpoch, sessionDone, content, libName, tmpName, lastGen>>

(* new_thread (1045-1059) -> new_chat_in stores pending_place with the   *)
(* project's id *as of now* (1036).  No save yet.                         *)
NewThread ==
    /\ "proj" \in mem /\ threadEpoch = "none"
    /\ threadEpoch' = IF loading THEN "loading" ELSE "loaded"
    /\ UNCHANGED <<loading, readVal, mem, early, made, sessionDone, saves,
                   content, libName, tmpName, lastGen, snaps>>

(* SessionCreated (766-774): library.place with the pending ProjectId,   *)
(* recorded in early_places if still loading.  Ids made while loading    *)
(* come from the empty early Library (library.rs:62-65) and collide with *)
(* the loaded file's ids; library_loaded remaps early_places by path     *)
(* (1145-1157) but never remaps pending_place.                            *)
SessionCreated ==
    /\ threadEpoch # "none" /\ ~sessionDone
    /\ sessionDone' = TRUE
    /\ LET item == IF threadEpoch = "loading" /\ ~loading /\ ~Fixed
                     THEN "misfiled" ELSE "thread"
       IN /\ mem' = mem \cup {item}
          /\ early' = IF loading THEN early \cup {item} ELSE early
          /\ snaps' = snaps \cup {mem'}
          /\ SaveLibrary(mem')
    /\ made' = made \cup {"thread"}
    /\ UNCHANGED <<loading, readVal, threadEpoch, content, libName, tmpName, lastGen>>

(* 229-238: Library::load on the background executor; a damaged file     *)
(* loads as empty (library.rs:44-49).                                     *)
LoadRead ==
    /\ loading /\ readVal = NotRead
    /\ readVal' = DiskSet(Disk)
    /\ UNCHANGED <<loading, mem, early, made, threadEpoch, sessionDone, saves,
                   content, libName, tmpName, lastGen, snaps>>

(* 1141-1161 library_loaded: replays early_projects and early_places onto *)
(* the loaded library, replaces memory, then saves.                       *)
LoadApply ==
    /\ loading /\ readVal # NotRead
    /\ loading' = FALSE
    /\ mem' = readVal \cup early
    /\ early' = {}
    /\ snaps' = snaps \cup {mem'}
    /\ saves' = IF Len(saves) >= MaxSaves THEN saves
                ELSE Append(saves, [snap |-> mem', gen |-> Len(saves) + 1,
                                    pc |-> "open", fd |-> NoInode])
    /\ UNCHANGED <<readVal, made, threadEpoch, sessionDone, content, libName, tmpName, lastGen>>

(* Writing `new` through an fd at offset 0 over `old` (std::fs::write    *)
(* truncates only at open): a shorter write leaves the old tail behind.  *)
Overlay(old, new) ==
    IF old = Empty THEN new
    ELSE IF old = Corrupt THEN Corrupt
    ELSE IF Cardinality(new) >= Cardinality(old) THEN new ELSE Corrupt

SetPc(i, pc, fd) == saves' = [saves EXCEPT ![i].pc = pc, ![i].fd = fd]

(* library.rs:57-58 std::fs::write(library.json.tmp): open+truncate ...  *)
SaveOpen(i) ==
    /\ ~Fixed /\ saves[i].pc = "open"
    /\ IF tmpName = NoInode
         THEN /\ tmpName' = i /\ content' = [content EXCEPT ![i] = Empty]
              /\ SetPc(i, "write", i)
         ELSE /\ content' = [content EXCEPT ![tmpName] = Empty]
              /\ UNCHANGED tmpName
              /\ SetPc(i, "write", tmpName)
    /\ UNCHANGED <<loading, readVal, mem, early, made, threadEpoch, sessionDone, libName, lastGen, snaps>>

(* ... then write the bytes (to the inode, whatever it is now named).    *)
SaveWrite(i) ==
    /\ ~Fixed /\ saves[i].pc = "write"
    /\ content' = [content EXCEPT ![saves[i].fd] = Overlay(@, saves[i].snap)]
    /\ SetPc(i, "rename", saves[i].fd)
    /\ UNCHANGED <<loading, readVal, mem, early, made, threadEpoch, sessionDone, libName, tmpName, lastGen, snaps>>

(* library.rs:59 rename(tmp, library.json): moves whatever tmp names now;*)
(* fails (ignored, workspace.rs:1308) if another save already moved it.  *)
SaveRename(i) ==
    /\ ~Fixed /\ saves[i].pc = "rename"
    /\ IF tmpName # NoInode
         THEN /\ libName' = tmpName /\ tmpName' = NoInode
         ELSE UNCHANGED <<libName, tmpName>>
    /\ SetPc(i, "done", saves[i].fd)
    /\ UNCHANGED <<loading, readVal, mem, early, made, threadEpoch, sessionDone, content, lastGen, snaps>>

(* Fixed: saves hold one lock, write a private temp file and rename it,  *)
(* and skip a snapshot older than the one already written.               *)
SaveFixed(i) ==
    /\ Fixed /\ saves[i].pc = "open"
    /\ IF saves[i].gen > lastGen
         THEN /\ content' = [content EXCEPT ![i] = saves[i].snap]
              /\ libName' = i /\ lastGen' = saves[i].gen
         ELSE UNCHANGED <<content, libName, lastGen>>
    /\ SetPc(i, "done", NoInode)
    /\ UNCHANGED <<loading, readVal, mem, early, made, threadEpoch, sessionDone, tmpName, snaps>>

SaveStep(i) == SaveOpen(i) \/ SaveWrite(i) \/ SaveRename(i) \/ SaveFixed(i)

Next ==
    \/ AddProject \/ NewFolder \/ NewThread \/ SessionCreated
    \/ LoadRead \/ LoadApply
    \/ \E i \in 1..Len(saves) : SaveStep(i)

Spec == Init /\ [][Next]_vars
        /\ WF_vars(LoadRead) /\ WF_vars(LoadApply)
        /\ \A i \in 1..MaxSaves : WF_vars(i <= Len(saves) /\ SaveStep(i))

TypeOK ==
    /\ mem \subseteq Items /\ made \subseteq Items
    /\ content \in [Inodes -> Content] /\ libName \in Inodes

(* No user edit is lost from memory once the library has loaded.         *)
NoEditLost == ~loading => made \subseteq mem
(* No thread is filed under a project other than the one it was made in. *)
NoMisfiled == "misfiled" \notin mem

(* library.json always holds some library memory held (or the original). *)
DiskIsSnapshot == Disk \in snaps \cup {{"base"}}

(* library.json never loses anything an earlier write put there (checked *)
(* step by step, so with DiskIsSnapshot it covers the whole history).    *)
DiskMonotone == [][(Disk \in SUBSET Items) =>
                     (Disk' \in SUBSET Items /\ Disk \subseteq Disk')]_vars

(* Liveness: everything the user did ends up on disk, and stays.         *)
EventuallyPersisted == <>[](~loading /\ made \subseteq Disk)
=============================================================================
