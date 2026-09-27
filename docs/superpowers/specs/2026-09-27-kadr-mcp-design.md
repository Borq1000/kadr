# Kadr MCP — design

Date: 2026-09-27 · Status: approved; revised the same day to reuse Slint's embedded MCP server (see §3.0)

## 1. Goal

Let Claude (Claude Code / Claude Desktop on the same machine) operate Kadr the
way a person does — see the window, read the project, edit with undo, import,
save, export, drive the UI — **and find input bugs** while doing it. Reference
points: the Figma, Blender and Unreal MCP servers.

Success means: every scenario debugged by hand in the 2026-09-26/27 sessions
(right-click context menu, unlink with `U`, click-to-narrow selection, drag
from the media library, Russian-layout shortcuts) can be reproduced and
verified by Claude through MCP alone, with no PowerShell/PostMessage scripts,
both in the user's open window and in a hidden instance.

## 2. Decisions taken with the user

| Question | Decision |
|---|---|
| Where Claude works | **Both**: attach to the running Kadr window; if none, start a hidden instance |
| Confirmations | **None**: Claude may save over files, export, remove media, run paid AI without asking. AI budget limits from Settings still apply |
| Architecture | **A**: bridge inside Kadr (127.0.0.1 + token) + separate stdio `kadr-mcp` |
| UI control | Semantic actions **plus real input** (pointer/keyboard through Slint's event routing) so input bugs are visible; window snapshot for sight |
| Sight + input layer | **Reuse Slint 1.18's embedded MCP server** (`slint/mcp`, `SLINT_MCP_PORT`) instead of writing our own; accepted that its port has no token (127.0.0.1 + Origin check only) |

## 3. Architecture

### 3.0 Two upstreams, one MCP server for Claude

Slint 1.18 ships an embedded MCP server (Streamable HTTP on
`127.0.0.1:$SLINT_MCP_PORT`, path `/mcp`) with `list_windows`,
`get_window_properties`, `get_element_tree`, `get_element_properties`,
`find_elements_by_id`, `query_element_descendants`, `take_screenshot`,
`click_element` (any button, single/double, modifiers), `drag_element`,
`hover_element`, `move_pointer`, `scroll_element`, `dispatch_pointer_scroll`,
`invoke_accessibility_action`, `set_element_value`, `dispatch_key_event`,
`start_event_recording`/`stop_event_recording`. Events go through Slint's real
routing (hit test, `TouchArea` grabs, `ContextMenuArea`, focus). Verified in a
spike on 2026-09-27: builds on Windows without `protoc`, answers `initialize`,
screenshots the off-screen window correctly. Element ids/types require the UI
to be compiled with debug info (`slint_build::CompilerConfiguration::with_debug_info(true)`).

`kadr-mcp` therefore aggregates two upstreams into one tool list:
**`ui_*`** tools proxied 1:1 to Slint's server (names prefixed `ui_`), and
**Kadr tools** (§4.3–4.4) served by our own bridge. Our bridge no longer
implements screenshots or pointer/keyboard input.

```
Claude ──stdio MCP──► kadr-mcp.exe ──HTTP JSON-RPC 127.0.0.1:<port>, Bearer token──► bridge thread in Kadr ──post(...)──► App (UI thread)
                          │                                                              ▲
                          └── no live instance → spawn `kadr.exe --headless`, wait for mcp.json ┘
```

### 3.1 `crates/mcp-bridge` (new, linked into the editor)

- Tiny HTTP/1.1 server on `127.0.0.1:0` (OS-chosen port) in its own thread,
  one request at a time (a single client is the supported case).
- On start writes `<data dir>/mcp.json`: `{ "port", "token", "ui_port", "pid", "started_ms", "headless" }`
  (`ui_port` = the Slint MCP port Kadr chose and exported as `SLINT_MCP_PORT`
  before creating the window);
  removes it on clean exit. Token: 32 random bytes, hex.
- Requests without `Authorization: Bearer <token>` → 401.
- Body: JSON-RPC 2.0 `{ "method": "<tool>", "params": {...} }`. The bridge
  knows nothing about tools: it hands `(method, params)` to a registered
  handler that runs on the UI thread and returns `Result<Value, BridgeError>`.
- UI-thread hop: `post(...)` + a oneshot channel; timeout 10 s by default
  (per-call override for `wait_idle`/export). A timeout reports what the UI
  thread is busy with if known (open modal, running export).
- Handler panics are caught (`catch_unwind`); the reply carries the panic
  message and the last 50 log lines; Kadr keeps running.
- Setting «Разрешить управление через MCP» / "Allow control via MCP"
  (default on). Off → bridge not started, no `mcp.json`.

### 3.2 `apps/editor/src/mcp_api.rs` (new)

Dispatch table `method → fn(&mut App, Value) -> Result<Value, BridgeError>`.
All edits go through `EditEngine` so they land in undo history; their labels
are prefixed «Claude: » / "Claude: ".

### 3.3 `apps/kadr-mcp` (new binary)

- MCP server over stdio (JSON-RPC 2.0, protocol `2025-06-18`), `tools/list` +
  `tools/call`; image results as MCP `image` content (PNG, base64).
- Instance discovery: read `<data dir>/mcp.json` (same `AppDirs` as the
  editor; `KADR_DATA_DIR` respected), check the PID is alive, ping.
  Missing/stale → spawn `kadr.exe --headless` (path: next to `kadr-mcp.exe`,
  overridable by `KADR_EXE`) and poll for `mcp.json` up to 15 s.
- A headless instance it started is terminated when the MCP session ends.
- `list_instances` / `use_instance` for several open windows (data dirs).

### 3.4 Headless mode (`kadr.exe --headless`)

A normal Kadr window placed off-screen, no taskbar button, never activated,
restore-prompt suppressed (it must not delete the user's autosave), autosave
to its own recovery file. Everything else is identical, so snapshots and input
behave as in the visible window.

### 3.5 In-window menus (`SLINT_NO_MUDA`)

Native Win32 context menus are modal (they block the UI thread), live in a
separate OS window (not in `take_snapshot`) and can't receive dispatched
input. Kadr sets `SLINT_NO_MUDA=1` at startup **always**, so context menus
render inside the Slint window: visible to Claude, clickable through real
input, and dark-themed like the rest of Kadr. The user sees the same menus
Claude tests.

## 4. Tools

Coordinates are logical window pixels (the snapshot's coordinate space).
Times are milliseconds on the sequence.

### 4.1 Sight

Screenshots and the element tree come from Slint (`ui_take_screenshot`,
`ui_get_element_tree`, …). Kadr adds what Slint can't know:

| Tool | Params | Returns |
|---|---|---|
| `get_state` | `detail?` (`summary`/`full`) | project name/path/dirty, sequence format, tracks with clips (id, name, kind, times, link, multicam, enabled, **screen rect**), selection, playhead, in/out, markers, zoom/scroll, open dialog/menu/popup, toasts, running jobs, AI panel messages |
| `get_log` | `lines?` (50), `level?` | recent log records; panics flagged |
| `get_frame` | `at_ms`, `max_w?` | composited preview frame PNG (same pipeline as the preview panel) |

### 4.2 Real input

All pointer and keyboard input is Slint's (`ui_click_element`,
`ui_drag_element`, `ui_move_pointer`, `ui_dispatch_key_event`, …). Kadr adds
one helper Slint lacks:

| Tool | Params | Purpose |
|---|---|---|
| `layout_text` | `text`, `layout` (`ru`/`en`) | returns the characters the same physical keys produce on the other layout, to feed `ui_dispatch_key_event` when testing layout-dependent shortcuts |

Known limit (documented in `kadr-mcp`'s instructions): this layer does not
exercise the OS → winit → Slint translation.

### 4.3 Semantic actions

| Tool | Purpose |
|---|---|
| `edit` | array of validated commands (the `kadr_ai::command` language plus `unlink`, `link`, `delete_part`, `add_track`, `set_clip_property`); one undo step |
| `undo` / `redo` | history |
| `select` | clip ids (replace/add), or `none` |
| `set_playhead` | `at_ms` |
| `playback` | `play`/`pause`/`stop`/`step` |
| `import_media` | file paths; waits until probed |
| `place_media` | asset/group id, `at_ms`, mode (`insert`/`overwrite`/`append`), track |
| `project` | `new`/`open`/`save`/`save_as` (path) |
| `export` | path, preset, range; waits (long timeout) and returns result |
| `ui` | `open_panel`/`close_dialog`/`menu` (menu action id)/`settings` (key/value) |
| `assistant` | send a chat prompt; returns the reply/plan cards |

### 4.4 Utility

`wait_idle` (jobs finished, no pending frame, UI settled; timeout),
`list_instances`, `use_instance`, `ping`.

## 5. Error handling

Structured `BridgeError { code, message, detail? }`; codes: `unauthorized`,
`unknown_method`, `bad_params`, `not_found`, `busy_timeout`, `panicked`,
`edit_rejected` (validator message), `io`. `kadr-mcp` maps them to MCP tool
errors with `isError: true` and a readable text. Kadr exiting mid-session →
`instance_gone`; the next call rediscovers or relaunches.

## 6. Security

Loopback only. Kadr's bridge: bearer token from a file in the user's data
dir. Slint's UI server: no token (Slint's design), loopback + Origin check;
accepted by the user. The setting "Allow control via MCP" disables **both**
(no `SLINT_MCP_PORT` exported, no bridge); no filesystem access beyond what the tools do (import
paths, save/export paths). No network listeners other than 127.0.0.1.

## 7. Testing

- **Unit:** JSON-RPC parsing, token check, error mapping, tool schemas.
- **Bridge integration:** `kadr-mcp` against a fake bridge: discovery, stale
  `mcp.json`, spawn + wait, timeouts.
- **Input layer (Slint's):** the regression proof is live, through
  `kadr-mcp`: with the pre-fix `timeline.slint` (commit before `2766f3a`)
  `ui_click_element` with `button: Right` on a clip opens no menu; on the
  current markup it opens one (visible in `ui_take_screenshot` and in
  `ui_query_element_descendants` for the menu items).
- **Live acceptance (by Claude through MCP only):** context menu + menu item
  click, `U` then click audio then `Del`, drag from the media library,
  Russian-layout shortcut, multicam angle key, screenshot/get_state
  consistency — once in the user's window, once headless.

## 8. Out of scope (v1)

Real OS input (`SendInput`), remote/network access, several simultaneous
clients, macOS/Linux specifics (code stays portable where free), an MCP
"resources" API (tools only).
