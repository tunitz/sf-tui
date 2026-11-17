@echo off
REM Synthetic SF CLI stub used by integration tests on Windows.
REM Contains no real credentials or org data.

if "%*"=="version --json" (
  echo {"status":0,"result":{"version":"99.0.0"}}
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
if "%*"=="org display --target-org test-scratch --json" (
  echo {"status":0,"result":{"username":"test-scratch@example.invalid","instanceUrl":"https://example.invalid.my.salesforce.com","id":"00D000000000001","accessToken":"SYNTHETIC-DO-NOT-USE"}}
  exit /b 0
)
if "%*"=="project deploy start --source-dir force-app --json" (
  echo {"status":0,"result":{"id":"0Af000000000001","status":"Queued"}}
  exit /b 0
)
if "%*"=="project deploy start --source-dir force-app --target-org test-scratch --json" (
  echo {"status":0,"result":{"id":"0Af000000000001","status":"Queued"}}
  exit /b 0
)
if "%*"=="project deploy start --metadata ApexClass --target-org test-scratch --json" (
  echo {"status":0,"result":{"id":"0Af000000000001","status":"Queued"}}
  exit /b 0
)
if "%*"=="project retrieve start --metadata ApexClass --json" (
  echo {"status":0,"result":{"id":"0Af000000000002","status":"Succeeded"}}
  exit /b 0
)
if "%*"=="project retrieve start --metadata CustomLabel --target-org test-scratch --json" (
  echo {"status":0,"result":{"id":"0Af000000000002","status":"Succeeded"}}
  exit /b 0
)
if "%*"=="apex run test --result-format json --code-coverage --class-names DemoTest --target-org test-scratch --json" (
  echo {"status":0,"result":{"testRunId":"08N000000000001","summary":{"outcome":"Processing","testsRan":0,"passing":0,"failing":0,"skipped":0}}}
  exit /b 0
)
if "%*"=="apex get test --test-run-id 08N000000000001 --result-format json --code-coverage --target-org test-scratch --json" (
  echo {"status":0,"result":{"testRunId":"08N000000000001","summary":{"outcome":"Passed","testsRan":2,"passing":2,"failing":0,"skipped":0,"testRunCoverage":"81%"},"tests":[{"fullName":"DemoTest.ok","outcome":"Pass"},{"fullName":"DemoTest.alsoOk","outcome":"Pass"}],"codecoverage":[{"name":"Demo","percentage":"81%","numLinesCovered":17,"numLinesUncovered":4},{"name":"DemoHelper","percentage":"44%","numLinesCovered":4,"numLinesUncovered":5}]}}
  exit /b 0
)
if "%*"=="project deploy report --job-id 0Af000000000001 --json" (
  echo {"status":0,"result":{"id":"0Af000000000001","status":"Succeeded","components":{"total":2,"successes":2,"failed":0}}}
  exit /b 0
)
if "%*"=="project deploy report --job-id 0Af000000000001 --target-org test-scratch --json" (
  echo {"status":0,"result":{"id":"0Af000000000001","status":"Succeeded","components":{"total":2,"successes":2,"failed":0}}}
  exit /b 0
)
if "%*"=="echo-error" (
  echo synthetic failure from stub 1>&2
  exit /b 3
)
echo unrecognized stub invocation: %* 1>&2
exit /b 2
