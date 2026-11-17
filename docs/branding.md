# Brand ownership

Apell owns the platform icon in `assets/icon.svg` and the host theme in
`src/styles.css`. The official apps repository owns applet identities and their
canonical artwork in `shared/assets/`. Applets can consume the Apell theme while
keeping their own icons and names.

Recorder, Learning, and Fieldnotes artwork under `site/applets/` is a website copy
of the corresponding apps assets. Change the canonical artwork in the apps
repository, then refresh these copies from an Apell checkout:

```sh
pnpm sync:app-icons
```

The script locates the primary Apell checkout through Git's common directory and
uses its sibling `apps` checkout, including when run from an Apell worktree. Set
`APELL_APPS_ROOT` to select another apps checkout. Paths are discovered locally and
are never written into the copied assets.

## Installed applet icons

Any applet can include an optional root `icon.png` in its `.apel` package. It must
be a still PNG, at most 256 KiB, with width and height each between 1 and 1,024
pixels. The native host validates the asset from the same package snapshot as the
manifest and supplies the launcher with a PNG data URL. No manifest extension or
host applet registry is required.

The launcher prefers package artwork. If artwork is missing, invalid, or fails to
display, it uses the applet's initial. The legacy Companion icon remains a
fallback for Companion packages without working artwork. Icon failures do not
block installation, listing, or launch. A store listing's separately uploaded
icon does not replace the icon inside an installed package.
