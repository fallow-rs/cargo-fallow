### Fixed

- The GitHub Action and the GitLab CI template now work with a jq that ends
  each line with CRLF, as jq on Windows does. The gate outputs, the
  annotation count, the job summary and the GitLab note pages no longer keep
  a CR. With bash 3.2, the GitLab comment step could loop without end. A
  comment compare now ignores each CR. The script tests run a second time with such a
  jq.
