# External Sources Setup

## Obsidian
```
mur source add obsidian --vault ~/Documents/MyVault
```
No auth required. mur reads `*.md` files; ignores `.obsidian/` and `.trash/`. Use `--exclude-folder Drafts,Sandbox` to skip more.

## Notion
Two paths:

### A. Internal Integration Token (PAT) — fastest
1. Visit https://www.notion.so/my-integrations and create an internal integration.
2. Copy the "Internal Integration Token".
3. In Notion, share the pages/databases you want indexed with your integration ("Add connections" in the page menu).
4. Run:
   ```
   mur source add notion --token <pat>
   ```

### B. Public OAuth (PKCE) — for shared installs
Build mur with your `MUR_NOTION_CLIENT_ID` env var set. Then:
```
mur source add notion
```
Browser opens; authorize the workspace. Token is stored in OS keyring (macOS Keychain, Linux Secret Service, Windows Credential Manager).

## Joplin
Two modes:

### Local SQLite (single-machine)
```
mur source add joplin --db ~/Library/Application\ Support/joplin-desktop/database.sqlite
```
(Linux: `~/.config/joplin-desktop/database.sqlite`)

### Joplin Server (multi-device)
```
mur source add joplin --server https://your-joplin-server --token <api-token>
```

## Sync
- Manual: `mur source sync [<id>] [--full]`
- Watch (foreground daemon): `mur source sync --watch`
- Scheduled: `mur source install-schedule` then enable per the printed launchctl/systemctl command

## Search
- `mur search "query"` returns Patterns + Notes sections
- `mur search "query" --only-sources` for sources only
- `mur source search "query" -k 5` direct sources query
