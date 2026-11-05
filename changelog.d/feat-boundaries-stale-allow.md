### Added

- A `boundaries.sources` rule reports an `allow` entry whose file has fewer
  matches than `max`, with the actual count. The finding has the kind
  `allow-entry`, so the list stays exact.
- An `allow` entry takes `require = ["pattern"]`. The entry then passes only
  when the file contains each required pattern.
