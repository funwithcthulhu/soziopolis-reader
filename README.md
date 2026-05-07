# Soziopolis Reader

Personal Rust desktop tool for saving Soziopolis articles locally and managing
LingQ import workflows.

I built this for my own Soziopolis -> LingQ workflow. It works for me, and I
have not tried to make it into a general-purpose app.

There is no CLI path here. Everything happens in the GUI.

## What It Does

- Browse Soziopolis sections and paginate through article listings
- Save article text locally in SQLite
- Upload saved articles to LingQ
- Retry failed imports and uploads
- Build a portable folder or a Windows installer
- Write support bundles with logs, settings, database files, and queue snapshots

## Run From Source

```powershell
git clone https://github.com/funwithcthulhu/soziopolis-reader.git
cd soziopolis-reader
cargo run
```

`cargo build --release` writes the executable to
`target\release\soziopolis_lingq_tool.exe`.

## Windows Build

If you want to try the packaged build instead:

- Releases: <https://github.com/funwithcthulhu/soziopolis-reader/releases>
- Latest installer: <https://github.com/funwithcthulhu/soziopolis-reader/releases/latest>

## Portable Build

To refresh a portable folder build:

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\build-portable.ps1 -NoDesktopShortcut
```

On a new PC, LingQ usually needs to be reconnected once because the token lives
in Windows Credential Manager for that machine.

## Installer Build

To build the installer, install [Inno Setup 6](https://jrsoftware.org/isinfo.php) and run:

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\build-installer.ps1
```

You can also point it at a specific compiler:

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\build-installer.ps1 -IsccPath "C:\Program Files (x86)\Inno Setup 6\ISCC.exe"
```

## Data Path

By default the SQLite database lives at:

`%LOCALAPPDATA%\soziopolis_lingq_tool\soziopolis_lingq_tool.db`

The app also supports a portable layout automatically. If the executable sits
beside a folder named `data` or `portable_data`, it stores settings and the
SQLite database there instead of `%LOCALAPPDATA%`.

Expected portable structure:

```text
Soziopolis Reader.exe
data/
  soziopolis_lingq_tool/
    settings.json
    soziopolis_lingq_tool.db
    logs/
      soziopolis-reader.log
    support_bundles/
      support-bundle-<timestamp>/
```

On Windows, LingQ tokens are stored in Windows Credential Manager rather than `settings.json`.

The internal storage folder keeps the historical `soziopolis_lingq_tool` name so
existing installs and upgrades continue to find the same data.

If you want the app and its data in a custom location, use the portable layout
instead of the default `%LOCALAPPDATA%` location.

## Notes

- This is packaged and tested as a Windows desktop tool.
- The scraper is tuned for Soziopolis article pages and section listings as they
  existed on April 16, 2026.
- If Soziopolis changes its markup, the scraping selectors may need an update.
- If LingQ changes its API behavior, the import/upload flow may need an update
  too.

A few implementation notes live in [docs/dev-notes.md](docs/dev-notes.md).
