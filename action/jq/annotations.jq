# Workflow commands for the results of a cargo-fallow SARIF log.
#
# Input: a SARIF 2.1.0 log. Arguments: $max (the most lines to write) and
# $prefix (the path from the repository root to the workspace root, with a
# trailing `/`, or an empty string).
#
# The SARIF log is the one location contract that each command shares, so
# one filter serves each command.

def data_escape: gsub("%"; "%25") | gsub("\r"; "%0D") | gsub("\n"; "%0A");
def property_escape: data_escape | gsub(":"; "%3A") | gsub(","; "%2C");

# GitHub shows a limited number of annotations of each level for a step, so
# the errors come first.
def rank: {"error": 0, "warning": 1}[.level // "warning"] // 2;

def command_of($level):
  if $level == "error" then "error"
  elif $level == "warning" then "warning"
  else "notice"
  end;

[.runs[]?.results[]?]
| sort_by(rank)
| .[0:($max | tonumber)][]
| (.locations[0].physicalLocation // {}) as $where
| ($where.region // {}) as $region
| [
    (if $where.artifactLocation.uri then "file=" + (($prefix + $where.artifactLocation.uri) | property_escape) else empty end),
    (if $region.startLine then "line=\($region.startLine)" else empty end),
    (if $region.startColumn then "col=\($region.startColumn)" else empty end),
    (if $region.endLine then "endLine=\($region.endLine)" else empty end),
    ("title=" + ("cargo-fallow " + (.ruleId // "finding") | property_escape))
  ] as $properties
| "::\(command_of(.level // "warning")) \($properties | join(","))::\(.message.text // "" | data_escape)"
