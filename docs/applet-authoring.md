# Applet authoring prompts

Paste any of these prompts into your coding assistant alongside your app idea, the downstream repository, and the Apell revision you are targeting. Use all three for a new applet, or pick the prompt that matches the work. The package example, architecture guide, and Companion source in that revision provide the details behind the prompts.

[Package example](../README.md#create-and-distribute-an-applet) · [Architecture and API contract](architecture.md) · [Companion source](../examples/chatbot)

## 1. Package, APIs, and permissions

```text
Build or migrate this app as an Apell applet. Read the targeted Apell checkout's README, docs/architecture.md, SDK types, and the relevant Companion code before choosing APIs. Confirm that each required capability exists for the target platform. Flag missing capabilities before designing a flow around them.

Package a root manifest.json and index.html with bundled local assets and relative URLs. Use schema_version 1, api_version 2, a stable applet_id, and a semantic version. If the view calls host services, bundle one copy of @apell/sdk and use client-side navigation within the entry document. For an optional Rust backend, use apell-guest from the same pinned Apell checkout. Export its ABI with apell_guest::export!, compile a cdylib for wasm32-unknown-unknown, and point manifest.lib at the packaged module under lib/. Use plain JSON with snake_case fields at the bridge boundary.

Choose the narrowest service and declare its permission in manifest.permissions:
- Use sqlite for structured durable data and schema migrations, fs for applet-private files, config for non-secret preferences, and resources for downloadable assets. Use backend-only secrets for app-owned credentials. Shared model credentials stay in Apell Settings.
- Use http.request for remote requests and device.info for platform/API information. Network access is on by default. Set "network": false for an intentionally offline applet. Network access still requires the relevant service declarations.
- Use llm.chooseModel from the attached view and llm.generate for inference. Check the selected model's capabilities. Media input and completed media output also require fs. Pass private paths and use result.media for generated files. Treat events as progress and the final result as authoritative. For continued conversations, retain successful assistant messages as returned, along with their model target and provenance.
- Call native camera, photo picker, location, audio, and speech from the attached view. Declare camera plus fs for captures, photos plus fs for imports, location for a foreground fix, audio for recording/playback, and speech for on-device transcription. Request each service from a relevant user action. Handle cancellation, denial, and unavailable hardware separately.
- Use backend-only websocket, secrets, crypto, and runtime permissions when the app actually needs those services.

Use @apell/sdk helpers or hostCall for permitted view services. Use applet.invoke and applet.onEvent for app-specific backend commands and events, and apell_guest::call for the backend's host services. Replace direct Tauri commands, browser network requests, and native Rust filesystem or socket calls with the corresponding Apell services. Keep transaction handles in the scope that opened them and commit before waiting on network work.

Pack the staging directory with apell pack stage --output artifacts/example.apel, with the output outside stage. Install the file in Apell and exercise the required permissions and failure paths. For public GitHub releases, attach the .apel and keep the asset name stable. Restart Apell when testing an update to an applet that already ran in the current process. Keep keys, personal paths, and signing settings out of source and release packages.
```

## 2. Mobile app architecture

```text
Design this applet for a phone running inside Apell. Use the applet viewport as the available content area. Apell owns the outer title bar, Close, and the menu for Reload and About. Give the applet its own navigation only for its internal tasks. Keep the main screen focused on the user's current task and put secondary tools where the user needs them.

Start with the narrowest phone layout. Let content reflow and wrap. Choose one main scroll region and keep focused fields and essential actions reachable when the keyboard is open. Size the layout to the iframe. Apell already applies the device's safe-area insets around it. Keep controls visible in landscape and at larger text sizes. Add tablet or desktop layouts when the extra space helps the task.

Use a frontend-only applet when view-owned work and host storage cover the requirements. Add a Rust backend when commands, state, or jobs should survive Close or Reload. A detached view loses its bridge, handles, and view-owned jobs. Accepted backend work can continue within the running host, and mobile suspension or process termination can interrupt it.

Treat every attached view as fresh. Subscribe to backend change events before fetching a snapshot. Refresh from authoritative state after reconnecting because events can be missed. Persist records, drafts, and job state that should survive process termination in host services. On restart, recover saved state and distinguish interrupted work from completed work. Store and display enough context for a user to decide whether to resume, retry, or discard.

Use standard HTML controls and layouts inside the applet. Route device features through Apell and use its native pickers and permission prompts. Bundle code, styles, fonts, and icons needed at startup. Keep rendering responsive while loading files or waiting for host calls, and release listeners, timers, and blob URLs when their owning view no longer needs them.
```

## 3. Interaction and review

```text
Review this applet by walking through its main task on a phone. Define the first useful screen and primary action. Give each control a clear label and visible focus. Aim for touch targets at least 48 CSS pixels wide and tall. Provide an accessible name for icon buttons. Keep important actions available without hover or gesture-only shortcuts. Check readable contrast, large text, and screen-reader order.

For each task, design the idle, loading, empty, success, and error states that apply. Keep input and selection when work fails. Tell the user what they can do next in the relevant screen. Separate user cancellation, permission denial, unavailable features, and retryable errors. Prevent duplicate submissions and offer Stop for work the app can cancel. Let input method composition finish before handling Enter as Submit. Show progress when the host provides it and keep partial or interrupted results visibly distinct from complete ones. Follow new content automatically only while the user is already at the end.

Ask for access when the user chooses the action that needs it. Explain what will happen in the applet and let Apell show the permission prompt or picker. Direct users to About this applet for applet permission settings. Before an explicit upload or model request with private files, show the selected items and destination. Use Apell's model picker for provider/model choice. Keep app-specific navigation, settings, and confirmations in the applet. Offer undo for reversible deletion when practical and confirm destructive actions that cannot be undone.

Check the primary task in the packaged applet on a narrow phone. Try the keyboard, landscape, large text, screen reader, cancellation or denial, slow or offline requests, and repeated taps. Close or reload mid-task and reopen to verify the intended behavior. For durable work, restart Apell and check recovery. Record which checks you ran on an actual target device and which still need device verification.
```
