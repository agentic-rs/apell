# Manage Store packages from the CLI

Install the current CLI from an Apell checkout:

```sh
cargo install --locked --path crates/apell-cli
```

## Sign in

```sh
apell login --store https://store.example.invalid
apell whoami
```

Login opens the Store in your browser and prints a verification code. Sign in
with GitHub if needed, compare the browser's code with your terminal, then approve
CLI access. The terminal finishes automatically. Use `--no-browser` to open the
printed address yourself, including when the CLI runs on another machine.

This reuses the Store's existing GitHub OAuth application and callback. The CLI
never receives the OAuth client secret, GitHub access token, or browser cookie.
Its own Store credential expires after seven days and has your current package
permissions. Removing package membership also removes CLI access to that package.

Login remembers the Store address. Use `--store` to choose another Store for a
command. Credentials are bound to that exact origin and are never forwarded
through HTTP redirects. Production sign-in requires HTTPS. The CLI accepts
loopback HTTP for local test fixtures.

On macOS, credentials use Keychain by default. On other Unix systems, the CLI
uses private files outside the repository. `APELL_CLI_CONFIG_DIR` explicitly
selects an isolated private file store, useful for testing. Never put that
directory inside a repository. Windows account credential storage is not yet
supported; packaging and GitHub Actions OIDC publishing remain separate paths.

## Register and inspect

```sh
apell package list
apell package register example
apell package show example
```

Use the same permanent `applet_id` in the package manifest. Registration reserves
the ID; the public listing appears after the first release. Owners and accepted
maintainers have the same management permissions as on the website.

## Connect GitHub Actions

```sh
apell package oidc set example \
  --repo example/applet \
  --workflow .github/workflows/publish.yml \
  --ref refs/heads/main

apell package oidc show example
apell package oidc disconnect example
```

Saving another repository changes the connection while preserving the applet ID
and existing releases. Connection changes take effect immediately and invalidate
old scoped CI credentials. Only public repository lookup is supported.

Use repeated `--ref` and `--event` options for multiple allowed values, and
`--environment` to require a GitHub environment. The default branch is
`refs/heads/main`; default events are `push` and `workflow_dispatch`. These are
exact refs, not wildcard patterns. The set command saves the complete policy.

The command configures the Store's publishing permission. Add the repository's
workflow and `APELL_STORE_URL` Actions variable as described in the
[CI guide](store.md#configure-the-workflow). The publishing job needs
`id-token: write` and should run `apell publish … --auth github-oidc`.

## Publish as yourself

```sh
apell publish artifacts/example.apel \
  --store https://store.example.invalid \
  --metadata listing.json \
  --icon icon.png \
  --auth account
```

Account publishing uses current membership and the existing immutable-version
rules. CI continues to select GitHub Actions OIDC automatically. Explicit account
or OIDC failures never fall back to an administrator token.

## Maintain access

```sh
apell package members list example
apell package members invite example example-maintainer
apell invitations list
apell invitations accept INVITATION_ID
apell invitations decline INVITATION_ID
apell package members remove example example-maintainer
apell package transfer example example-maintainer
```

Invitations require acceptance by the addressed GitHub account. Ownership can
only be transferred by the current owner to an existing maintainer, who must
accept the transfer. The old owner then becomes a maintainer. The owner cannot
be removed through the maintainer command.

## Sign out and revoke

```sh
apell logout
```

Logout revokes the current CLI credential on the Store and removes its local
copy. You can also revoke CLI credentials from **My packages → CLI access** on
the website. Revoking CLI access does not sign out the browser or disconnect a
package's GitHub Actions publisher.

An account can have up to 16 active CLI credentials. If login reaches that limit,
revoke an unused credential on the website before signing in again.

Management commands support `--json` for structured output. Credentials and
polling secrets are never printed, included in command arguments, or written to
project files. Only the temporary human verification code and approval address
are displayed during login.
