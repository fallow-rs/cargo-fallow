### Performance

- The memory of `cargo fallow visibility` now grows about linearly with the
  source size. A field access, a method call or a name in macro tokens gave
  one credit to each item with the name, so a generated file with many fields
  of one name needed memory for each pair. Each name now keeps its items once.
  On a generated 5 MB file, the peak memory goes from 8.9 GB to 1.5 GB, and
  the time from 33 s to 1.5 s.
