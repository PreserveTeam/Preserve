param(
  [switch]$Locked,
  [string]$Version
)

$ErrorActionPreference = "Stop"
$repository = (Resolve-Path $PSScriptRoot).Path
$remaps = [System.Collections.Generic.List[string]]::new()
$remaps.Add("--remap-path-prefix=$repository=.")

if ($env:USERPROFILE) {
  $remaps.Add("--remap-path-prefix=$env:USERPROFILE=<user>")
}
if ($env:CARGO_HOME) {
  $remaps.Add("--remap-path-prefix=$env:CARGO_HOME=<cargo>")
}

$previousEncodedFlags = $env:CARGO_ENCODED_RUSTFLAGS
$previousAppVersion = $env:PRESERVE_APP_VERSION
try {
  $separator = [char]0x1f
  $encoded = $remaps -join $separator
  if ($previousEncodedFlags) {
    $encoded = "$previousEncodedFlags$separator$encoded"
  }
  $env:CARGO_ENCODED_RUSTFLAGS = $encoded
  if ($Version) { $env:PRESERVE_APP_VERSION = $Version }

  $arguments = @("build", "--release", "--manifest-path", (Join-Path $repository "Cargo.toml"))
  if ($Locked) { $arguments += "--locked" }
  & cargo @arguments
  if ($LASTEXITCODE -ne 0) { throw "Release build failed." }

  $executable = Join-Path $repository "target\release\preserve.exe"
  $binaryText = [Text.Encoding]::ASCII.GetString([IO.File]::ReadAllBytes($executable))
  $forbiddenPaths = @($repository, $env:USERPROFILE, $env:CARGO_HOME) |
    Where-Object { $_ } |
    Select-Object -Unique
  foreach ($path in $forbiddenPaths) {
    if ($binaryText.IndexOf($path, [StringComparison]::OrdinalIgnoreCase) -ge 0) {
      throw "Release executable contains a local build path."
    }
  }
  if ($binaryText -match '(?i)[A-Z]:\\Users\\[^\\]+|/home/[^/]+') {
    throw "Release executable contains a user home path."
  }
} finally {
  $env:CARGO_ENCODED_RUSTFLAGS = $previousEncodedFlags
  $env:PRESERVE_APP_VERSION = $previousAppVersion
}
