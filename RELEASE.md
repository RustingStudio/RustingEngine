# RustingEngine Release Guide

## Release notes

Release notes are the `CHANGELOG.md` entries contributors add under
`## [Unreleased]` (see `CONTRIBUTING.md`). To cut a release:

1. Pick the version. New features or a `Changed` line that breaks saved
   scenes, game code or the wire protocol raise the minor version (2.3.0
   changed the protocol of 2.2.0); a release with only fixes raises the
   patch version.
2. Rename `## [Unreleased]` to `## [x.y.z] - YYYY-MM-DD` and add a fresh,
   empty `## [Unreleased]` above it.
3. Under the new heading, write one sentence saying what the release is
   for, such as the game or feedback that drove it.
4. Keep the sections in this order and drop empty ones: `Added`,
   `Changed`, `Fixed`, `Performance`, `Known limits`. Merge duplicate lines
   and remove lines for work that was reverted before the release.
5. End the entry with a `---` line, like the earlier ones.
6. The release workflow uses the entry as the GitHub Release description.

## Before tagging

1. Update the version in `Cargo.toml` to the one in `CHANGELOG.md`.
2. Run the complete local gate:

```bash
./scripts/check_release.sh
```

3. Run `./scripts/run_editor.sh` and check project create/open, scene editing,
   Cargo Check, Build & Run, and Export Game on a Vulkan-capable machine.
4. Confirm `git status` contains only the intended release changes, then commit
   and push them. Wait for the CI workflow on that commit to pass.

## Publish on GitHub

Push the release commit to `main`. When CI passes on it, the release
workflow sees a `Cargo.toml` version without a GitHub Release, builds the
Linux and Windows archives, tags the commit `v<version>`, and publishes the
Release with the version's `CHANGELOG.md` entry as its notes. Pushing a tag
by hand is not needed; a hand-made tag must point at that same commit.
Use "Run workflow" on the Release workflow to retry a failed publish.

Generated game projects use the matching Git tag when the editor is running
without a local engine source checkout. Do not move or recreate a published
version tag; publish a patch version for later fixes.

## After publishing

- Download both archives and confirm their editor executable starts.
- Create a clean project with the downloaded editor.
- Export its default scene and run the exported game on Windows and Linux.
- Record any driver-specific Vulkan problem in the GitHub Release notes.
