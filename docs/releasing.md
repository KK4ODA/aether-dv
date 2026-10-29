# Releasing Aether DV

A release is a `vX.Y.Z` tag on `main`. The `package` workflow then:
1. builds the app with the engine bundled, on Linux, macOS (Apple silicon) and Windows;
2. signs the update packages;
3. publishes the installers to the public [KK4ODA/aether-dv-releases](https://github.com/KK4ODA/aether-dv-releases);
4. points the update feed there.

Installed copies find the update when they next check. They install it only when the operator clicks **Install and restart**.

## How updates are trusted

- Every update package is signed with the project's update key. The app carries the matching public key and refuses anything else.
- The signature also names the version it was signed for, and the app rejects a package whose signed version differs from the one the feed announces. A tampered feed therefore can't push an older release.
- The feed is `latest.json` on the `main` branch of the releases repo, at `https://raw.githubusercontent.com/KK4ODA/aether-dv-releases/main/latest.json`. Pre-releases are listed too, which `releases/latest` would hide.
- **Whoever holds the private key can ship code to every installation.** Keep it offline, back it up, and never commit it.
- **If the key is lost**, installed apps can no longer be updated. Each operator would have to reinstall a build carrying a new public key by hand.

## One-time setup

1. **Releases repo.** On GitHub, create the public repository `KK4ODA/aether-dv-releases` and tick **Add a README** (the feed is committed to its `main` branch).
2. **Update key.** On your own computer:
   ```bash
   cargo install tauri-cli --version "^2" --locked
   ```
   ```bash
   cargo tauri signer generate -w ~/.tauri/aether-dv.key
   ```
   Choose a password when asked. This writes `aether-dv.key` (private) and `aether-dv.key.pub` (public). Back up both, with the password, somewhere offline.
3. **Token for publishing.** GitHub → Settings → Developer settings → Fine-grained tokens → Generate new token:
   - **Resource owner:** KK4ODA
   - **Repository access:** only `aether-dv-releases`
   - **Permissions:** Contents set to Read and write
   - **Expiry:** a date you'll remember to renew
4. **Secrets and variables.** In `KK4ODA/aether-dv` → Settings → Secrets and variables → Actions:

   | Kind | Name | Value |
   |---|---|---|
   | Secret | `TAURI_SIGNING_PRIVATE_KEY` | the full contents of `aether-dv.key` |
   | Secret | `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` | its password |
   | Secret | `RELEASES_TOKEN` | the token from step 3 |
   | Variable | `UPDATER_PUBKEY` | the full contents of `aether-dv.key.pub` |

   A tag build fails with a clear message if any of these is missing. It never publishes an unsigned release.

## Each release

1. Set the version in `Cargo.toml` (`[workspace.package] version`) and `app/src-tauri/tauri.conf.json` (`version`). The workflow refuses a tag that doesn't match.
2. Write `docs/releases/vX.Y.Z.md`. It becomes both the release page and the text shown in the app's update prompt.
3. Merge to `main`, then tag and push:
   ```bash
   git tag vX.Y.Z
   ```
   ```bash
   git push origin vX.Y.Z
   ```
4. Watch the `package` workflow. When it's green, the release is on the releases repo and the feed points at it.

Versions `0.x` and versions with a suffix (`1.0.0-rc.1`) are published as GitHub pre-releases.

## Testing the updater

`app/tools/updater-e2e.sh WORKDIR` runs the whole path on Linux with a throwaway key:
1. builds the current version and the next patch version as AppImages;
2. serves the feed locally;
3. runs the older build under Xvfb.

That build must find the update, verify it, install it over itself and restart as the new version. It uses the test-only `selftest` feature, which is never enabled in release builds.

## What the updater does not do

- **Linux:** the AppImage updates itself. Installs from the `.deb` or `.rpm` are updated through the same packages, which needs administrator rights (the updater asks via the system's password prompt).
- **The engine:** it is bundled, so an update replaces it too. An engine started by the app is stopped during the update, and you start it again from the connect screen. An engine you started yourself from the installation folder must be stopped before installing; on Windows the installer can't replace a running program.
- **OS code signing:** installers are not code-signed for Windows or notarised for macOS yet (Phase F). Windows SmartScreen and macOS Gatekeeper warn on first install. On macOS: right-click the app → Open. The update-signature check above is separate from OS code signing and is always enforced.
