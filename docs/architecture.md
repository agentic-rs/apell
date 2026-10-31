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

The envelope `request_id` correlates a bridge call. Long-running operations also carry their own `request_id` inside `params`, which correlates progress and cancellation. All errors reject the request promise. The shell rejects duplicate in-flight envelope IDs and allows at most 32 concurrent calls per bridge.

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

| Method         | Parameters                                       | Result                                                   |
| -------------- | ------------------------------------------------ | -------------------------------------------------------- |
| `llm.config`   | None                                             | `{ model_id, mode }`, where mode is `demo` or `provider` |
| `llm.complete` | `request_id`, `messages`, optional `temperature` | `{ request_id, content }`                                |
| `llm.cancel`   | `request_id`                                     | `null`                                                   |

Messages contain `role` (`system`, `user`, or `assistant`) and `content`. Applets construct prompts and conversation history. The host chooses the configured provider, endpoint, model, and credential; these cannot be overridden in an applet request.

`onLlmEvent` subscribes to `{ request_id, kind, text }` events. `delta` carries incremental text and `done` marks successful completion. Failures and cancellation reject `hostCall`; the SDK's `HostError.partial_content` retains received text so an applet can save an interrupted response. Subscription cleanup is the applet's responsibility.

Provider mode uses an OpenAI-compatible text chat-completion endpoint, supports SSE streaming and buffered JSON responses, and does not follow redirects. Provider URLs require HTTPS, except loopback HTTP during development, and cannot contain embedded credentials, query parameters, or fragments. Demo mode returns scripted local text and makes no model request.

Provider requests, generic HTTP requests, and resource downloads share a TLS policy. Android uses bundled Mozilla root certificates with rustls certificate and hostname verification; device-installed private certificate authorities are not loaded. Updating Android's bundled roots requires rebuilding the shell. Apple platforms continue to use native certificate trust.

The host accepts 1–256 messages, at most 512 KiB of input content, and temperature values from 0 to 2. It caps generated content at 1 MiB and raw provider responses at 8 MiB, with a 180-second request timeout and a 60-second network read timeout. At most four requests run per session and 16 across the service. History beyond these limits is rejected; there is no automatic truncation, summarization, tool calling, or multimodal support.

The native shell owns global provider configuration. Non-secret settings live in its application data directory. Keys are scoped by endpoint and stored in Apple Keychain on iOS/macOS, or encrypted with an Android Keystore key on Android. Android ciphertext is excluded from device backup. Applets can read the selected model/mode, but cannot read keys, change shared settings, or access a generic credential store. Secure provider credentials are currently implemented for iOS, Android, and macOS.

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
