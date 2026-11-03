# rekordbox-to-engine

`rekordbox-to-engine` prepares a fresh Rekordbox USB export for Engine DJ players. It reads the modern Rekordbox OneLibrary database, runs the installed Engine DJ `OfflineAnalyzer` once per track, and writes an Engine Library beside the existing `PIONEER` and `Contents` folders. The current workflow is serial and uses an 88 BPM analyzer minimum.

## Requirements

- macOS with Engine DJ installed at `/Applications/Engine DJ.app` and its `OfflineAnalyzer` executable available.
- Rust toolchain to build from source. The executable includes its SQLCipher and SQLite dependencies; neither CLI is needed at runtime.
- A fresh Rekordbox USB export containing `PIONEER/rekordbox/exportLibrary.db` and the referenced audio under `Contents`. Close Rekordbox before running the command. This version refuses a target that already has an `Engine Library` folder.

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

The command prints a JSON report after writing and verifying the Engine databases, analysis cells, and RGB waveform files. Captured analyzer packets go into a new directory under the system temporary directory; use `--capture-output-dir /private/tmp/my-new-captures` to choose a fresh directory. `--analyzer /path/to/OfflineAnalyzer` overrides the installed executable path. The command refuses to overwrite an existing Engine library or capture directory.

The tool reads the Pioneer database and analysis files and writes only the new `Engine Library` directory on the USB. It stages the Engine files first and publishes the directory after internal verification. Keep a backup of a USB before using it on a player.

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
- Starts from a fresh Rekordbox export. Updating an existing Engine library, caching analysis, and parallel workers are later work. Analyzer execution is behind the per-track `Analyzer` trait in `src/analyzer.rs`.
- Internal verification checks the written database, media paths, and analysis payload presence. Player behavior still requires a player test for each new workflow or firmware target.
