# Architecture and API contract

## Runtime boundary

```text
Applet: HTML / TypeScript + optional Rust WASM
  → SDK MessagePort requests
  → trusted shell frontend
  → authenticated native Tauri commands
  → SQLite, LLM providers, HTTP, files, config, resources, device information
```

Tauri is compiled natively for the host platform. An applet contains portable code and calls host services asynchronously. Existing Tauri apps need adapters and separation of native dependencies; there is no general compilation of an arbitrary Tauri application into WASM.

| Location                   | Responsibility                                                                                    |
| -------------------------- | ------------------------------------------------------------------------------------------------- |
| `src/`                     | React shell, applet library, shared model settings, iframe lifecycle, message broker              |
| `src-tauri/`               | Tauri commands, session permissions, host persistence, native credential integration              |
| `crates/apell-core/`       | Bundle validation and installation, native SQLite, files/config/resources, HTTP and LLM transport |
| `packages/applet-sdk/`     | TypeScript request transport, service types/helpers, event subscriptions                          |
| `examples/chatbot/`        | Sample interface and portable Rust conversation, schema, persistence, and prompt logic            |
| `scripts/build-chatbot.ts` | WASM build and self-contained bundle packaging                                                    |

The library presents an adaptive grid of applet icons, with installation in an Add sheet and shared model settings on a separate screen. Native window sizes are configured only for desktop; mobile uses device bounds, and the UI accounts for safe areas and the visible keyboard viewport. iOS declares a single scene in `Info.ios.plist` so UIKit supplies orientation-aware window geometry. Its WebView disables automatic scroll insets; the shell applies CSS safe-area padding once, including around the applet iframe.

The UI exposes one foreground applet. Its host-owned header has a top-right menu and close capsule; the menu exposes reload and applet information. Each launch creates a fresh host-owned session; durable data is scoped to its stable `applet_id`. Reload suspends the iframe, closes the old session, and mounts a fresh iframe keyed by the replacement session ID. Failed reloads keep the old iframe suspended and offer retry or close. Closing a session invalidates its SQLite and LLM registrations, rolls back open transactions, and cancels active LLM requests, HTTP requests, and resource downloads. Local file/config operations already in progress finish before session closure is acknowledged; queued calls cannot write afterward. Applets never supply their own authoritative identity to a storage or provider service.

## Applet isolation and installation

The shell loads the installed HTML into an iframe with `sandbox="allow-scripts"`. It has an opaque origin, a restrictive content policy, and no same-origin access to the shell. The SDK announces readiness; the shell verifies the iframe's window before transferring a dedicated `MessagePort`. The shell attaches the session identity to forwarded requests. A host bootstrap secret authorizes native commands and stays outside applet content.

Manifest permissions select whole service groups: `sqlite`, `llm`, `resources`, `device`, `http`, `fs`, and `config`. Each service checks its permission in the native host before dispatch. Applets declare only the groups they use; the chatbot still requires only `sqlite` and `llm`. The broker additionally limits requests to known methods and caps concurrent calls. This is a boundary for trusted personal applets, not a third-party app store or a claim of complete hostile-code containment.

Bundles use this format:

```json
{
  "manifest": {
    "schema_version": 1,
    "api_version": 1,
    "applet_id": "chatbot",
    "name": "Companion",
    "version": "0.1.0",
    "permissions": ["sqlite", "llm"]
  },
  "html": "<html>...inline CSS, JavaScript, and embedded WASM...</html>"
}
```

Applet IDs contain at most 80 lowercase ASCII letters, digits, hyphens, or underscores. The current host accepts only schema/API version 1 and rejects unknown or duplicate permissions. The sample embeds WASM bytes as base64 and instantiates them directly, so it needs no network fetch inside the sandbox.

Installation verifies the expected SHA-256 before parsing or activation. A remote checksum must come from a trusted source; the native host computes the checksum for local bundle content selected by the shell. Local installs may supply an expected checksum, which is still enforced; URL installs always require one. Neither flow authenticates a publisher. Verified bytes are stored in immutable files named by their checksum. An atomic active-version pointer selects the installed version. Reads verify integrity again and keep the manifest and checksum from one snapshot. Invalid updates preserve the current active version.

