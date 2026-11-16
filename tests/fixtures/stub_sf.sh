#!/bin/sh
# Synthetic SF CLI stub used by integration tests. Contains no real credentials or org data.

case "$*" in
  "version --json")
    printf '%s\n' '{"status":0,"result":{"version":"99.0.0"}}'
    ;;
  "commands --json")
    printf '%s\n' '{"status":0,"result":[{"id":"org list","summary":"List orgs","category":"Orgs","flags":[{"name":"json","description":"JSON output","type":"boolean","required":false}],"examples":[["sf org list","List orgs"]]}]}'
    ;;
  "org list --json")
    printf '%s\n' '{"status":0,"result":{"scratchOrgs":[{"username":"test-scratch@example.invalid","aliases":["test-scratch"],"orgId":"00D000000000001","isDefaultUsername":true,"connectedStatus":"Connected","instanceUrl":"https://example.invalid.my.salesforce.com"}],"nonScratchOrgs":[]}}'
    ;;
  "org display --target-org test-scratch --json")
    printf '%s\n' '{"status":0,"result":{"username":"test-scratch@example.invalid","instanceUrl":"https://example.invalid.my.salesforce.com","id":"00D000000000001","accessToken":"SYNTHETIC-DO-NOT-USE"}}'
    ;;
  "project deploy start --source-dir force-app --json" | "project deploy start --source-dir force-app --target-org test-scratch --json")
    printf '%s\n' '{"status":0,"result":{"id":"0Af000000000001","status":"Queued"}}'
    ;;
  "project retrieve start --metadata ApexClass --json")
    printf '%s\n' '{"status":0,"result":{"id":"0Af000000000002","status":"Succeeded"}}'
    ;;
  "apex run test --result-format json --wait 10 --json")
    printf '%s\n' '{"status":0,"result":{"testRunId":"08N000000000001","summary":{"outcome":"Passed"}}}'
    ;;
  "logic get test --test-run-id 08N000000000001 --json")
    printf '%s\n' '{"status":0,"result":{"testRunId":"08N000000000001","summary":{"outcome":"Passed"}}}'
    ;;
  "project deploy report --job-id 0Af000000000001 --json")
    printf '%s\n' '{"status":0,"result":{"id":"0Af000000000001","status":"Succeeded","components":{"total":2,"successes":2,"failed":0}}}'
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
