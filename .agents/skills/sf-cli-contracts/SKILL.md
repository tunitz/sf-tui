---
name: sf-cli-contracts
description: JSON shapes the TUI relies on from the Salesforce CLI and how to regenerate synthetic test fixtures. Use when changing parsing in src/sf or fixtures in tests.
---

# Salesforce CLI contracts

## Endpoint shapes

All parsers accept both a bare JSON value and a `{ "status": ..., "result": ... }` wrapper.
Fields absent in one CLI version must degrade to empty values rather than fail.

| Command | Consumed shape | Notes |
| --- | --- | --- |
| `version --json` | `result.version` or a plain version token | Probe runs with a 5 s timeout |
| `apex run test --json` | `result.testRunId`, optional `summary`/`tests`/`codecoverage` | Runs without `--wait` and returns at once; the summary outcome is `Processing`/`Queued` until the org finishes |
| `apex get test --test-run-id <id> --code-coverage --json` | `result.summary`, `result.tests[]`, `result.codecoverage[]` | Polled every 3 s until the outcome is terminal; `codecoverage` carries per-class `percentage`, `numLinesCovered`, `numLinesUncovered` |
| `org list --json` | `result.scratchOrgs[]`, `result.nonScratchOrgs[]` | `aliases` may be a string or array; flags `isDefaultUsername`, `isDefaultDevHubUsername` |
| `org display --json` | `result{}` | Filter `accessToken`, `refreshToken`, `sfdxAuthUrl`, `password`, `clientSecret`, `privateKey` |
| `config get target-org --json` | `result.target-org`, a string, or an array of `{name,value}` entries | Probed at startup to show the current org without running `org list` |
| `project deploy start --json` | `result.id` | Job id drives `project deploy report --json --job-id <id>`; flags: `--source-dir`, `--dry-run`, `--metadata`, `--test-level` |
| component completion | all project files (cap 50k), package.xml `<types>`, common metadata types | `src/sf/metadata.rs` detects Apex by extension, ranks concrete `Type:Name` candidates above bare metadata types with a fast substring pass, shows source paths; `PromptField` debounces re-ranking by 90 ms and supports multi-select with Space |
| `project deploy report --json` | `result.status`, `result.components` | `components` may be counters (`total`, `successes`, `failed`) or an array of per-component objects |
| `project retrieve start --json` | final result directly | There is no `project retrieve report`; the command waits for the retrieve and returns the components |

Never assume an org alias is a username. Keep the original username for all CLI invocations and
use aliases only as labels.

## Synthetic stub fixtures

`tests/fixtures/stub_sf.sh` and `stub_sf.bat` emulate the commands above. Rules:

- Use only reserved example domains (`.invalid`), synthetic usernames and fake org ids.
- Never copy a real org id, username, access token, auth URL or customer name into a fixture.
- Tests must refuse to start if `SF_ACCESS_TOKEN`, `SFDX_AUTH_URL`, `SF_PASSWORD` or
  `SF_CLIENT_SECRET` is present in the environment.
- When a new command shape is consumed by the TUI, add a matching stub response and a parser unit
  test in the same change.
- Keep the shell script executable in git (`100755`).
