@echo off
REM Synthetic SF CLI stub used by integration tests on Windows.
REM Contains no real credentials or org data.

if "%*"=="version --json" (
  echo {"status":0,"result":{"version":"99.0.0"}}
  exit /b 0
)
if "%*"=="commands --json" (
  echo {"status":0,"result":[{"id":"org list","summary":"List orgs","category":"Orgs","flags":[{"name":"json","description":"JSON output","type":"boolean","required":false}],"examples":[["sf org list","List orgs"]]}]}
  exit /b 0
)
if "%*"=="config get target-org --json" (
  echo {"status":0,"result":{"target-org":"test-scratch"}}
  exit /b 0
)
if "%*"=="org list --json" (
  echo {"status":0,"result":{"scratchOrgs":[{"username":"test-scratch@example.invalid","aliases":["test-scratch"],"orgId":"00D000000000001","isDefaultUsername":true,"connectedStatus":"Connected","instanceUrl":"https://example.invalid.my.salesforce.com"}],"nonScratchOrgs":[]}}
  exit /b 0
)
if "%*"=="project deploy start --source-dir force-app --json" (
  echo {"status":0,"result":{"id":"0Af000000000001","status":"Queued"}}
  exit /b 0
)
if "%*"=="project retrieve start --metadata ApexClass --json" (
  echo {"status":0,"result":{"id":"0Af000000000002","status":"Succeeded"}}
  exit /b 0
)
if "%*"=="apex run test --result-format json --wait 10 --json" (
  echo {"status":0,"result":{"testRunId":"08N000000000001","summary":{"outcome":"Passed"}}}
  exit /b 0
)
if "%*"=="logic get test --test-run-id 08N000000000001 --json" (
  echo {"status":0,"result":{"testRunId":"08N000000000001","summary":{"outcome":"Passed"}}}
  exit /b 0
)
if "%*"=="echo-error" (
  echo synthetic failure from stub 1>&2
  exit /b 3
)
echo unrecognized stub invocation: %* 1>&2
exit /b 2
