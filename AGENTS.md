# TaskAgent — default tracker when available

When a workspace exposes the **taskagent** MCP server, treat it as the
single source of truth for tasks, plans, memory, and durable project docs.

TaskAgent workspace-to-project bindings live in
`~/.agents/taskagent/workspaces.json`. Treat that file as the single source of
truth for this repository's TaskAgent project id.

When calling `taskagent_*` MCP tools, read the project UUID from
`~/.agents/taskagent/workspaces.json` using the current repository path as the
key, then pass that UUID as `project_id` explicitly. Do not rely on automatic
workspace inference for this repository.

## Availability gate (taskagent is workspace-scoped)

taskagent runs per workspace, so it is not always reachable. Decide once
per session:

1. If `taskagent_*` MCP tools are present, probe `taskagent_healthz`.
2. **Healthy** → route all durable task/plan state through taskagent:
   read `project_id` from `~/.agents/taskagent/workspaces.json` →
   `taskagent_create` →
   `taskagent_plan_create` → `taskagent_plan_add_task` →
   `taskagent_set_status` / `taskagent_comment`. Read the active plan via
   `taskagent_plan_get` / `taskagent_plan_next_task`.

## Durable docs and memory

Do not write new project knowledge, conventions, or durable notes to Serena
memories. Use taskagent docs as the persistent project knowledge base:
`taskagent_doc_list`, `taskagent_doc_get`, `taskagent_doc_create`,
`taskagent_doc_append`, `taskagent_doc_replace`, and
`taskagent_doc_rename`.

When durable project knowledge matters, read taskagent docs before answering
or changing that knowledge:

1. Resolve the workspace with `taskagent_workspace_info`.
2. Read `project_id` from `~/.agents/taskagent/workspaces.json`.
3. List relevant docs with `taskagent_doc_list` using that `project_id`.
4. Fetch relevant bodies with `taskagent_doc_get`.
5. Only then create, append, replace, or rename docs.

Serena memories are not a source of truth for this project. Do not rely on
them for current conventions when taskagent docs are available.

For notes tied to a specific task or plan, use `taskagent_comment`.

If taskagent is unavailable and the user asks to record durable knowledge,
say that taskagent is unreachable instead of falling back to Serena memory or
ad-hoc markdown.

## In-session ephemerals

`TaskCreate`/`TodoWrite` panels are fine for within-turn structure, but
anything that must survive the session (multi-step refactors,
cross-session work, decomposition output) goes into taskagent when it is
available.
