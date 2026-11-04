### Performance

- `cargo fallow check` parses each file one time for the module graph and the
  `files` check. Before, the `files` check read and parsed each file a second
  time. On a large workspace, the run uses about 14% less CPU time. The output
  does not change.
