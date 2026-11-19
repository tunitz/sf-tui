#!/bin/sh
# Synthetic SF CLI stub used by integration tests. Contains no real credentials or org data.

case "$*" in
  "version --json")
    printf '%s\n' '{"status":0,"result":{"version":"99.0.0"}}'
    ;;
  "config get target-org --json" | "org display --json")
    printf '%s\n' '{"status":0,"result":{"target-org":"test-scratch"}}'
    ;;
  "org list --json")
    printf '%s\n' '{"status":0,"result":{"scratchOrgs":[{"username":"test-scratch@example.invalid","aliases":["test-scratch"],"orgId":"00D000000000001","isDefaultUsername":true,"connectedStatus":"Connected","instanceUrl":"https://example.invalid.my.salesforce.com"}],"nonScratchOrgs":[]}}'
    ;;
  "org display --target-org test-scratch --json")
    printf '%s\n' '{"status":0,"result":{"username":"test-scratch@example.invalid","instanceUrl":"https://example.invalid.my.salesforce.com","id":"00D000000000001","accessToken":"SYNTHETIC-DO-NOT-USE"}}'
    ;;
  "org list metadata --metadata-type ApexClass --target-org test-scratch@example.invalid --json")
    printf '%s\n' '{"status":0,"result":[{"type":"ApexClass","fullName":"Demo"},{"type":"ApexClass","fullName":"OrgOnlyClass"}]}'
    ;;
  "org list metadata --metadata-type ApexTrigger --target-org test-scratch@example.invalid --json")
    printf '%s\n' '{"status":0,"result":[{"type":"ApexTrigger","fullName":"DemoTrigger"}]}'
    ;;
  "org list metadata --metadata-type Flow --target-org test-scratch@example.invalid --json")
    printf '%s\n' '{"status":0,"result":[{"type":"Flow","fullName":"Demo Flow"}]}'
    ;;
  "org list metadata --metadata-type PermissionSet --target-org test-scratch@example.invalid --json")
    printf '%s\n' '{"status":0,"result":[{"type":"PermissionSet","fullName":"DemoPerm"}]}'
    ;;
  "org list metadata --metadata-type CustomObject --target-org test-scratch@example.invalid --json")
    printf '%s\n' '{"status":0,"result":[{"type":"CustomObject","fullName":"Demo__c"}]}'
    ;;
  "org list metadata --metadata-type LightningComponentBundle --target-org test-scratch@example.invalid --json")
    printf '%s\n' '{"status":0,"result":[{"type":"LightningComponentBundle","fullName":"demoCard"}]}'
    ;;
  "project deploy start --source-dir force-app --json" | "project deploy start --source-dir force-app --target-org test-scratch --json" | "project deploy start --metadata ApexClass --target-org test-scratch --json")
    printf '%s\n' '{"status":0,"result":{"id":"0Af000000000001","status":"Queued"}}'
    ;;
  "project retrieve start --metadata ApexClass --json" | "project retrieve start --metadata CustomLabel --target-org test-scratch --json")
    printf '%s\n' '{"status":0,"result":{"id":"0Af000000000002","status":"Succeeded"}}'
    ;;
  "apex run test --result-format json --code-coverage --class-names DemoTest --target-org test-scratch --json")
    printf '%s\n' '{"status":0,"result":{"testRunId":"08N000000000001","summary":{"outcome":"Processing","testsRan":0,"passing":0,"failing":0,"skipped":0}}}'
    ;;
  "apex get test --test-run-id 08N000000000001 --result-format json --code-coverage --target-org test-scratch --json")
    printf '%s\n' '{"status":0,"result":{"testRunId":"08N000000000001","summary":{"outcome":"Passed","testsRan":2,"passing":2,"failing":0,"skipped":0,"testRunCoverage":"60%"},"tests":[{"fullName":"DemoTest.ok","outcome":"Pass"},{"fullName":"DemoTest.alsoOk","outcome":"Pass"}],"coverage":{"coverage":[{"name":"DemoService","totalLines":20,"totalCovered":8,"coveredPercent":40},{"name":"DemoHelper","totalLines":10,"totalCovered":10,"coveredPercent":100}],"records":[],"summary":{"totalLines":30,"coveredLines":18,"testRunCoverage":"60%"}}}}'
    ;;
  "apex run test --result-format json --code-coverage --class-names MixedTest --target-org test-scratch --json")
    printf '%s\n' '{"status":0,"result":{"testRunId":"08N000000000002","summary":{"outcome":"Processing","testsRan":0,"passing":0,"failing":0,"skipped":0}}}'
    ;;
  "apex get test --test-run-id 08N000000000002 --result-format json --code-coverage --target-org test-scratch --json")
    printf '%s\n' '{"status":100,"result":{"testRunId":"08N000000000002","summary":{"outcome":"Failed","testsRan":3,"passing":2,"failing":1,"skipped":0,"testRunCoverage":"75%"},"tests":[{"fullName":"AlphaTest.ok","outcome":"Pass"},{"fullName":"AlphaTest.alsoOk","outcome":"Pass"},{"fullName":"BetaTest.bad","outcome":"Fail","message":"System.AssertException: synthetic assertion failed"}],"coverage":{"coverage":[{"name":"BetaService","totalLines":4,"totalCovered":3,"coveredPercent":75}],"records":[],"summary":{"totalLines":4,"coveredLines":3,"testRunCoverage":"75%"}}}}'
    exit 100
    ;;
  "apex run test --result-format json --code-coverage --class-names ErrorTest --target-org test-scratch --json")
    printf '%s\n' '{"status":0,"result":{"testRunId":"08N000000000003","summary":{"outcome":"Processing","testsRan":0,"passing":0,"failing":0,"skipped":0}}}'
    ;;
  "apex get test --test-run-id 08N000000000003 --result-format json --code-coverage --target-org test-scratch --json")
    printf '%s\n' '{"status":100,"name":"SfError","message":"synthetic report failure","exitCode":100}'
    exit 100
    ;;
  "project deploy report --job-id 0Af000000000001 --json" | "project deploy report --job-id 0Af000000000001 --target-org test-scratch --json")
    printf '%s\n' '{"status":0,"result":{"id":"0Af000000000001","status":"Succeeded","components":{"total":2,"successes":2,"failed":0}}}'
    ;;
  "project deploy cancel --job-id 0Af000000000001 --target-org test-scratch --async --json")
    printf '%s\n' '{"status":0,"result":{"id":"0Af000000000001","status":"CancelRequested"}}'
    ;;
  "project deploy cancel --job-id 0Af000000000002 --target-org test-scratch --async --json")
    printf '%s\n' '{"status":0,"result":{"id":"0Af000000000002","status":"CancelRequested"}}'
    ;;
  project*retrieve*start*metadata*)
    printf '%s\n' '{"status":0,"result":{"status":"Succeeded","components":{"total":1,"successes":1,"failed":0}}}'
    ;;
  "slow")
    # Synthetic long-running command used by process-cancellation tests only.
    sleep 30
    ;;
  template\ generate\ *)
    printf '%s\n' '{"status":0,"result":{"generated":true}}'
    ;;
  "cmdt generate object --type-name"* | "schema generate sobject --label"*)
    printf '%s\n' '{"status":0,"result":{"generated":true}}'
    ;;
  "echo-error")
    printf '%s\n' 'synthetic failure from stub' >&2
    exit 3
    ;;
  *)
    printf 'unrecognized stub invocation: %s\n' "$*" >&2
    exit 2
    ;;
esac
