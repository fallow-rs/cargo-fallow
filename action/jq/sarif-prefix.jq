# Put $prefix in front of each artifact path of a SARIF log.
#
# cargo-fallow writes each path relative to the workspace root. Code scanning
# reads a relative path from the repository root, so a workspace in a
# subfolder needs the path of that folder in front.

(.. | objects | select(.artifactLocation.uri? | type == "string") | .artifactLocation.uri) |= ($prefix + .)
