# Architecture and API contract

## Runtime boundary

Apell compiles Tauri natively for each host platform. Each applet ships a web frontend and may include a portable Rust WebAssembly backend. The backend runs in the host's Wasmi interpreter. Existing Tauri applications need to adapt native dependencies and command handlers to these two execution environments.

```text
Sandboxed applet view (HTML, CSS, JS, @apell/sdk)
  └─ MessagePort → trusted shell → verified native command
      ├─ direct host service call, owned by the current view
      └─ applet.invoke → applet's Wasmi backend
          └─ apell_guest::call → host service, owned by the backend run
```

| Location                   | Responsibility                                                                                                  |
| -------------------------- | --------------------------------------------------------------------------------------------------------------- |
| `src/`                     | Shell library, provider settings, applet view and message broker                                                |
| `src-tauri/`               | Native permissions, applet runs and views, package asset protocol, credentials                                  |
| `crates/apell-core/`       | Package validation and storage, GitHub release downloads, SQLite, HTTP, files, config, resources, LLM transport |
| `crates/apell-runtime/`    | Wasmi interpreter, guest actor, bounded messages and service dispatch                                           |
| `crates/apell-guest/`      | Rust guest ABI exports, async service calls and applet events                                                   |
| `packages/applet-sdk/`     | `@apell/sdk` TypeScript bridge, service helpers and backend command/event API                                   |
| `crates/apell-cli/`        | `apell pack` command for an independent applet repository                                                       |
| `examples/chatbot/`        | Companion frontend, manifest, and Rust backend                                                                  |
| `scripts/build-chatbot.ts` | Builds and packs Companion for the shell's Add sheet                                                            |

## Runs and views

On first launch, the host loads the installed package and creates a run scoped to its `applet_id`. When `lib` is present, it starts a Wasmi instance and invokes the guest's `initialize` command once, with a 30-second initialization deadline. The host attaches the first view after initialization succeeds. A frontend-only package also gets a run and a pinned package snapshot.

Apell keeps up to eight runs and one attached view in a process. **Close** detaches the current view and returns to the library. **Reload applet** replaces that view and its bridge. Opening a previously launched applet attaches a new view to its existing run. If the shell frontend reloads while a run is foreground, it can resume with a fresh view. The native instance and its service context remain available through these view changes. A running applet retains the package version with which it started. An installed update is used on that applet's next run, which requires an Apell restart if it already ran in the current process.

The host assigns independent opaque IDs to the run and view. A direct `hostCall` from the iframe uses the current view's service scope. Detaching it revokes that bridge, closes its SQLite handles and transactions, and cancels its LLM, HTTP, and resource work. Already-started local mutations finish before closure is acknowledged. Guest service calls use the backend run's scope and can complete while the view is detached. Both scopes access the same applet's durable data. A handle or transaction from one scope cannot be used in the other.

A guest command's reply is delivered to the requesting view while it remains attached. Losing that receiver does not cancel an accepted backend command. Applet events are transient and can be missed while detached. Companion subscribes before requesting its current snapshot and uses later events to refresh it. Its reply work and stop operation belong to the backend, which commits the finished or interrupted result to SQLite. When Apell terminates, in-memory runs and pending operations end. After restart, Companion initializes from SQLite and marks leftover pending rows interrupted. Device suspension and termination are subject to the operating system.

A trap, fuel exhaustion, or another terminal interpreter failure retires that run and closes its service scopes. The shell shows a stopped-backend message when attached. Close and reopen the applet to create a new run from the installed package; other healthy runs retain their state and pinned versions.

## Package format and installation

An `.apel` is a ZIP32 archive. `manifest.json` and a non-empty UTF-8 `index.html` are required at the root. Reference other frontend assets relative to the entry document. The optional `lib` field names a contained WebAssembly module under `lib/`. Companion declares its backend in this manifest.

```json
{
  "schema_version": 1,
  "api_version": 2,
  "applet_id": "chatbot",
  "name": "Companion",
  "version": "0.4.0",
  "permissions": ["sqlite", "llm"],
  "lib": "lib/main.wasm"
}
```

Applet IDs use up to 80 lowercase ASCII letters, digits, hyphens, or underscores. Package versions use semantic versioning. The host rejects unknown or duplicate permission groups. The available groups are `sqlite`, `llm`, `resources`, `device`, `http`, `fs`, and `config`. Each service checks the verified manifest and the host-assigned scope.

