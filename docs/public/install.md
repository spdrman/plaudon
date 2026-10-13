# Installing Plaudon

Plaudon is an independent, local-first desktop application for importing,
transcribing and reviewing your own recordings. It is not produced by,
affiliated with or approved by PLAUD, OpenRouter or Apple.

## What is actually published today

Nothing. 0.1.0 exists as a draft prerelease: the macOS bundle and the Windows
installer are built, checksummed and attached to it, and a human has not
published it. Until that happens:

- `brew tap spdrman/plaudon && brew install --cask plaudon` finds no cask,
  because the tap is only written when a release is published.
- `winget install spdrman.Plaudon` finds no package, because the manifests have
  not been submitted to `microsoft/winget-pkgs`, and submission is a pull
  request a human opens and a maintainer there accepts.
- The manual download below is the only route, and it needs the draft to be
  published first as well.

Both artifacts are unsigned: the macOS bundle is ad-hoc signed and not
notarized, and the Windows installer is not Authenticode-signed. The Windows
installer has never been installed or run on Windows. See the
[release notes](./release-notes.md) for what that means in detail.

## macOS, Apple Silicon

```sh
brew tap spdrman/plaudon
brew install --cask plaudon
plaudon-setup-plaud
```

The cask is Apple Silicon only (`depends_on arch: :arm64`) and requires the
macOS version the bundle itself declares as its minimum. It depends on the
`node` formula, because the official PLAUD Cloud CLI is an npm package.

Three things the cask does that are worth knowing before you run it:

1. **It installs an npm package globally.** Unless a `plaud` is already in
   Homebrew's prefix, installation runs
   `npm install -g @plaud-ai/cli@<pinned version>`. That is outside Homebrew's
   bookkeeping and `brew uninstall --cask plaudon` does not undo it; remove it
   with `npm uninstall -g @plaud-ai/cli`.
2. **It puts one helper on your PATH.** `plaudon-setup-plaud` ships inside the
   bundle and is linked from it. It checks Node.js, installs the pinned CLI if
   it is missing or the wrong version, runs the vendor's `plaud login`, and
   confirms the session with `plaud me`. It prints no token: everything the
   vendor CLI writes is passed through a redactor first.
3. **The bundle is quarantined.** Homebrew applies the quarantine attribute to
   what it downloads, and because the bundle is unsigned and not notarized,
   Gatekeeper will refuse to open it. Either clear the attribute afterwards:

   ```sh
   xattr -dr com.apple.quarantine /Applications/Plaudon.app
   ```

   or install without it in the first place:

   ```sh
   brew install --cask --no-quarantine plaudon
   ```

   The cask does not strip the attribute for you. Running unsigned software is
   a decision that stays yours.

## Windows 11, x64

```powershell
winget install spdrman.Plaudon
& "$env:LOCALAPPDATA\Programs\Plaudon\Plaudon-Setup-Plaud.ps1"
```

The manifests declare a package dependency on `OpenJS.NodeJS.LTS`, because
WinGet installs packages and cannot install an npm package. The installer is
per-user: it writes to `%LOCALAPPDATA%\Programs\Plaudon` and `HKCU`, needs no
elevation, and leaves `%APPDATA%\app.plaudon.desktop` — your library — alone
when uninstalled.

`Plaudon-Setup-Plaud.ps1` is installed beside `Plaudon.exe` and does what the
macOS helper does: Node.js check, pinned CLI install, `plaud login`,
`plaud me`, no token printed. It is also attached to the release as a
standalone asset.

Two caveats, both unavoidable today:

- The installer is unsigned, so SmartScreen will warn about an unknown
  publisher.
- Windows is untested. The installer is built and audited on a Windows runner,
  but no automated suite installs or runs the application on Windows, and the
  [release notes](./release-notes.md) list that as a known limitation.

## The official PLAUD Cloud CLI

Importing from a PLAUD account needs PLAUD's own command-line tool, the npm
package `@plaud-ai/cli`. It is not bundled and not redistributed here; it is
installed from npm, and it signs in to your own PLAUD account through the
vendor's browser flow. We never ask you to paste a token.

Exactly one release of that tool has been exercised — `@plaud-ai/cli` 0.3.15,
on macOS — and the version lock in `crates/source-plaud-cli/cli-compatibility.yaml`
is compiled into the application. Any other version is refused with a message
naming the version you have, rather than parsed on the assumption that nothing
changed. That is why both installers pin one exact version: a newer CLI is not
an upgrade here, it is a stop.

The PLAUD source is also blocked while Local-only Mode is on, which it is from
the first launch. See the [privacy disclosure](./privacy-disclosure.md) and the
[vendor dependency matrix](./vendor-matrix.md).

## Manual download

Once a release is published, every artifact is on its GitHub release page:

- `Plaudon-<version>-macos-arm64.zip` — unzip, move `Plaudon.app` to
  `/Applications`, then `xattr -dr com.apple.quarantine /Applications/Plaudon.app`.
  `plaudon-setup-plaud` is inside the bundle at
  `Plaudon.app/Contents/Resources/plaudon-setup-plaud`.
- `Plaudon-<version>-x64.msi` — run it, then
  `Plaudon-Setup-Plaud.ps1` from the install directory.
- `checksums.txt` — the SHA-256 of both files. Check them.
- `macos-signing-report.json` and `windows-signing-report.json` — what was and
  was not verified, in the packaging scripts' own words. A release that claims
  to be signed says `verified` there.
- `plaudon.rb` and `winget-manifests-<version>.zip` — the cask and the WinGet
  manifests this release was described by, so the published checksums can be
  compared against the files you downloaded.

## How the cask and the manifests are produced

They are rendered, not written. `plaudon-release packaging render` reads the
two built files, hashes them, and fills the templates under `packaging/`; it
refuses to emit a file that still contains a placeholder, a digest that is not
a SHA-256, or an asset whose name belongs to a different version. The release
workflow runs it against the artifacts it just built and attaches the result to
the release. `packaging/examples/` holds the same rendering for a version that
does not exist — 0.0.0, fixture bytes, a URL that resolves to nothing — so the
output can be read without a release in hand.

Publishing has two manual halves, both deliberate:

- **Homebrew.** The cask lives in the `spdrman/homebrew-plaudon` tap, a
  separate repository, and a workflow's `GITHUB_TOKEN` cannot write to another
  repository. With a `TAP_PUSH_TOKEN` secret the release workflow pushes the
  cask there once the release is published and signed; without it, or while the
  release is still a draft, it prints the exact commands instead of pushing a
  cask whose download URL would answer 404.
- **WinGet.** Manifests reach users through a pull request to
  `microsoft/winget-pkgs`, which a person opens and a maintainer there accepts.
  The rendered files are laid out as `manifests/s/spdrman/Plaudon/<version>/`
  so submitting them is a copy, not a transcription. Nothing here submits
  automatically.

## No warranty

Plaudon is provided as is, with no warranty of any kind; you use it at your own
risk. See the MIT [LICENSE](../../LICENSE).
