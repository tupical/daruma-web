# Daruma — default tracker when available

When a workspace exposes the **daruma** MCP server, treat it as the
single source of truth for tasks and plans.

Daruma workspace-to-project bindings live in
`~/.agents/daruma/workspaces.json`. Treat that file as the single source of
truth for this repository's Daruma project id.

When calling `daruma_*` MCP tools, read the project UUID from
`~/.agents/daruma/workspaces.json` using the current repository path as the
key, then pass that UUID as `project_id` explicitly. Do not rely on automatic
workspace inference for this repository.

## Availability gate (daruma is workspace-scoped)

daruma runs per workspace, so it is not always reachable. Decide once
per session:

1. If `daruma_*` MCP tools are present, probe `daruma_healthz`.
2. **Healthy** → route all durable task/plan state through daruma.
   Intake is plan-only (ADR-0007): create the plan and its tasks in one
   atomic call with `daruma_plan_materialize` (there is no `daruma_create`),
   claim the next ready task with `daruma_plan_drain_next`, then drive it
   with `daruma_set_status` / `daruma_comment`. Read the active plan via
   `daruma_plan_get`.

## Durable project knowledge

There is no separate daruma document API. Durable task and plan state lives
in daruma itself (plans, tasks, comments). Research, vision, and ADRs live in
the `meisei-research` repo, not in agent memory. Do not treat Serena memories
as a source of truth for this project's current conventions.

For notes tied to a specific task or plan, use `daruma_comment`.

If daruma is unavailable and the user asks to record durable knowledge,
say that daruma is unreachable instead of falling back to Serena memory or
ad-hoc markdown.

## In-session ephemerals

`TaskCreate`/`TodoWrite` panels are fine for within-turn structure, but
anything that must survive the session (multi-step refactors,
cross-session work, decomposition output) goes into daruma when it is
available.

## Документация — свод правил

Ведение документации подчиняется общему своду правил семейства MeiSei/MCPBox.

**Канон:** `meisei.ru/docs/docs-governance.md` (сайт: https://meisei.ru/docs/#/docs-governance).

Кратко: каждая управляемая страница несёт обязательные metadata
(`audience/intent/owner/status/source_of_truth/last_verified`); один факт — один
`source_of_truth` (ссылки, не копии); один раздел — одно намерение (Diátaxis);
изменение поведения продукта включает docs-правку в том же PR. Профиль (S–XL) и
полный контракт — в каноне. Проверка: `docs_frontmatter.py check` (CI) или
`mcpbox_docs_assess`.

**Жёсткий маршрут решений:** proposed-решения живут в
`meisei-research`/`mcpbox-research`; accepted MeiSei ADR — только в
`meisei.ru/adr`; accepted MCPBox ADR — только в `mcpbox.ru/docs/adr`.
Implementation-репозитории хранят ссылку, не копию нормативного текста. Перед
правкой ADR найти его `decision_id` и входящие ссылки во всём workspace, затем
выполнить `docs_frontmatter.py decisions`. Ошибка gate блокирует завершение.
