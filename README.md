# rekordbox-to-engine

`rekordbox-to-engine` prepares a Rekordbox USB export for Engine DJ players. It reads the modern Rekordbox OneLibrary database, runs the installed Engine DJ `OfflineAnalyzer` once per track, and writes an Engine Library beside the existing `PIONEER` and `Contents` folders. The current workflow is serial and uses an 88 BPM analyzer minimum.

## Requirements

- macOS with Engine DJ installed at `/Applications/Engine DJ.app` and its `OfflineAnalyzer` executable available.
- Rust toolchain to build from source. The executable includes its SQLCipher and SQLite dependencies; neither CLI is needed at runtime.
- A modern Rekordbox USB export containing `PIONEER/rekordbox/exportLibrary.db` (or `.PIONEER`) and the referenced audio under `Contents`. Close Rekordbox before running the command.

## Use

Install the command from this checkout with `cargo install --path . --locked`.
It is installed as `rekordbox-to-engine`; scripts that invoked `rb-engine`
need to use the new command name.

From this repository, preview the tracks and playlists without writing to the USB:

```sh
cargo run --release -- --target /Volumes/BANGERS --dry-run
```

Then prepare the same USB:

```sh
cargo run --release -- --target /Volumes/BANGERS
```

The command prints a JSON report after writing and verifying the Engine databases, analysis cells, and RGB waveform files. Captured analyzer packets go into a new directory under the system temporary directory; use `--capture-output-dir /private/tmp/my-new-captures` to choose a fresh directory. `--analyzer /path/to/OfflineAnalyzer` overrides the installed executable path. The command refuses to reuse an existing capture directory.

If an existing Engine Library was edited in Engine DJ or on a player, the command normally refuses to replace it. Pass `--replace-engine-library` only when you intentionally want to discard that library and build a new one. rbxport asks before making this choice.

The tool reads the Pioneer database and analysis files and writes the `Engine Library` directory on the USB. On repeat exports it refreshes only a library created by this tool whose main database is unchanged and whose companion databases contain no Engine-side data. It stages and verifies the replacement before moving the previous library aside, restores it if publication fails, and recovers an interrupted swap on the next run. An unknown or modified Engine library is left alone. A refresh rebuilds the Engine database and analysis; it does not merge edits made in Engine DJ or on a player. Keep a backup of a USB before using it on a player.

## Testing

```sh
cargo test --offline
```

Normal tests use generated tiny files and three recorded analyzer packet streams, each below 1 MB. The ignored reference integration test can replay those streams against a **disposable** copy of the three-track Rekordbox export:

```sh
REKORDBOX_TO_ENGINE_INTEGRATION_USB=/private/tmp/rekordbox-to-engine-integration-example \
REKORDBOX_TO_ENGINE_INTEGRATION_CAPTURES=/private/tmp/rekordbox-to-engine-captures-example \
cargo test --offline --test reference_integration -- --ignored
```

The integration test refuses a target outside `/private/tmp/rekordbox-to-engine-integration-*`. It requires an unmodified three-track stage-one USB copy with IDs 1, 2, and 3 and no Engine library. It writes an Engine library to that disposable copy.

## Current limits

- Requires a modern OneLibrary export; legacy `export.pdb` alone is not supported.
- A helper-managed Engine library with an unchanged main database can be refreshed. Merging Engine-side edits, caching analysis, and parallel workers are later work. Analyzer execution is behind the per-track `Analyzer` trait in `src/analyzer.rs`.
- Internal verification checks the written database, media paths, and analysis payload presence. Player behavior still requires a player test for each new workflow or firmware target.
