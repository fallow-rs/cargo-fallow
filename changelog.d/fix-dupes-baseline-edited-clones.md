### Fixed

- A `dupes` baseline now keeps a saved clone group after an edit inside its
  copies. The baseline entry of a clone group saves the file, the lines and
  the functions of each copy. A group with a new fingerprint matches a saved
  group when each copy is in the same file and functions. Save the baseline
  again to add this data. See
  [Clone groups in a baseline](docs/structure.md#clone-groups-in-a-baseline).
- A baseline no longer hides a new identical copy of a saved clone group. A
  group with more copies than its saved entry is new duplication.
