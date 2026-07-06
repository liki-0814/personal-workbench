# pwcli v2 Runtime Boundary

This crate starts with a small, reusable runtime surface. It intentionally does
not implement concrete tools or memory yet.

## Core Boundary

User input flows through:

```text
context builder
-> tool selection
-> ToolRegistrySnapshot
-> graph run
-> policy guard per ToolCall
-> tool execution
-> audit record
```

`graph` does not scan skills, connect MCP servers, or mutate the live registry.
It receives a `ToolRegistrySnapshot` at run start and uses that snapshot until
the run ends.

## Tools

`tools` is the unified capability layer. A tool can come from:

- builtin code
- external Agent Skill directories
- MCP servers
- verification checks
- model providers used internally by graph nodes

Concrete tool implementations can be added independently by implementing
`ToolLoader` and registering `LoadedTool` values.

The base runtime includes model providers under `tools/model`:

- OpenAI Responses API streaming via `OPENAI_API_KEY`
- Anthropic Messages API streaming via `ANTHROPIC_API_KEY`
- text delta, thinking delta, usage, and done events
- `ThinkingConfig` mapped to OpenAI reasoning and Anthropic thinking payloads

Model providers are used by graph model nodes and planners. They are not exposed
as ordinary model-callable tools.

## Skills

Skills are external directories and are not copied into this repo:

```text
skill-name/
├── SKILL.md
├── scripts/
├── references/
├── assets/
└── agents/
```

`SKILL.md` is the manifest. Its YAML frontmatter is parsed and unknown fields
are preserved as metadata. Missing `name` falls back to the directory name.
Missing `description` falls back to the first markdown paragraph and records a
load warning.

Prompt skills are the default. Executable skills require explicit frontmatter:

```yaml
executable:
  command: ["python3", "${skill_dir}/scripts/run.py"]
```

Executable skills use JSON stdin/stdout: pwcli writes `ToolCall` JSON to stdin
and expects `ToolResult` JSON on stdout.

## Reload Semantics

A long-running runtime may watch or poll skill roots and rebuild the live
`ToolRegistry`. Reloading increments the registry version. Active graph runs
continue with their existing snapshot; new runs receive the new version.

## Policy And Audit

Every tool call must pass through `PolicyGuard` before execution. Decisions are:

- `Allow`
- `Deny`
- `AskUser`

Every graph run records audit events for the registry version, selected tools,
tool calls, policy decisions, tool results, and run completion.

## CLI

The initial CLI supports:

```text
pwcli init
pwcli tools list
pwcli run <prompt>
```

`run` loads settings, discovers external skills, builds a context pack, creates
a registry snapshot, calls the configured model provider with streaming output,
and writes audit events to `~/.pwcli/audit/events.jsonl`.
