### Performance

- The GitHub Action accepts `sarif-output: when-needed` to skip the additional SARIF analysis for clean checks that do not request SARIF output, annotations or code-scanning uploads. The default outputs and diagnostics for runs with findings stay unchanged.
