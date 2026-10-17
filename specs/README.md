# TLA+ specs for sense's concurrent paths

| Spec | Models | Passing configs (`specs/*.cfg`) | Bug reproductions (`specs/bugs/*.cfg`) |
|---|---|---|---|
| `WorktreeThread.tla` | worktree thread creation, submit gating, folder picker, delete, worktree/branch removal | `WorktreeThread.cfg` (Mode `fixed` = recommended) | `current_orphan`, `current_picker`, `intended_midcreation`, `wip_picker_orphan` |
| `LibraryPersistence.tla` | async/sync library load, user edits, background saves at the file-system level (shared `.json.tmp`, open/truncate, write, rename) | `LibraryPersistence.cfg` (working tree: sync load + single saver), `LibraryPersistence_asyncload.cfg` (alternative fix) | `current_lost_edit`, `current_misfiled`, `current_corrupt`, `current_stale_write`, `current_not_persisted` |
| `HistoryPaging.tla` | hover preload, open, NeedOlderHistory, delivery/failure, delete, librarian stop/start, re-listing | `HistoryPaging.cfg` (Fixed = recommended) | `current_failed_page`, `current_librarian_restart`, `current_stale_cache`, `fixed_duplicates` (spec-only) |

Each action carries a `file:line` citation in the spec. The "current" models
are the code as first read on 2026-10-16. The library load/save code and the
worktree deletion code were changed in the working tree during that session;
the `wip`/`SyncLoad` variants model that working tree.

## Running

```
specs/check.sh              # every specs/*.cfg; non-zero exit on any violation
specs/check.sh --bugs       # also the bug reproductions (expected to fail)
specs/check.sh bugs/HistoryPaging_current_failed_page.cfg   # one config
```

Logs of failing runs (with the counterexample trace) are kept in `specs/.last/`.

Tooling lives in `.toolchain/` (gitignored), nothing system-wide:

- `.toolchain/jdk`: Eclipse Temurin 21.0.12.1+1, macOS aarch64, from
  `api.adoptium.net`; sha256 `3623232f33a9c3baadf304480b2535f9a3cba8a58d42ecbb438ba267315d9998`
  (matches the API's published checksum).
- `.toolchain/tla2tools.jar`: TLA+ tools v1.7.4 from GitHub releases (no
  published digest); sha256 `936a262061c914694dfd669a543be24573c45d5aa0ff20a8b96b23d01e050e88`.

To recreate:

```
mkdir -p .toolchain/jdk
curl -L https://api.adoptium.net/v3/binary/latest/21/ga/mac/aarch64/jdk/hotspot/normal/eclipse -o /tmp/jdk.tgz
tar -xzf /tmp/jdk.tgz -C .toolchain/jdk --strip-components=1
curl -L https://github.com/tlaplus/tlaplus/releases/download/v1.7.4/tla2tools.jar -o .toolchain/tla2tools.jar
```

## Properties

- WorktreeThread: `PromptsOnlyInWorktree`, `CheckoutNeverThreadFolder`,
  `NoOrphans` (once a deleted thread settles: no worktree, branch only if it
  has unpushed commits), liveness `DeletedWorktreeRemoved`, `CreationResolves`.
- LibraryPersistence: `NoEditLost`, `NoMisfiled`, `DiskIsSnapshot`, action
  property `DiskMonotone`, liveness `EventuallyPersisted` (`<>[]`).
- HistoryPaging: `ContiguousSuffix`, `NoWrongChat`, liveness `WaitingResolves`
  (an open chat waiting for history is never stuck in Loading).
