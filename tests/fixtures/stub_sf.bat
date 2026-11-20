@echo off
REM Synthetic SF CLI stub used by integration tests on Windows.
REM Contains no real credentials or org data.

set "ARGS=%*"

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
if "%*"=="org list metadata --metadata-type ApexClass --target-org test-scratch@example.invalid --json" (
  echo {"status":0,"result":[{"type":"ApexClass","fullName":"Demo"},{"type":"ApexClass","fullName":"OrgOnlyClass"}]}
  exit /b 0
)
if "%*"=="org list metadata --metadata-type ApexTrigger --target-org test-scratch@example.invalid --json" (
  echo {"status":0,"result":[{"type":"ApexTrigger","fullName":"DemoTrigger"}]}
  exit /b 0
)
if "%*"=="org list metadata --metadata-type Flow --target-org test-scratch@example.invalid --json" (
  echo {"status":0,"result":[{"type":"Flow","fullName":"Demo Flow"}]}
  exit /b 0
)
if "%*"=="org list metadata --metadata-type PermissionSet --target-org test-scratch@example.invalid --json" (
  echo {"status":0,"result":[{"type":"PermissionSet","fullName":"DemoPerm"}]}
  exit /b 0
)
if "%*"=="org list metadata --metadata-type CustomObject --target-org test-scratch@example.invalid --json" (
  echo {"status":0,"result":[{"type":"CustomObject","fullName":"Demo__c"}]}
  exit /b 0
)
if "%*"=="org list metadata --metadata-type LightningComponentBundle --target-org test-scratch@example.invalid --json" (
  echo {"status":0,"result":[{"type":"LightningComponentBundle","fullName":"demoCard"}]}
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
  echo {"status":0,"result":{"testRunId":"08N000000000001","summary":{"outcome":"Passed","testsRan":2,"passing":2,"failing":0,"skipped":0,"testRunCoverage":"60%"},"tests":[{"fullName":"DemoTest.ok","outcome":"Pass"},{"fullName":"DemoTest.alsoOk","outcome":"Pass"}],"coverage":{"coverage":[{"name":"DemoService","totalLines":20,"totalCovered":8,"coveredPercent":40},{"name":"DemoHelper","totalLines":10,"totalCovered":10,"coveredPercent":100}],"records":[],"summary":{"totalLines":30,"coveredLines":18,"testRunCoverage":"60%"}}}}
  exit /b 0
)
if "%*"=="apex run test --result-format json --code-coverage --class-names MixedTest --target-org test-scratch --json" (
  echo {"status":0,"result":{"testRunId":"08N000000000002","summary":{"outcome":"Processing","testsRan":0,"passing":0,"failing":0,"skipped":0}}}
  exit /b 0
)
if "%*"=="apex get test --test-run-id 08N000000000002 --result-format json --code-coverage --target-org test-scratch --json" (
  echo {"status":100,"result":{"testRunId":"08N000000000002","summary":{"outcome":"Failed","testsRan":3,"passing":2,"failing":1,"skipped":0,"testRunCoverage":"75%"},"tests":[{"fullName":"AlphaTest.ok","outcome":"Pass"},{"fullName":"AlphaTest.alsoOk","outcome":"Pass"},{"fullName":"BetaTest.bad","outcome":"Fail","message":"System.AssertException: synthetic assertion failed"}],"coverage":{"coverage":[{"name":"BetaService","totalLines":4,"totalCovered":3,"coveredPercent":75}],"records":[],"summary":{"totalLines":4,"coveredLines":3,"testRunCoverage":"75%"}}}}
  exit /b 100
)
if "%*"=="apex run test --result-format json --code-coverage --class-names ErrorTest --target-org test-scratch --json" (
  echo {"status":0,"result":{"testRunId":"08N000000000003","summary":{"outcome":"Processing","testsRan":0,"passing":0,"failing":0,"skipped":0}}}
  exit /b 0
)
if "%*"=="apex get test --test-run-id 08N000000000003 --result-format json --code-coverage --target-org test-scratch --json" (
  echo {"status":100,"name":"SfError","message":"synthetic report failure","exitCode":100}
  exit /b 100
)
if "%*"=="project deploy report --job-id 0Af000000000001 --json" (
  echo {"status":0,"result":{"id":"0Af000000000001","status":"Succeeded","components":{"total":2,"successes":2,"failed":0}}}
  exit /b 0
)
if "%*"=="project deploy report --job-id 0Af000000000001 --target-org test-scratch --json" (
  echo {"status":0,"result":{"id":"0Af000000000001","status":"Succeeded","components":{"total":2,"successes":2,"failed":0}}}
  exit /b 0
)
if "%*"=="project deploy cancel --job-id 0Af000000000001 --target-org test-scratch --async --json" (
  echo {"status":0,"result":{"id":"0Af000000000001","status":"CancelRequested"}}
  exit /b 0
)
if "%*"=="project deploy cancel --job-id 0Af000000000002 --target-org test-scratch --async --json" (
  echo {"status":0,"result":{"id":"0Af000000000002","status":"CancelRequested"}}
  exit /b 0
)
if "%ARGS:~0,18%"=="template generate " (
  echo {"status":0,"result":{"generated":true}}
  exit /b 0
)
if "%ARGS:~0,32%"=="cmdt generate object --type-name" (
  echo {"status":0,"result":{"generated":true}}
  exit /b 0
)
if "%ARGS:~0,31%"=="schema generate sobject --label" (
  echo {"status":0,"result":{"generated":true}}
  exit /b 0
)
if "%*"=="echo-error" (
  echo synthetic failure from stub 1>&2
  exit /b 3
)
echo unrecognized stub invocation: %* 1>&2
exit /b 2
