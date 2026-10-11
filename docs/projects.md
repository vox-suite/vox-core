# Projects

Projects hold context across many Plans. Start in Projects with either Import project or New project. Import creates a project, opens Sources, and accepts one or more local folders or up to five individual text/code files. New project opens the Context canvas and a separate Ideate panel. Engineering, Launch and 3D print templates seed open questions rather than fabricated answers.

## Existing project

1. Choose the repository folders in Vox desktop. The first import installs the pinned code-graph-mcp v0.169.0 executable, verifies its release checksum, and indexes a private snapshot with embeddings disabled.
2. Inspect the repository/module canvas, then open the local symbol list or relationship view. Citations open the actual approved file at its referenced lines. Static analysis may miss dynamic calls; missing edges never establish that code is unused.
3. Use Understand source to review a bounded selection of README/manifest excerpts before making one model request. The character cap is adjustable and the UI shows an approximate source-token count. Alternatively, enable relevant local excerpts for a specific ideation message.
4. Accept useful proposals as context. Edit, remove, restore earlier versions or add your own statements. Edits preserve history, and source refreshes preserve user-authored context.
5. Start a Plan inside the project or attach a stopped standalone Plan later. Each workflow task reads the current approved, non-stale context. Review a completed Plan's findings to propose context updates for explicit acceptance.

## Source lifecycle

Folder scanning respects gitignore and skips symlinks, generated directories, common credential files, binary content and files above 1 MB. Imports are bounded to 15,000 accepted files and 200 MB per source. Limits and unsupported files are reported in Sources. PDF/binary documents need conversion to text for this first adapter.

Sources and indexes live under the signed-in account's private `~/.vox/projects` directory. Source roots are never uploaded. Core stores summaries, relative citations, context history, messages and activity. Explicitly selected excerpts are sent for that one ideation request; they are not stored as a source corpus. Model-generated summaries can contain derived information from those excerpts.

While a project is open on desktop, source hashes are checked every 30 seconds. Changed or missing citations mark context possibly outdated; stale nodes are excluded from Plan context. Refresh updates the structural index. Manually reviewed claims can be marked current after their citations are read against the refreshed source. Sources can be relinked on another desktop; Android shows synced summaries and file references without reading desktop files.

## Android

Open Projects from Plans or the profile menu. Android supports creating and ideating, the Context/Plans/Sources/Activity views, editing context and project notes, version restoration, accepting/dismissing proposals, archiving, creating attached Plans, attaching standalone Plans and reviewing completed findings. The Sources tab explains which files live on desktop. Project lists use count/revision checks and an account-scoped device cache that clears at sign-out.

## Engine comparison

The first adapter runs code-graph-mcp's local CLI and reads its SQLite graph; it does not open a public MCP server. Native source access and the project model are separate from the engine-specific adapter. codebase-memory-mcp is not installed or selectable yet: it remains the next comparison engine after testing this implementation on a real project.

## Validation and limits

Validated locally with PostgreSQL migrations and authenticated CRUD, owner isolation, optimistic conflicts, pagination, count revisions, archiving and Plan attachment. Real code-graph-mcp indexed the desktop project and a mixed Rust/Python/C++/TypeScript fixture; incremental changes, deletion, ignore rules and outdated citations were exercised. Existing Core, desktop and Android checks ran without adding test files. Desktop source inspection and context editing were exercised in a browser preview with real database APIs and an adapter fixture made from real index output.

The native desktop app and Android debug APK were built. Successful ideation against a real provider, native picker interaction and Android hardware behavior still require signed-in user validation. The local API check used an intentionally invalid provider key and verified that failed requests do not persist partial messages. Publishing these commits does not by itself prove that the hosted API migration/deployment is ready.
