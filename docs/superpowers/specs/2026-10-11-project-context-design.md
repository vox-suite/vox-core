# Projects and local code context

The user authorized implementation of the product record and two entry flows: import an existing project to iterate with its context, or create a project and begin ideating in a separate panel.

Projects are independent long-lived entities, separate from collections and Plans. Desktop presents Context, Plans, Sources and Activity tabs. A persistent ideation panel can propose sourced context updates; accepting an update preserves the prior node version. User-authored nodes remain authoritative. Project templates seed goals and constraints without restricting the model.

Core stores names, notes, short context nodes, source labels, citations, conversation and activity. Local absolute paths, file contents and code graph indexes stay on the device. Explicitly enabled excerpts are bounded, read from approved sources, and sent only for an ideation request. Context nodes distinguish extracted structure, inferred claims and user statements. Changed citation hashes flag stale nodes; refreshing sources replaces extracted structure while retaining edited nodes.

Desktop uses a pinned code-graph-mcp native executable as its first index adapter. Sources are selected through a native picker and retained per authenticated account. Folder scanning respects gitignore, rejects symlinks, excludes secrets and generated folders, and reports limits. Indexing operates on a private local snapshot so it does not modify imported repositories. SQLite data is adapted into repository/module/file/symbol views with lazy drilldown. An adapter boundary separates installation, indexing and SQLite query behavior from shared project data so codebase-memory-mcp can be evaluated later without changing the project model.

Plans can be standalone or attached to an owned project. Their workflow reads current non-stale project context when each task starts; abandoned Plans stay hidden. A completed Plan can propose context updates for explicit acceptance. Existing branching and node history work must be preserved. User-facing Spaces and Pulse labels become Plans and Insights; API names remain compatible.

Validation uses build, type checking, lint, existing tests, a real engine smoke run, API/database checks where available and browser interaction. No code comments or new test files are added, honoring the user's instruction. Source review, local validation, deployed behavior and hardware validation are reported separately.