There is no code-signature infrastructure, version ordering, or schema downgrade management. Old version files remain available on disk but have no management UI yet.

## Bridge contract

`hostCall<T>(method, params)` in the applet SDK returns a promise. Request and response envelopes use snake_case fields:

```ts
type Request = {
  type: 'request';
  request_id: string;
  method: string;
  params: Record<string, unknown>;
};
type Response =
  | { type: 'response'; request_id: string; result: unknown }
  | { type: 'response'; request_id: string; error: string };
type Event = { type: 'event'; event: unknown };
```

The envelope `request_id` correlates a bridge call. Long-running operations also carry their own `request_id` inside `params`, which correlates progress and cancellation. Bridge errors reject the request promise; `llm.generate` additionally returns explicit terminal outcomes for provider and generation failures. The shell rejects duplicate in-flight envelope IDs and allows at most 32 concurrent calls per bridge.

WASM uses the same asynchronous transport. The chatbot's `bridge.rs` adapts `apellHostCall` into Rust futures with `wasm-bindgen-futures`; `serde-wasm-bindgen` maps serialized request and result values. The applet has no direct SQLite file access or provider credential access.

## Native SQLite

Each opened handle has a dedicated native connection and belongs to one applet session. A validated database name resolves under the applet's private storage directory. The applet owns tables, indexes, migrations, and queries.

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

`begin` uses `BEGIN IMMEDIATE`. Every query or write on that database handle must then include its transaction handle. Transactions have a 30-second lifetime, with periodic expiry and automatic rollback on session closure. Keep transactions short and commit before awaiting an LLM request or download. Other connections encountering a write lock fail with a retryable busy error after a short wait.

`PRAGMA user_version` supports applet migrations. Safe schema introspection pragmas are allowed; attachment, extension loading, virtual tables, unsafe pragmas, and SQL transaction-control statements are denied. Foreign keys are enabled, temporary storage is memory-backed, and applets cannot choose filesystem paths. The separate `fs` service cannot access these database files.

The host limits SQL to 64 KiB, 256 parameters, 1 MiB of parameter data, 1 MiB SQLite values/rows, 1,000 result rows, and 4 MiB encoded query output. Queries have a two-second execution budget. There are at most eight open handles per session and 64 across the service. Page growth is capped at 32,768 pages per connection. These are per-operation/database bounds, not a complete applet disk quota.

## Shared LLM access

Apell owns provider connections, credentials, model selection, transport, cancellation, and provider-specific metadata. Applets own prompts, history and context selection, persistence, tool implementations, validation, and user review. The host never executes an applet's tools or interprets their domain meaning. Provider setup stays in the shell's Settings screen.

| Method         | Parameters                                                               | Result                                                     |
| -------------- | ------------------------------------------------------------------------ | ---------------------------------------------------------- |
| `llm.config`   | None                                                                     | `{ model_id, mode, connection_id, capabilities }`          |
| `llm.generate` | `request_id`, `messages`, optional `temperature`, `tools`, `tool_choice` | `LlmResult` with an explicit terminal status               |
| `llm.cancel`   | `request_id`                                                             | `null`                                                     |
| `llm.complete` | Legacy text-only request                                                 | `{ request_id, content }` on completion; rejects otherwise |

The SDK exports `llm.config()`, `llm.generate(request)`, `llm.cancel(request_id)`, and `onLlmEvent(handler)`. New clients should use `generate`. The legacy `complete` method retains its success/rejection contract so installed applets cannot mistake a resolved partial outcome for a completed answer. It does not accept tools or continuation handles.

### Messages, tools, and continuation

Messages contain a `role` and string `content`. Roles are `system`, `user`, `assistant`, and `tool`. Assistant messages may contain `tool_calls: [{ id, name, arguments }]` and `continuation_id`; tool messages require `tool_call_id`. Tool arguments remain JSON text for the applet to parse and validate. Tool definitions are `{ name, description?, parameters }`, where `parameters` is a JSON Schema object. `tool_choice` accepts `auto`, `none`, `required`, or `{ name }`. Every outstanding tool call must receive exactly one tool result before generating the next response.