The parser accepts stored and deflated entries. Limits are 24 MiB of archive data, 64 MiB of actual expanded content, 1,024 ZIP records, and a 64 KiB manifest. It checks both directory records and decoded content, including CRC and declared sizes. Path traversal, absolute paths, symlinks, encryption, ambiguous entries, and case or Unicode normalization collisions are rejected. File names retain their original spelling for exact lookup. The declared backend is checked for a valid location and module header at install. Wasmi checks the complete module, imports, and ABI at launch.

`apell pack <directory> --output <file.apel>` validates the staging directory, fixes file order, timestamps, and permissions, and writes a deterministic archive. Keep the output outside the staging directory. A separate applet repository can consume the local `@apell/sdk` package and `apell-guest` crate from a pinned Apell checkout and install `apell` from the CLI crate. See the [independent applet build example](../README.md#create-and-distribute-an-applet).

For GitHub installation, the host resolves `owner/repository` or a repository URL to its numeric repository ID and latest stable public release. An explicit `owner/repository@tag` or `https://github.com/owner/repository/releases/tag/<tag>` URL selects that tag and accepts a published prerelease. Drafts remain unsupported. Tags are validated and encoded as API path segments; links cannot supply arbitrary download hosts. Repository-only lookup never falls back to prereleases. It lists uploaded `.apel` assets from the selected release, so the user can choose when several are available. On installation it rechecks the release ID and selected numeric asset ID supplied by the shell. A replaced asset requires rediscovery even if its name and release stayed the same. It downloads that asset ID, permits bounded HTTPS redirects to supported GitHub download hosts, and checks the asset size and GitHub SHA-256 digest when supplied. Release lookup and download use unauthenticated GitHub requests. Local file installation computes the SHA-256 in the host. These hashes identify content. Publisher signatures are outside this format.

Each installed version is immutable and named by its content hash. A single atomic pointer selects the active hash, applet ID, and installation source. A GitHub source binds the numeric repository ID, stable asset name, and optional explicit release tag to the applet ID. Updates re-resolve that selection and verify the binding under the installation lock before publication. A different repository, asset, or tag cannot silently replace that applet, and a release asset cannot take over an existing file install. Changed stable content requires a higher semantic version. A source already bound to a tagged prerelease accepts rebuilt bytes at the same version only when the incoming release is also a prerelease. This supports rolling nightly tags without relaxing stable releases. A developer may explicitly reinstall a local build at the same version. Neither source accepts a lower version. Installing changed local bytes over a GitHub applet makes it a file install. Reinstalling identical local bytes preserves its existing source. Existing sources without a saved tag continue following latest stable.

Old API-1 JSON bundles are read only from installed caches, with their HTML served as `index.html`. Fresh installations require the ZIP package and API revision 2. Updates keep databases, resources, files, and config under the existing applet ID. Version files are retained; the UI has no rollback or uninstall control.

## Applet isolation and bridge

The host serves verified package bytes through a custom `apel` protocol at a view-specific entry URL. Android and Windows use Tauri's local HTTP mapping for the protocol. Requests must name the attached view. The handler serves public package files and refuses `manifest.json`, `lib/`, and unsafe paths. It supplies MIME types, a restrictive content policy, `nosniff`, and disables caching. The iframe uses `sandbox="allow-scripts"` and has an opaque origin. The host bootstrap credential stays outside applet content.

The TypeScript SDK announces readiness. The shell verifies the sending iframe and its live view before transferring a dedicated `MessagePort`. It supplies the authoritative run and view IDs when forwarding calls. Direct API methods must be on the bridge allowlist. `hostCall<T>(method, params)` returns the result or rejects with a host error. Each message carries an envelope `request_id`; long-running services may also use their own request ID for progress and cancellation. The bridge permits up to 32 concurrent requests and rejects duplicate in-flight envelope IDs.

Use the TypeScript package's `applet.invoke(command, params)` to call an applet-defined backend command. `applet.onEvent(handler)` receives `{ name, payload }` events and returns an unsubscribe function. A frontend-only applet can call permitted host services directly. UI interactions such as `llm.chooseModel()` belong in the view.

```ts
import { applet } from '@apell/sdk';

const stop = applet.onEvent(() => void refresh());
async function refresh() {
  document.body.textContent = JSON.stringify(await applet.invoke('snapshot'));
}
void refresh();
window.addEventListener('pagehide', stop, { once: true });
```

The current view contract uses one SDK instance per document and `index.html` as its entrypoint. Use client-side routing for SDK-enabled frontends. A new document announcing readiness resets the view to the entrypoint; loading the SDK twice in one document also triggers that reset. Static H5 documents need no SDK handshake.

## Rust backend

Build a `cdylib` for `wasm32-unknown-unknown` and place it at the manifest's `lib` path. The `apell_guest::export!` macro exports the allocation, release, and dispatch functions expected by the host. The guest module exports its linear memory. This minimal guest returns a snapshot when the view requests one.

```rust
use apell_guest::HostError;
use serde_json::{json, Value};

apell_guest::export!(handle);

async fn handle(command: String, _params: Value) -> Result<Value, HostError> {
  match command.as_str() {
    "initialize" => Ok(Value::Null),
    "snapshot" => Ok(json!({ "status": "ready" })),
    _ => Err(HostError::new("Unknown command.")),
  }
}
```

`apell_guest::call` awaits a permitted native service. `call_with_events` also receives transient progress for that call. `emit(json!({"name": "changed", "payload": {}}))` sends an applet event, `spawn` schedules a guest future, and `create_id` and `now` obtain host-generated values. Native operations run asynchronously while guest code executes in serial interpreter turns. The guest receives no WASI filesystem or generic socket imports. Store persistent data through the applet's declared services.

The interpreter accepts modules up to 16 MiB and limits linear memory to 64 MiB. It meters each turn with 32 million fuel units, caps individual ABI messages at 8 MiB, and bounds outstanding commands and host calls. A trap or exhausted fuel fails the backend. These bounds are independent of each service's SQL, network, and storage limits.

## Native SQLite

Each opened handle has a dedicated native connection and belongs to the view or backend run that opened it. A validated database name resolves under the applet's private storage directory. The applet owns tables, indexes, migrations, and queries.

| Method            | Parameters                                                                     | Result                                  |
| ----------------- | ------------------------------------------------------------------------------ | --------------------------------------- |
| `sqlite.open`     | `database_name`                                                                | `{ database_handle }`                   |
| `sqlite.query`    | `database_handle`, `sql`, optional `parameters`, optional `transaction_handle` | `{ columns, rows }`                     |
| `sqlite.execute`  | Same as query                                                                  | `{ affected_rows, last_insert_row_id }` |
| `sqlite.begin`    | `database_handle`                                                              | `{ transaction_handle }`                |
| `sqlite.commit`   | `database_handle`, `transaction_handle`                                        | `null`                                  |
| `sqlite.rollback` | `database_handle`, `transaction_handle`                                        | `null`                                  |
| `sqlite.close`    | `database_handle`                                                              | `null`                                  |

Only one SQL statement is accepted per call. `query` accepts read-only statements; `execute` accepts statements without result columns. `RETURNING` is currently unsupported. Parameters are positional, and SQL values have explicit tags:

```ts
type SqlValue =
  | { type: 'null' }
  | { type: 'integer'; value: string }
  | { type: 'real'; value: number }
  | { type: 'text'; value: string }
  | { type: 'blob'; value: number[] };
```

Integers and `last_insert_row_id` use decimal strings to preserve signed 64-bit precision across JavaScript. Real values must be finite. Rows are arrays aligned with `columns`, preserving duplicate column names without object-key collisions. Binary values are byte arrays.

`begin` uses `BEGIN IMMEDIATE`. Every query or write on that database handle must then include its transaction handle. Transactions have a 30-second lifetime, with periodic expiry and automatic rollback when its owning scope closes. Keep transactions short and commit before awaiting an LLM request or download. Other connections encountering a write lock fail with a retryable busy error after a short wait.

`PRAGMA user_version` supports applet migrations. Safe schema introspection pragmas are allowed; attachment, extension loading, virtual tables, unsafe pragmas, and SQL transaction-control statements are denied. Foreign keys are enabled, temporary storage is memory-backed, and applets cannot choose filesystem paths. The separate `fs` service cannot access these database files.

The host limits SQL to 64 KiB, 256 parameters, 1 MiB of parameter data, 1 MiB SQLite values/rows, 1,000 result rows, and 4 MiB encoded query output. Queries have a two-second execution budget. Each view or backend run can open at most eight handles, with 64 across the service. Page growth is capped at 32,768 pages per connection. These are per-operation/database bounds, not a complete applet disk quota.

## Shared LLM access

Apell owns provider connections, credentials, model selection, transport, cancellation, and request attribution. Applets own prompts, complete message history and provider replay metadata, context selection, persistence, tool implementations, validation, and user review. The host never stores a conversation or executes an applet's tools. Provider setup stays in the shell's Settings screen.

| Method             | Parameters                                                                                                        | Result                                                                        |
| ------------------ | ----------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------- |
| `llm.config`       | Optional `target: { connection_id, model_id }`                                                                    | `{ model_id, mode, connection_id, provider_id, provider_name, capabilities }` |
| `llm.choose_model` | Optional `current: { connection_id, model_id }`                                                                   | `{ connection_id, model_id, provider_id, provider_name }` or `null` on cancel |
| `llm.generate`     | `request_id`, `messages`, optional `target`, `temperature`, `tools`, `tool_choice`, `stream`, `max_output_tokens` | `LlmResult` with an explicit final status                                     |
| `llm.cancel`       | `request_id`                                                                                                      | `null`                                                                        |

The SDK exports `llm.config(target?)`, `llm.chooseModel({ current? })`, `llm.generate(request, { signal?, on_event? })`, and `llm.cancel(request_id)`. It generates request IDs when omitted, scopes progress callbacks to one call, and forwards AbortSignal cancellation to the host. Rust/WASM clients may supply request IDs explicitly. The promise result is the sole authoritative final outcome. There is no legacy completion method or global LLM event subscriber API; applets must use the current contract.

`llm.chooseModel` opens a host-owned provider/model picker for applets with the `llm` permission. The user chooses a saved real provider connection and a discovered or manually entered model. Provider setup stays in Settings. The picker returns public provider labels and an opaque connection ID, while saved aliases, endpoints, and credentials stay in the host UI. Closing the picker returns `null`. Closing or reloading the applet cancels its picker.

An explicit generation target captures that saved connection and requested model for one request. Invalid, deleted, or synthetic targets fail with `invalid_target` and retain the requested target in provenance. Requests without a target use the shared active connection. Applets persist their chosen target with their own conversation state. Companion binds each chat to its target and starts a new chat when the user changes a populated conversation's provider or model. Legacy history is routed only when its saved attribution establishes a consistent real target.

### Messages, tools, and continuation

The applet message shape uses `role`, string or null `content`, and optional provider fields. Roles are `system`, `user`, `assistant`, and `tool`. Assistant messages may contain raw `tool_calls: [{ id, type: 'function', function: { name, arguments } }]`; tool messages require `tool_call_id`. Chat Completions metadata, including fields inside tool calls, is passed through unchanged. Native Responses/Messages replies include client-owned `provider_state` containing the complete native records needed for replay. Tool definitions remain `{ name, description?, parameters }`, where `parameters` is a JSON Schema object. `tool_choice` accepts `auto`, `none`, `required`, or `{ name }`. Every outstanding tool call must receive exactly one tool result before generating the next response.

A `tool_calls` outcome hands control to the applet. Append `result.message` unchanged, execute or reject the finalized calls in `result.tool_calls`, append their tool results, and generate again. Normalized result calls are a convenience for execution; they are not a replacement for the complete assistant message. A required or named choice that the provider silently ignores is an error. The host does not enforce application-specific schemas or execute partial tool-call fragments. See the [tool continuation example](llm.md).

The applet stores the complete assistant message with its history, including reasoning or signature fields required by its provider. Apell does not generate a continuation handle or keep a replay cache. Native `provider_state` includes a version, protocol, provider ID, non-secret connection ID, requested model ID, and complete output records. The host validates their binding and consistency with the message's display text/tool calls on replay; a mismatch fails explicitly. A restarted shell can send client-supplied history without recovering host conversation state. Context selection and the decision to rebuild history after a model switch belong to the client.

The `provider_state` field is reserved for this native replay record. Conflicting fields from Chat Completions providers return an explicit unsupported outcome with partial display text, rather than an unreplayable successful message.

Buffered Chat responses preserve complete assistant JSON, including unknown metadata and nested tool fields. Responses retains its authoritative final output items; Messages preserves native content blocks, including thinking/signature deltas. Unknown Chat metadata or Messages deltas whose merge semantics are unclear fail explicitly as unsupported. Clients can set `stream: false` to suppress progress and request buffered metadata where supported; Codex always streams upstream. There is no automatic retry that could repeat a generation. Metadata remains bounded as part of the request and result. Displaying reasoning transiently does not imply that client-persisted replay messages contain no reasoning.

### Outcomes, events, and attribution

Every `generate` outcome contains `request_id`, `status`, `content`, normalized `tool_calls`, nullable `message`, nullable `finish_reason`, `provenance`, and nullable `error`. Only `completed` and `tool_calls` return a replayable assistant `message`; interrupted outcomes retain partial display text with `message: null`. The native result is authoritative even if progress events arrive late or are missed.

| Status        | Meaning                                                                                       |
| ------------- | --------------------------------------------------------------------------------------------- |
| `completed`   | The provider explicitly finished the answer.                                                  |
| `tool_calls`  | Validated complete tool calls are ready for applet handling.                                  |
| `cancelled`   | The request was stopped; the answer is partial.                                               |
| `truncated`   | The provider reached its output limit; the answer is partial.                                 |
| `unsupported` | A requested capability or continuation format is explicitly unsupported.                      |
| `failed`      | Transport, protocol, validation, credential, refusal, or other failure; inspect `error.code`. |

An error contains a stable `code`, sanitized `message`, `retryable`, and an optional `capability`. Do not infer status from text or auto-retry a tool execution based on `retryable`. A dropped stream, missing completion reason, malformed tool call, content filter, or unknown finish reason cannot become a completed answer. `tool_calls` is a handoff, not a final answer. Session authorization, malformed bridge envelopes, and unavailable host transport may still reject the bridge promise with `HostError`; its `partial_content` includes answer deltas only.

Events carry `request_id`, `kind`, and `text`. `started` includes the captured configuration in `provenance`; `text_delta` carries answer text; `reasoning_delta` carries provider-exposed reasoning. No event announces final success or executable tool calls; await the returned result. Reasoning stays out of display `content` and the SDK's partial-answer buffer, but provider replay fields remain in a successful `message`. Providers exposing no reasoning still work. Request-scoped SDK callbacks are removed when the call settles.

`provenance` contains a non-secret `connection_id`, the captured `requested_model_id`, and nullable provider-reported `model_id`. Missing provider identity remains null; the requested alias is never passed off as the actual model. Save attribution from the result, not a prior `llm.config` read. Configuration and credential reads form one snapshot; settings changes cannot alter an in-flight request. Connection IDs are random host-generated values persisted across restarts. Selecting another model or rotating a key preserves the connection identity; switching endpoints creates a new identity. No endpoint or key is exposed to applets.

Capabilities contain `tools`, `required_tool_choice`, and `exposed_reasoning`, each `supported`, `unsupported`, or `unknown`. The current integration reports `unknown` for live connections and attempts the request; a model list or successful text probe does not prove feature support. Demo mode reports these features `unsupported`. A generic HTTP 400 is a provider error, not proof of unsupported tools; only recognized explicit unsupported-feature errors are classified that way. Capability discovery is advisory; every request has its own outcome and provenance.

### Provider settings

The shell stores up to 31 named provider connections plus Local demo, with one active connection/model shared by applets. Setup follows provider selection, authentication, model selection, then Save. Sign-in and model discovery operate on temporary drafts; final Save requires a model and commits the connection and its credentials together. Adding or editing a connection does not activate it. Activation requires a valid model and any required credentials. The original single connection is imported with its stable identity and secure-store reference. Existing invalid endpoints remain editable.

| Provider                             | Authentication   | Selected protocol                                            |
| ------------------------------------ | ---------------- | ------------------------------------------------------------ |
| OpenAI                               | API key          | Responses                                                    |
| Codex                                | Device sign-in   | Responses                                                    |
| GitHub Copilot                       | Device sign-in   | Responses, Messages, or Chat according to Tokn's model rules |
| OpenCode Go                          | API key          | Responses, Messages, or Chat according to Tokn's model rules |
| DeepSeek                             | API key          | Chat Completions                                             |
| Z.ai / Zhipu, including coding plans | API key          | Chat Completions                                             |
| llama.cpp                            | Optional API key | Chat Completions                                             |
| Custom compatible endpoint           | Optional API key | Chat Completions                                             |

Settings exposes connection CRUD, activation, model lookup, a cancellable generation test, and device sign-in. These are shell-only operations; applets cannot enumerate or administer connections. Model discovery uses the draft destination and current credential input with Tokn's provider headers and bounded response reads. Missing model-list support leaves manual model entry available. It does not consult Tokn's account files or disk catalogue. Testing a saved connection sends a short prompt through the same generation service and returns the actual outcome; it neither saves nor activates the connection.

New API-key connections show a key field, marked optional only for providers that allow keyless access. Existing credentials are never prefilled: a blank replacement field preserves the saved key, while removal is a separate action with Undo before saving. The native `keep`/`replace`/`remove` operations are derived from those user actions rather than exposed as a creation-form choice.

Codex and Copilot sign-in use Tokn's device-flow helpers. The shell retains the device handle in memory and exposes only its display code, approved HTTPS sign-in link, expiry, and an operation ID. The system browser opens only that stored provider link. Polling is bounded by the original challenge expiry and supports cancellation. Successful authorization stages access/refresh tokens and provider account identifiers in native memory; they reach the secure vault only on final Save. Refresh of a temporary account stays in memory, while refresh of an existing saved account persists rotated credentials before use.

The shell creates a draft lazily before authentication, discovery, or Save. Draft destinations are immutable; provider/endpoint edits discard the old draft and use a new one. Only an exact normalized destination match can inherit a saved credential. Closing setup discards the draft and cancels its authentication and discovery work. Drafts are bounded to four, expire after 30 minutes, and disappear on shell restart. Failed saves leave the draft available for retry; successful saves consume it. A changed source connection cannot be overwritten by a stale editor. Entered API keys apply to individual discovery requests without changing the draft's inherited credential, so clearing a replacement still preserves the saved key.

The shell-only draft operations are `llm.create_draft`, `llm.discover_draft_models`, and `llm.discard_draft`. `llm.begin_login` now takes a draft ID, `llm.poll_login` returns draft credential status without secrets, and `llm.save_connection` takes the draft ID, final name/model, and credential action. Applet LLM APIs are unchanged.

Credentials are isolated by host-generated connection identity, allowing multiple accounts at the same endpoint. Rename/model changes preserve identity; endpoint/provider changes create a new identity and require explicit credential replacement or removal. API-key edits use `keep`, `replace`, or `remove`, and vault writes roll back if saving connection metadata fails. Settings remains accessible when credential reads fail. Deleting the active connection selects Local demo; already-started generations retain their captured configuration and credential.

### Transport and limits

The native host embeds [Tokn's Rust SDK](https://github.com/tokn-ai/tokn) from Git `main`, with the resolved revision recorded in `Cargo.lock`. Each request builds an in-memory fixed-provider relay from the captured Apell configuration. Tokn's native driver helpers prepare provider-specific bodies and credentials; its converters handle portable text/tool messages while native replay records stay intact. Routing uses the exact configured model and endpoint prefix; it never selects another account/model or retries a generation automatically. The custom platform injects Apell's TLS client, disables request-event capture, and bounds response reads. It starts no gateway listener and loads no Tokn configuration or credential files.

Settings validates the same routing rules as generation. Provider hosts must be valid DNS names or IP addresses; trailing-dot DNS names and port zero are rejected. HTTP remains restricted to localhost and numeric loopback. A previously saved connection that no longer passes validation remains available in Settings for correction, without blocking shell startup or silently changing its endpoint. Invalid connections cannot send requests or be saved again unchanged.

The integration executes Chat Completions, Responses, and Messages through Tokn with SSE or buffered JSON and does not follow redirects. Apell retains SSE framing and semantic validation on opaque response bytes so client message metadata, provider-reported model identity, structured stream errors, and explicit finish reasons remain intact. At the integrated revision, Tokn's higher-level generation API cannot carry all required assistant metadata, so Apell uses its low-level execution path without maintaining host conversation state.

Direct Anthropic and Gemini drivers are absent from the integrated Tokn registry; support for Messages through Copilot/OpenCode does not imply a direct Anthropic connection. A service exposing compatible Chat Completions can use the custom connection. Schema-constrained answers and multimodal input remain outside this contract. Demo mode returns scripted local text without a model request.

Provider URLs require HTTPS, except loopback HTTP, and reject embedded credentials, query parameters, and fragments. Provider errors are sanitized before crossing the bridge. Provider, HTTP, and resource requests share a TLS policy: Android uses bundled Mozilla roots with rustls verification, while Apple platforms use native certificate trust. Android device-installed private certificate authorities are not loaded; updating its bundled roots requires rebuilding the shell.

Requests allow 1–256 messages, up to 64 tool definitions/calls, at most 512 KiB of encoded input including schemas and replay metadata, and temperature from 0 to 2. Optional `max_output_tokens` accepts 1–131072 and maps to the selected protocol's limit; actual model limits may be lower. Codex explicitly rejects that option because its upstream API does not support it. Output and replay metadata are bounded; generated output is capped at 1 MiB and raw provider responses at 8 MiB. The request timeout is 180 seconds, with a 60-second network read timeout. At most four requests run per view or backend service scope and 16 globally. The host rejects oversized context; it does not truncate history or summarize it for the applet.

Non-secret settings live in host application data. Credentials are scoped by connection identity and stored in Apple Keychain on iOS/macOS or encrypted with an Android Keystore key on Android. Android ciphertext is excluded from device backup. Applets cannot read keys, modify shared settings, or access a generic credential store. Secure provider credentials are currently implemented for iOS, Android, and macOS.

## Native HTTP

Declare the `http` permission and call `http.request` through the SDK helper or `hostCall`:

```ts
import { http } from '@apell/sdk';

const response = await http.request({
  url: 'https://example.com/api/items',
  method: 'POST',
  headers: [{ name: 'content-type', value: 'application/json' }],
  body: Array.from(new TextEncoder().encode(JSON.stringify({ item_id: 'one' }))),
  timeout_ms: 30_000,
});
const text = new TextDecoder().decode(new Uint8Array(response.body));
```

The request fields are `url`, optional `method` (default `GET`), `headers` (default empty), `body` (optional byte array), and `timeout_ms` (default 30,000). Supported methods are `GET`, `HEAD`, `POST`, `PUT`, `PATCH`, `DELETE`, and `OPTIONS`. The result is `{ status, headers, body }`. HTTP error statuses and redirects are returned normally; redirects are never followed. Transport failures, invalid input, and limits reject the promise.

Headers are arrays of `{ name, value }`, preserving repeated fields. Header values are byte strings: each character from U+0000 through U+00FF represents the corresponding byte; invalid HTTP control characters are rejected on input. Header names in responses are lowercase. Body bytes are integers from 0 to 255. Transport framing and connection headers are host-managed, including `Host`, `Content-Length`, `Transfer-Encoding`, `Connection`, `Upgrade`, `Proxy-Authorization`, `Proxy-Connection`, `Accept-Encoding`, `Keep-Alive`, `TE`, and `Trailer`.

URLs require HTTPS or loopback HTTP and reject embedded credentials and fragments. Queries are supported. Native networking is independent of browser CORS. This service has no cookie jar, adds no authorization, and cannot retrieve the shared provider key. An applet can supply its own authentication headers.

Each body is limited to 16 MiB; request headers to 64 fields and 32 KiB; response headers to 64 KiB; URLs to 8 KiB. Timeouts range from 100 to 120,000 milliseconds and cover the response body. The service allows four active requests across applets and returns a busy error when full. Closing the applet cancels its requests. The SDK allows 135 seconds for the bridge response. Streaming responses and individual request cancellation are not exposed yet.

## Applet files

Declare `fs` to use raw binary files under the applet's private directory. This directory is separate from config, databases, resources, bundles, and host credentials. Paths are relative to it; there is no absolute path API.

| Method      | Parameters                             | Result                                      |
| ----------- | -------------------------------------- | ------------------------------------------- |
| `fs.read`   | `path`, optional `offset` and `length` | `{ bytes, eof }`                            |
| `fs.write`  | `path`, `bytes`                        | `{ size_bytes }`                            |
| `fs.list`   | optional `path` (default empty)        | `{ entries: [{ name, kind, size_bytes }] }` |
| `fs.stat`   | `path`                                 | `{ kind, size_bytes }`                      |
| `fs.mkdir`  | `path`                                 | `null`                                      |
| `fs.remove` | `path`                                 | `null`                                      |
| `fs.rename` | `from_path`, `to_path`                 | `null`                                      |

The SDK exports matching `fs.read(path, options)`, `write(path, bytes)`, `list(path)`, `stat(path)`, `mkdir(path)`, `remove(path)`, and `rename(from_path, to_path)` helpers. `write` accepts a `Uint8Array` or byte array and atomically replaces the whole file; the parent directory must exist. `mkdir` creates missing parent directories. `remove` accepts files and empty directories. `rename` rejects an existing destination.

Reads default to offset zero and 65,536 bytes, with a 1 MiB maximum per call. Writes are limited to 16 MiB. `eof` indicates the end of the file, including reads beyond its end. Listings are sorted by name and reject directories with more than 1,000 entries. `kind` is `file` or `directory`; directory `size_bytes` is zero. An empty path addresses the root for `list` and `stat` only.

Paths reject absolute forms, empty components, `.`/`..`, backslashes, colons, control characters, and symbolic links. Only regular files and directories are supported. All paths are limited to 4,096 UTF-8 bytes, with 255 bytes per component. There is no append, recursive deletion, filesystem watcher, or aggregate disk quota. Data persists across session closure and bundle updates using the same applet ID.

## Applet configuration

Declare `config` to store small, non-secret JSON preferences independently of applet files and global LLM settings.

| Method          | Parameters     | Result                                         |
| --------------- | -------------- | ---------------------------------------------- |
| `config.get`    | `key`          | `{ found: false }` or `{ found: true, value }` |
| `config.set`    | `key`, `value` | `null`                                         |
| `config.delete` | `key`          | `{ deleted }`                                  |

The SDK exports `config.get(key)`, `set(key, value)`, and `delete(key)`. Values must be plain JSON with finite numbers and at most 64 nested objects or arrays. Non-finite numbers, `undefined`, custom objects, and cycles are rejected before serialization. A stored `null` is distinct from a missing key. Keys contain 1–128 UTF-8 bytes without control characters. Each encoded value is limited to 64 KiB, with at most 1,000 keys and 1 MiB of total encoded config per applet. Writes are atomic and concurrent updates are serialized. Invalid or oversized updates preserve the saved config. This API does not expose provider secrets or change shared host settings.

## Resources and device information

| Method               | Parameters                                    | Result                        |
| -------------------- | --------------------------------------------- | ----------------------------- |
| `resources.download` | `request_id`, `url`, optional `sha256`        | `{ resource_id, size_bytes }` |
| `resources.read`     | `resource_id`, optional `offset` and `length` | `{ bytes }`                   |
| `device.info`        | None                                          | `{ platform, api_version }`   |

Resource IDs are SHA-256 digests, and files are stored separately for each applet. Downloads require HTTPS or loopback HTTP, reject URL credentials, and do not follow redirects. Each download is limited to 32 MiB and 120 seconds. Reads return at most 1 MiB; the default length is 65,536 bytes and the default offset is zero. Reading beyond the end returns fewer bytes, including an empty array.

`onDownloadProgress` subscribes to `download_progress` events with `request_id`, `received_bytes`, and nullable `total_bytes`. The SDK allows 135 seconds for the download request, longer than the native transfer timeout. Resource data and bundles stay local. Resource deletion, eviction, overall disk quotas, and cloud synchronization are not implemented. Device information currently reports the platform and host API version; orientation, sharing, camera, and other mobile operations need separate host services.

## Migrating an existing Tauri app

Start with the existing frontend build and use relative asset URLs. Package its `index.html` and static assets. Replace Tauri frontend commands with direct SDK service calls, or with `applet.invoke` for applet-specific commands. Keep prompts, validation, migrations, and domain data in the applet. For a reader such as `read`, portable PDF rendering can stay in the frontend while annotations, schema migrations, and query logic move into the Rust guest.

To migrate a native command handler, separate its portable logic into a `wasm32-unknown-unknown` guest. Replace native filesystem access, `rusqlite`, and provider calls with the corresponding Apell services. Put database initialization and migrations in `initialize`. Let each new view request a snapshot of the running applet. Keep the same `applet_id` on code updates. The applet is responsible for schema compatibility and recovery from interrupted work.
