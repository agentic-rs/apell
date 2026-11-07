# Package and APIs

## Build an installable applet

Stage the built frontend with a root `index.html`, `manifest.json`, and relative paths to bundled assets. Use a stable lowercase `applet_id`, semantic `version`, `schema_version: 1`, and `api_version: 2` for the current package contract.

```json
{
  "schema_version": 1,
  "api_version": 2,
  "applet_id": "example",
  "name": "Example",
  "version": "0.1.0",
  "permissions": ["config"]
}
```

Use `@apell/sdk` from the pinned Apell checkout when the view calls host services. For a checkout at `vendor/apell`, add it with `pnpm add ./vendor/apell/packages/applet-sdk`. Bundle one SDK instance in the entry document and use client-side navigation. For an optional backend, use `apell-guest` from the same checkout, export the ABI with `apell_guest::export!`, build a `cdylib` for `wasm32-unknown-unknown`, and add `"lib": "lib/main.wasm"` to the manifest with the module at that path.

Install the CLI from the same checkout, pack to a path outside the staging directory, and install the resulting file with **+ → Install an .apel file** in Apell.

```sh
cargo install --locked --path vendor/apell/crates/apell-cli
apell pack stage --output artifacts/example.apel
```

Adapt `vendor/apell` to the actual checkout. Keep keys, personal paths, and signing settings out of source and release artifacts. For public GitHub distribution, attach the `.apel` to a published release, keep the asset name stable, and advance the version for changed stable content. Restart Apell to test the new package if the old version already ran in the process.

## Choose host services

Use SDK helpers or `hostCall` for permitted calls from the view. Use `applet.invoke` for app-defined backend commands, `applet.onEvent` for backend events, and `apell_guest::call` for host calls from the guest. Send plain JSON with `snake_case` fields. Port native Tauri commands to these boundaries. The view's direct network access is restricted, and the guest has no WASI filesystem or socket imports.

| Need                     | Service and manifest declaration                                                                                                   |
| ------------------------ | ---------------------------------------------------------------------------------------------------------------------------------- |
| Structured durable data  | `sqlite`. Run applet-owned migrations and keep handles in their opening scope. Commit before waiting on network work.              |
| Private file bytes       | `fs`. Use relative applet paths and read large files in chunks.                                                                    |
| Non-secret preferences   | `config`. Distinguish a stored `null` from a missing key.                                                                          |
| Downloadable assets      | `resources`. Keep the returned resource ID.                                                                                        |
| HTTP or platform details | `http` for `http.request`, `device` for `device.info`.                                                                             |
| Shared model access      | `llm`. Add `fs` for input attachments or completed media output.                                                                   |
| Native sensors           | `camera` plus `fs` for captures, `photos` plus `fs` for imports, and `location` for a foreground fix. Call from the attached view. |
| Audio and transcription  | `audio` for native recording, playback, import, and sharing. `speech` for on-device transcription. Call from the attached view.    |
| Guest-only services      | `websocket` for binary sockets, `secrets` for app-owned credentials, `crypto` for secure randomness, and `runtime` for timers.     |

Network access is enabled by default. Add `"network": false` for an intentionally offline applet. Each service still requires its own declaration. A declared `secrets` permission gives the guest applet-scoped secure storage without a prompt. Shared provider credentials stay in Apell Settings.

For model work, use `llm.chooseModel()` in the attached view and check `llm.config(target).capabilities`. Treat generation events as progress and use the final result to determine success. Keep successful assistant messages intact for continuation and store the model target and returned provenance. Pass media attachments as private file paths and read completed output from `result.media`. Use `photos.pick({ output_format: 'jpeg' })` to prepare a selected photo for models accepting JPEG. Check supported audio formats before reusing native recordings as model input.

The macOS host uses the same bridge and package format. Photo selection opens a native image file picker; microphone recording, audio file import, and playback are supported. Check `audio.capabilities` before offering controls; audio sharing remains mobile-only. Use the existing audio start/pause/resume/stop/status calls and the host's per-applet microphone prompt. An ongoing macOS recording continues when the native window is hidden or minimized, while closing or reloading the applet stops capture. Keep the interface usable at phone width and let wider layouts follow the applet viewport when the desktop window grows.

Use the target revision's package and API sources for signatures, platform checks, and limits. [Package walkthrough](https://github.com/agentic-rs/apell/blob/main/README.md#package-and-distribute) · [Host API contract](https://github.com/agentic-rs/apell/blob/main/docs/architecture.md) · [SDK types](https://github.com/agentic-rs/apell/blob/main/packages/applet-sdk/src/types.ts)