A `tool_calls` outcome ends the current provider turn and hands control to the applet. Append the returned assistant content, calls, and continuation handle unchanged, execute or reject the proposed tools in applet code, append their tool results, and generate again. A required or named choice that the provider silently ignores is an error. The host does not enforce application-specific schemas or execute partial tool-call fragments. See the [tool continuation example](llm.md).

The opaque `continuation_id` belongs to one assistant message. It preserves supported provider-private metadata without exposing provider-specific fields to applets. Pass it back on that same message whenever retaining it in the next request's context, including older assistant messages. Handles bind the applet session, connection, requested model, and exact assistant content/tool calls. Expired, evicted, altered, or mismatched handles fail explicitly; Apell never silently drops their metadata or switches the continuation to another connection.

Continuation data stays in memory, never in applet storage or host logs. It expires after 30 minutes, is cleared on applet close/reload, and is bounded to 16 entries per session, 64 globally, and 8 MiB of private metadata. Applets should keep handles transient and start a fresh context after expiry. The first adapter preserves `reasoning_content` and `reasoning` fields, plus complete `reasoning_details` from buffered JSON. Streaming `reasoning_details` and other metadata formats it cannot preserve losslessly fail as unsupported rather than being discarded. This is not a provider-independent durable conversation store.

### Outcomes, events, and attribution

Every `generate` outcome contains `request_id`, `status`, `content`, `tool_calls`, nullable `continuation_id`, nullable `finish_reason`, `provenance`, and nullable `error`. The native result is authoritative for partial answer text even if events arrive late or are missed.

| Status        | Meaning                                                                                       |
| ------------- | --------------------------------------------------------------------------------------------- |
| `completed`   | The provider explicitly finished the answer.                                                  |
| `tool_calls`  | Validated complete tool calls are ready for applet handling.                                  |
| `cancelled`   | The request was stopped; the answer is partial.                                               |
| `truncated`   | The provider reached its output limit; the answer is partial.                                 |
| `unsupported` | A requested capability or continuation format is explicitly unsupported.                      |
| `failed`      | Transport, protocol, validation, credential, refusal, or other failure; inspect `error.code`. |

An error contains a stable `code`, sanitized `message`, `retryable`, and an optional `capability`. Do not infer status from text or auto-retry a tool execution based on `retryable`. A dropped stream, missing completion reason, malformed tool call, content filter, or unknown finish reason cannot become a completed answer. `tool_calls` is a handoff, not a final answer. Session authorization, malformed bridge envelopes, and unavailable host transport may still reject the bridge promise with `HostError`; its `partial_content` includes answer deltas only.

Events carry `request_id`, `kind`, and `text`. `started` includes the configuration snapshot in `provenance`; `delta` carries answer text; `reasoning_delta` carries only provider-exposed reasoning; `tool_call` carries a finalized call; `terminal` includes the authoritative `result`. Use the final result's status before executing tools or saving a completed answer. Reasoning is transient event data, absent from final result content and the SDK's partial-answer buffer. It is not reconstructed if events are missed. Providers exposing no reasoning still work. Legacy `complete` emits only `delta` and successful `done` events. Remove event subscriptions when leaving the applet's view.

`provenance` contains a non-secret `connection_id`, the captured `requested_model_id`, and nullable provider-reported `model_id`. Missing provider identity remains null; the requested alias is never passed off as the actual model. Save attribution from the result, not a prior `llm.config` read. Configuration and credential reads form one snapshot; settings changes cannot alter an in-flight request. Connection IDs are random host-generated values persisted across restarts. Selecting another model or rotating a key preserves the connection identity; switching endpoints creates a new identity. No endpoint or key is exposed to applets.

Capabilities contain `tools`, `required_tool_choice`, and `exposed_reasoning`, each `supported`, `unsupported`, or `unknown`. An arbitrary OpenAI-compatible endpoint has no reliable standard model capability advertisement, so the initial adapter reports `unknown` and attempts the request. Demo mode reports these features `unsupported`. A generic HTTP 400 is a provider error, not proof of unsupported tools; only recognized explicit unsupported-feature errors are classified that way. Capability discovery is advisory; every request has its own outcome and provenance.

