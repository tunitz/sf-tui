---
name: sf-cli-contracts
description: JSON shapes the TUI relies on from the Salesforce CLI, how to refresh the bundled catalog snapshot, and how to regenerate synthetic test fixtures. Use when changing parsing in src/sf or fixtures in tests.
---

# Salesforce CLI contracts

## Endpoint shapes

All parsers accept both a bare JSON value and a `{ "status": ..., "result": ... }` wrapper.
Fields absent in one CLI version must degrade to empty values rather than fail.

| Command | Consumed shape | Notes |
| --- | --- | --- |
| `version --json` | `result.version` or a plain version token | Probe runs with a 5 s timeout |
| `commands --json` | `result[]` with `id`, `summary`, `description`, `flags`, `examples`, optional `category` | Missing list falls back to the snapshot |
| `<command> --help` | plain text | Flag lines such as `-o, --target-org <value> ... (required)` are parsed for the detail pane |
| `org list --json` | `result.scratchOrgs[]`, `result.nonScratchOrgs[]` | `aliases` may be a string or array; flags `isDefaultUsername`, `isDefaultDevHubUsername` |
| `org display --json` | `result{}` | Filter `accessToken`, `refreshToken`, `sfdxAuthUrl`, `password`, `clientSecret`, `privateKey` |
| `config get target-org --json` | `result.target-org`, a string, or an array of `{name,value}` entries | Probed at startup to show the current org; falls back to `org display --json` |
| `project deploy start --json` | `result.id` | Job id drives `project deploy report --json --job-id <id>`; flags: `--source-dir`, `--dry-run`, `--metadata`, `--test-level` |
| component completion | all project files (cap 50k), package.xml `<types>`, common metadata types | `src/sf/metadata.rs` detects Apex by extension, ranks `Type:Name` candidates with a fast substring pass, shows source paths; `PromptField` debounces re-ranking by 90 ms and supports multi-select with Space |
| `project deploy report --json` | `result.status`, `result.components` | `components` may be counters (`total`, `successes`, `failed`) or an array of per-component objects |
| `project retrieve start --json` | final result directly | There is no `project retrieve report`; run it with `--wait` and read the returned components |

Never assume an org alias is a username. Keep the original username for all CLI invocations and
use aliases only as labels.

## Catalog snapshot refresh

`data/commands.json` is a compact array of
`{id, summary, description, category, flags: [[name, description]], examples: [[command, description]]}`.
It is generated from the public sfcli command reference:

1. Read the public command reference JSON (`id`, `summary`, `description`, `flags`, `examples`).
2. Normalize ids from `org:list` to `org list`; derive a readable `category` from the id prefix.
3. Keep only the fields above; write compact JSON to `data/commands.json`.
4. Run `cargo test sf::catalog` and check that the catalog has more than 250 commands.

The bundled snapshot is always used at startup. A cached `sf commands --json` response is read
when it is younger than seven days, and F5 in the Catalog tab performs an explicit live refresh
that replaces the catalog and updates the cache. JSON extraction tolerates bracketed warning
lines before the document.

## Synthetic stub fixtures

`tests/fixtures/stub_sf.sh` and `stub_sf.bat` emulate the commands above. Rules:

- Use only reserved example domains (`.invalid`), synthetic usernames and fake org ids.
- Never copy a real org id, username, access token, auth URL or customer name into a fixture.
- Tests must refuse to start if `SF_ACCESS_TOKEN`, `SFDX_AUTH_URL`, `SF_PASSWORD` or
  `SF_CLIENT_SECRET` is present in the environment.
- When a new command shape is consumed by the TUI, add a matching stub response and a parser unit
  test in the same change.
- Keep the shell script executable in git (`100755`).
