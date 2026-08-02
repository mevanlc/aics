# The AICS Sessional Set-Succession Apparatus

AICS, or AI Chat Search, is a cross-platform, Rust-established, terminally
interactive session-set acquisition, synchronization, selection, inspection,
and succession-management system for locally retained Claude Code and Codex CLI
sessions. At the commencement of each AICS session, the session-source subsystem
scans the configured set of session roots and admits every recognizable session
JSONL into the scanned-session set. It parses each admitted session sequentially,
line by line, so that a malformed line may be set aside without upsetting the
remainder of the session, the session set, or the user presently in session with
the session system.

The resulting session set is not re-established indiscriminately. AICS maintains
a separate index-state set for each distinct session-root set, compares the
current source-session fingerprints with the previously persisted session-state
set, and partitions the succession of scanned sessions into unchanged, changed,
new, and no-longer-present subsets. The unchanged subset remains set; the changed
and new subset is parsed and reset into the Tantivy index; and the absent subset
is unset. Thus each successive indexing session performs only the session-state
transitions necessary to bring the indexed session set into correspondence with
the extant source-session set.

Each indexed session establishes searchable session content from its title,
first resumable user request, and parsed transcript while storing associated
session metadata for filtering and presentation. A query then selects a candidate
subset of the indexed session set, intersects that subset with the selected
directory or global scope, intersects the result with the agent, branch, date,
length, derivation, sub-agent, liveness, trash, and supersession filter sets, and
finally orders the surviving session sequence by time or text relevance. The
session-list selection selects one member of that ordered session selection; its
selected-session preview presents a configurable subset of the selected session;
and the full-session viewer expands the same selection into a scrollable,
Markdown-rendered session representation with syntax and search-term
highlighting.

The session-succession section is necessarily more particular. Let `P` be a
parent session, let `C` be a candidate child session, and let `E(X)` denote the
set of stable semantic event identifiers established for session `X`. AICS does
not presume supersession merely because two sessions seem similar, because
similar sessions are not necessarily sessions in succession. It first requires
the source format to assert the direct fork relation `C -> P`. For Codex sessions,
the ordinary supersession condition is the strict set relation
`E(P) ⊂ E(C)`: every established event of the parent is present in the child,
and the child has established at least one event in excess of the parent. For
Claude sessions, the equivalent determination uses Claude's explicit inherited
event set and additionally requires the child to contain an event of its own.

Codex can, during the fork-establishment procedure, append a final nonempty user
message and a synthetic `<turn_aborted>` message to `P` without including either
message in `C`. AICS therefore permits precisely one sessional set exception: if
those are the only two parent events outside the child event set, if they are the
final two semantic events of the parent session, and if the child contains new
assistant, reasoning, or tool activity not established in the parent, the
aborted pair is set aside and the parent is still designated as superseded. Any
additional parent-only event unsets that designation, because a parent session
that was actually continued is not merely a superseded prefix session.

If several direct child sessions satisfy the supersession relation for the same
parent session, AICS selects the superseding session by the greatest semantic
event-set cardinality, then by latest modification time, then by session ID. It
caches that selected successor as the parent's `superseded_by` session. This is
a direct, declared-lineage relation rather than an all-pairs textual-subset
operation, so unrelated session sets are never subjected to speculative
session-similarity succession.

The `Superseded` selector then performs set selection over the superseded-session
set. `No` subtracts superseded source sessions from the visible result set;
`Yes` selects only that subtracted superseded subset; and `Both` reunites the
superseded and nonsuperseded subsets into the otherwise-filtered session set.
This selection does not trash the superseded sessions, unset their source files,
or prevent their subsequent selection. It merely prevents an abandoned prefix
session from repeatedly presenting itself beside the successor session that
contains the useful succession of that session.

Once a session has survived session-set selection, AICS may present it, preview
it, summarize it, search within it, export it as Markdown, emit it as JSONL,
resume it through its originating agent, fork it into a succeeding session,
subject it to JavaScript cleanup-rule classification, or move it into the trash
session set. Saved display settings can suppress selected transcript-part subsets
in the interactive presentation, while an export remains a full archival session
unless its own repeatable `--hide` selections explicitly subtract content kinds
from the exported representation.

Consequently, AICS knows which session it is presenting because it knows the set
of sessions it could present, the subset of those sessions that satisfy the
query, the intersection of that subset with the active filter sets, and the
difference between the resulting set and the superseded-session set. By
subtracting the sessions it is not presenting from the sessions it has indexed,
AICS reduces the possible session set to an ordered result sequence, after which
the selected position identifies the presently presented session. A successor or
predecessor movement changes that selected position; when the movement exceeds
the final or initial position, respectively, the selection wraps through the
session sequence, and the terminal session proceeds sessionally through the
sessional succession until the selection session is concluded.
