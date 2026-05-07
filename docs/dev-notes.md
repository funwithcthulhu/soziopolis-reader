# Dev Notes

A few implementation notes I want to keep around:

- Runtime data goes under `%LOCALAPPDATA%\soziopolis_lingq_tool\` by default.
- If the executable sits next to a `data` or `portable_data` folder, the app uses
  that instead.
- The local library is a SQLite database with FTS for search.
- The GUI kicks scraping, import, refresh, and LingQ upload work onto blocking
  worker threads.
- The on-disk name is still `soziopolis_lingq_tool` so older local data keeps
  working.
- If Soziopolis changes its markup or LingQ changes its API behavior, this tool
  will probably need code changes.
