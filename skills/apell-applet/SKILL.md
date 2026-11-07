---
name: apell-applet
description: Build, migrate, package, or review applets for the Apell mobile shell. Use for .apel manifests and packages, Apell host APIs and permissions, optional Rust WASM backends, and applet layout and interaction on iOS and Android.
---

# Apell Applet

Work in the downstream applet repository. Check the existing manifest, applet ID, and SDK revision before changing them. Use the applet's pinned Apell checkout for API details when available. Match proposed capabilities to the target host and platforms. Flag any required host feature that is absent.

## Choose the references for the task

- For new packages, Tauri migrations, permissions, or host API calls, read [Package and APIs](references/package-and-api.md).
- For screen layout, view lifecycle, durable state, or work that should survive Close or Reload, read [Mobile architecture](references/mobile-app.md).
- For interaction design, native permission flows, accessibility, or usability review, read [Interaction and review](references/interactions.md).

A new applet may need all three. A focused fix or review usually needs only the reference for that change.

## Respect the shell boundary

- Apell owns the outer title bar, Close, Reload, About, native permission UI, and provider settings. The applet owns its task flow, internal navigation, and data.
- Bundle the view for the sandboxed WebView and call native services through the Apell SDK. An optional Rust WASM backend runs in Apell outside that view and has its own service scope.
- Keep the applet ID stable across updates. Declare the permissions used by the applet. Persist data that must survive an Apell restart in a host storage service.
- Exercise changes through the installed `.apel` when a compatible host is available. A browser preview can check layout. The installed applet verifies the bridge, permissions, and native lifecycle. Report which checks ran.