### Transport and limits

The native host embeds [Tokn's Rust SDK](https://github.com/tokn-ai/tokn) from Git `main`, with the resolved revision recorded in `Cargo.lock`. Each request builds an in-memory fixed-provider relay from the captured Apell configuration and supplies the optional key only in native request headers. Routing uses the exact configured model and endpoint prefix; it never selects another account/model or retries a generation automatically. The custom platform injects Apell's TLS client, disables request-event capture, and bounds response reads. It starts no gateway listener and loads no Tokn configuration or credential files.

Settings validates the same routing rules as generation. Provider hosts must be valid DNS names or IP addresses; trailing-dot DNS names and port zero are rejected. HTTP remains restricted to localhost and numeric loopback. A previously saved connection that no longer passes validation remains available in Settings for correction, without blocking shell startup or silently changing its endpoint. Invalid connections cannot send requests or be saved again unchanged.

The first integration executes OpenAI-compatible Chat Completions through Tokn with SSE or buffered JSON and does not follow redirects. Apell retains SSE framing and semantic validation on the opaque response bytes so private continuation metadata, provider-reported model identity, structured stream errors, and explicit finish reasons remain intact. At the integrated revision, Tokn's higher-level generation API cannot carry all required opaque assistant metadata; its managed OpenAI route also requires a key and rejects buffered responses to streaming requests. The relay path preserves Apell's existing compatible-endpoint contract while exercising Tokn's request pipeline.

Tokn's other provider adapters are not automatically available in Apell. Native provider presets, OAuth onboarding, Responses API, Anthropic/Gemini protocols, schema-constrained answer generation, model catalog discovery, and multimodal input need separate integration. Demo mode returns scripted local text without a model request. The protocol follows [Chat Completions tool and finish-reason fields](https://developers.openai.com/api/reference/resources/chat/subresources/completions/methods/create); assistant reasoning replay accommodates the [compatible reasoning-content contract](https://api-docs.deepseek.com/guides/thinking_mode/).

Provider URLs require HTTPS, except loopback HTTP, and reject embedded credentials, query parameters, and fragments. Provider errors are sanitized before crossing the bridge. Provider, HTTP, and resource requests share a TLS policy: Android uses bundled Mozilla roots with rustls verification, while Apple platforms use native certificate trust. Android device-installed private certificate authorities are not loaded; updating its bundled roots requires rebuilding the shell.

Requests allow 1–256 messages, up to 64 tool definitions/calls, at most 512 KiB of encoded input including schemas and private continuation data, and temperature from 0 to 2. Output and private metadata are bounded; generated output is capped at 1 MiB and raw provider responses at 8 MiB. The request timeout is 180 seconds, with a 60-second network read timeout. At most four requests run per session and 16 globally. The host rejects oversized context; it does not truncate history or summarize it for the applet.

Non-secret settings live in host application data. Keys are scoped by endpoint and stored in Apple Keychain on iOS/macOS or encrypted with an Android Keystore key on Android. Android ciphertext is excluded from device backup. Applets cannot read keys, modify shared settings, or access a generic credential store. Secure provider credentials are currently implemented for iOS, Android, and macOS.

## Native HTTP

Declare the `http` permission and call `http.request` through the SDK helper or `hostCall`:

```ts
// Import path from examples/chatbot/src; adjust for your applet's location.
import { http } from '../../../packages/applet-sdk/src';

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

For a reader such as `read`, retain the frontend, PDF rendering, domain models, annotation rules, schema, migrations, and SQL in the applet where portable. Replace `rusqlite` calls with asynchronous host calls. Use `http.request` for general networking, `fs` for private files, and `config` for JSON preferences; declare their manifest permissions as needed. Separate network transport, downloadable files, secure credentials, and mobile operations from application-specific logic before compiling the portable Rust crate to WASM.

An applet-level adapter can preserve familiar frontend command names while routing each operation to WASM or a general host service. Provider transport and credentials belong in the host; prompts, conversations, and domain-specific workflows belong in the applet. Applet upgrades keep the same `applet_id` to retain stored data. Cloud synchronization remains deferred, and updating the bundle does not automatically migrate or roll back its database.
