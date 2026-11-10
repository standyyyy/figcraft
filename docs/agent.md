# The in-app Agent

FigCraft's embedded Agent works on the open design file from a chat panel: it can change the canvas
and write structured requirements documents. This page covers configuring it, what it may do, and
the limits that keep a model from taking the app down.

External MCP is separate and unaffected: it keeps the full tool set and its own transport. Both
surfaces read the same tool catalog, so a tool behaves the same either way.

## Configuring a provider

Preferences ▸ Agent:

| Field | Meaning |
|---|---|
| Enabled | Whether the panel is offered at all. |
| Base URL | An OpenAI-compatible endpoint, e.g. `http://127.0.0.1:8000` or `http://127.0.0.1:8000/v1`. Every shape normalizes to the same endpoint: `/v1` exactly once, a trailing slash, or a pasted `/v1/chat/completions`. |
| Model | The model name to ask for. Opaque to FigCraft. |
| Credential | The **name of an environment variable** holding the key, not the key. Leave empty for a local server that needs none. |
| Timeout | Seconds per request, 5–600. |
| Test connection | Sends one small request. |

### HTTP only, for now

The Agent speaks `http://`. `https://` is refused with a message saying so: adding a second TLS
backend for the Agent alone is not a change this feature makes. The workspace already ships one TLS
stack, and a later change can reuse it deliberately.

### The model name is opaque

A provider may answer under a different name than the one asked for — an alias, a quantised
variant, a routing target. FigCraft accepts whatever comes back and does not compare it to the
configured name.

## What the Agent may do

The Agent gets a **document-local, replayable** subset of the canvas tools:

```
list_commands  run_command  batch  create_node  set_properties
inspect_document  get_node  select  screenshot  type_text  inspect_ui
list_requirements  get_requirements  upsert_requirements  export_requirements_markdown
```

Everything else stays on the external MCP surface: opening and saving files, undo and redo,
sharing, plugin installation, live sessions, pointer and key injection, and window captures. Those
cannot be replayed as a document edit, so they are not advertised to the Agent, and the catalog
refuses them before they reach any command.

A model that guesses a denied name gets a readable refusal, not a run.

## One turn is one undo step

A turn runs its tool calls against a **shadow** copy of the document. Nothing reaches the live
document until the whole turn succeeds, and then the recorded commands are replayed as one
`command.batch`:

- the undo entry is named `AI: <your request>`;
- one Undo removes the whole turn, Redo brings it back;
- a cancelled, failed or read-only turn changes nothing.

The shadow records **canonical command ids** (`node.create`, not `create_node`), so a later change to
the tool catalog does not invalidate a recorded turn.

### Revision conflicts

If the document changes while the Agent works — you edit the canvas, or a collaborator does — the
commit is **refused** rather than replayed against a document the Agent never inspected. The panel
says so and offers to run the request again. Replaying stale commands would silently drop your edit,
which is the one thing the guard exists to prevent.

## Limits

Every dimension a model can run away in is bounded:

| Limit | Default |
|---|---|
| Model rounds per turn | 12 |
| Tool calls per turn | 40 |
| One tool's arguments | 256 KiB |
| One tool's result (fed back) | 512 KiB |
| One turn | 600 s |
| Transcript added by one turn | 8 MiB |

A tool **error** is not a failed turn: the message goes back to the model, which can try another
way. Only the turn's own limits, a provider failure, a cancellation or a revision conflict end it.

## Requirements documents

Requirements are ordinary design-file data, not chat state. They save and reopen with the file, take
part in undo, sync in a live session, and appear in branches and version history.

The Requirements tab lists the documents in the file and edits one as a form. Saving runs
`requirements.upsert`, so one save is one undo step and MCP sees the same document. Export renders
Markdown: the text comes back in the tool result, or is written through the engine's guarded file
access, so automation roots apply.

Caps bound every field and list (`32` documents, `200` items per list, 200–4000 characters per
field). An edit that breaks one is refused; a **file** that breaks one is repaired on open, because a
file written when a cap was larger must still open.

## Conversations

A transcript travels with the file as `agent/conversations.json`, capped on bytes, conversation
count, turns and decoded text. It is file metadata: it never enters `document.json`, undo history or
a live session, and a reader from before this feature ignores it.

A turn saved mid-flight reopens as **interrupted**, not running: its shadow is gone and its edits
were never committed, so a spinner would be a lie.

Long conversations are compacted before they reach a model's context. Compaction drops the oldest
turns first, always keeps the most recent complete turns, and records what it removed as one explicit
summary with how each turn ended. It uses a character estimate rather than a provider tokenizer,
because no OpenAI-compatible endpoint shares one.

## Browser

The browser build needs the endpoint to permit CORS. A refusal is reported as an unreachable
provider with that cause, not as a silent failure.

No JavaScript or TypeScript is involved: the web shell is the existing Rust one, and the Agent runs
there through the same host interface.

## Where the pieces live

| Crate | Responsibility |
|---|---|
| `figcraft-agent-tools` (L6) | The shared catalog: definitions, policy, validation, dispatch. |
| `figcraft-agent` (L6) | Rig gateway, model transport, turn loop, shadow session, lifecycle, history. |
| `figcraft-ui-egui` | The panel, its state machine, and the `AgentHost` interface. |
| `apps/figcraft` | The desktop worker thread behind `AgentHost`. |

The engine owns the provider preference **data** and never depends on `figcraft-agent`, so no Rig
type reaches a preferences file, and the credential is stored as a variable **name** that `Debug`
redacts.

## Running the live acceptance test

The deterministic suite never touches the network. To exercise a real endpoint:

```sh
FIGCRAFT_AI_BASE_URL=http://127.0.0.1:8111/v1 \
FIGCRAFT_AI_MODEL=DeepSeek-V4.1-Flash-1017 \
cargo test -p figcraft-agent --test live_vllm -- --ignored --nocapture
```

A key, when the endpoint needs one, is named by `FIGCRAFT_AI_API_KEY_NAME` and read from that
variable; no key is committed and none is printed. The test checks a text answer, a canvas tool call
with usable arguments, and a structured requirements document.

## What is not here

Figma-AI parity is not claimed from a chat-and-tools foundation. There is no image generation, no
design search, and no automatic layout suggestion. What exists is the mechanism: a model can call
the same tools MCP exposes, its changes land as one undo step, and the requirements it writes are
real document data.
