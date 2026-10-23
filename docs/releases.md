# Building the app and signed releases

lemur ships as `lemur.app` in a disk image: open it, drag lemur onto the
Applications shortcut, eject it. Releases are Apple Silicon (arm64) only and
need macOS 11 or newer. The version lives in one place, `Cargo.toml`; the app
build writes it into `Info.plist`.

## Local builds

```sh
sh scripts/build-app.sh
```

builds `build/lemur.app` from a release build and signs it ad hoc. That runs
on the Mac that built it; a copy downloaded elsewhere would be blocked by
Gatekeeper, which is what signed releases are for. `BUILD_DIR` moves the
output. `LEMEN_SIGNING_IDENTITY` signs with another identity; a
`Developer ID Application: …` identity also turns on the hardened runtime and
a secure timestamp, as notarization requires. lemur needs no entitlements.

The build turns `Resources/lemur.png` (square, at least 1024×1024) into the
app icon with `scripts/build-icon.sh`; without it the app has the default
icon. To replace the logo, prepare the new original once with libvips (`brew
install vips`): `sh scripts/optimize-icon.sh /path/to/original.png
Resources/lemur.png` scales it to 1024×1024 sRGB, keeps full transparency so
gradients don't band, and strips metadata.

## Disk image

The image opens with `lemur.app` on the left and a shortcut to
`/Applications` on the right, with an arrow between them. Packaging uses
[dmgbuild](https://dmgbuild.readthedocs.io/), with hash-locked dependencies
installed into a private virtual environment (`build/dmg-tools`). It needs
Python 3.10 or newer; macOS's own `python3` is older, so name another:

```sh
PYTHON=python3.13 sh scripts/prepare-dmg-tools.sh   # once
sh scripts/build-dmg.sh build/lemur.app build/lemur.dmg
```

The packager never signs or changes the app, refuses to replace an existing
image, and mounts the finished one to check the app's signature and the
Applications shortcut. `sh scripts/test-dmg.sh` packages a small ad-hoc
fixture app the same way, without credentials.

## Tests on main

Every push to `main` runs **Build lemur** (`.github/workflows/release.yml`)
for the tests only: it installs the Rust version from `mise.toml`, Xcode's
Metal toolchain and tmux, and runs the tests. It gets no secrets and
publishes nothing. A newer push cancels a run still going for an older one.
These runs keep the compiled dependencies in GitHub's cache
([rust-cache](https://github.com/Swatinem/rust-cache)) for the next one;
the signed build of a release never uses it and compiles everything itself.

## Publish a release

Bump `version` in `Cargo.toml`, commit and push to `main`, then tag that
commit with the same version:

```sh
GIT_EDITOR=true git tag -a v0.1.0 -m "lemur v0.1.0"
git push origin refs/tags/v0.1.0
```

Only a new `v*` tag builds and publishes. The build checks that the tag
matches `Cargo.toml` and that the commit is in `main`'s history, installs the
same toolchain and runs the tests, all before it touches any secret. Then
`scripts/notarize-app.sh` imports the certificate into a temporary keychain,
builds and signs the app and the disk image, submits the image to Apple's
notary service, and staples the ticket. It publishes `lemur-macos-arm64.dmg`
and `SHA256SUMS.txt` as the GitHub Release for the tag. Use a new version for
each release; updating or deleting a tag doesn't rebuild it.

## Apple credentials

lemur uses the same credentials as any other app of the team: the
**Developer ID Application** certificate (exported with its private key as a
password-protected `.p12`) and an App Store Connect **team API key** allowed
to notarize (its `.p8`, key ID and issuer ID). No new certificate or App ID
is needed.

In the repository's settings:

1. The environment named **release** admits only `v*` tags (**Selected
   branches and tags**). Protect `v*` tags against updates and deletion, and
   protect `main` against force-pushes: whoever can push a `v*` tag on
   `main`'s history can get code signed.
2. Add these **environment secrets** (not repository secrets). They, and the
   signing variables of the scripts, keep the `LEMEN_` names from before
   the rename:

| Secret | Value |
| --- | --- |
| `LEMEN_CERTIFICATE_BASE64` | Single-line base64 of the `.p12` |
| `LEMEN_CERTIFICATE_PASSWORD` | The `.p12`'s password |
| `LEMEN_SIGNING_IDENTITY` | The exact identity, e.g. `Developer ID Application: Your Name (TEAMID)` |
| `LEMEN_NOTARY_KEY_BASE64` | Single-line base64 of the `.p8` |
| `LEMEN_NOTARY_KEY_ID` | The API key's ID |
| `LEMEN_NOTARY_ISSUER_ID` | The API key's issuer ID |

Supply them through GitHub's secret settings only; never commit them or paste
them anywhere else.

## Launched from Finder

An app started from Finder or the Dock gets a short `PATH`
(`/usr/bin:/bin:/usr/sbin:/sbin`). Remote hosts are unaffected: `ssh` and
`scp` live in `/usr/bin`, and commands on the host use the host's own
`PATH`. A host with `ssh = "local"` runs tmux on this Mac, often from
Homebrew's `/opt/homebrew/bin`, so at startup lemur asks your login shell
(`$SHELL -l`) for its `PATH` and adds the folders its own lacks, after its
own. A shell that takes longer than 2 seconds leaves the `PATH` as it was;
start lemur from a shell then.
