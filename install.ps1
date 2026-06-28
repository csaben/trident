# trident installer for native Windows PowerShell.
#
#   irm https://raw.githubusercontent.com/csaben/trident/main/install.ps1 | iex
#   trident host                 # this machine runs the hub + a session
#   trident join http://IP:8790  # other machines point at the hub
#
# Env overrides: $env:TRIDENT_BIN_DIR, $env:TRIDENT_VERSION
$ErrorActionPreference = 'Stop'

$repo    = 'csaben/trident'
$dest    = if ($env:TRIDENT_BIN_DIR) { $env:TRIDENT_BIN_DIR } else { "$env:USERPROFILE\.trident\bin" }
$version = if ($env:TRIDENT_VERSION) { $env:TRIDENT_VERSION } else { 'latest' }

# Only x86_64 Windows binaries are published today.
if (-not [Environment]::Is64BitOperatingSystem) {
  throw 'trident: only 64-bit Windows is supported by the prebuilt binary.'
}
$target = 'x86_64-pc-windows-msvc'
$asset  = "trident-$target.zip"
$url = if ($version -eq 'latest') {
  "https://github.com/$repo/releases/latest/download/$asset"
} else {
  "https://github.com/$repo/releases/download/$version/$asset"
}

$tmp = Join-Path $env:TEMP ("trident-" + [guid]::NewGuid())
New-Item -ItemType Directory -Force -Path $tmp | Out-Null
try {
  Write-Host "▸ Downloading $asset" -ForegroundColor Cyan
  Invoke-WebRequest -Uri $url -OutFile "$tmp\$asset"

  Write-Host "▸ Extracting" -ForegroundColor Cyan
  Expand-Archive -Path "$tmp\$asset" -DestinationPath $tmp -Force

  New-Item -ItemType Directory -Force -Path $dest | Out-Null
  Copy-Item "$tmp\trident.exe" "$dest\trident.exe" -Force
}
finally {
  Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
}

Write-Host "`n✓ trident installed to $dest\trident.exe" -ForegroundColor Green

# Add to the user PATH if missing.
$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
if ($userPath -notlike "*$dest*") {
  [Environment]::SetEnvironmentVariable('Path', "$userPath;$dest", 'User')
  Write-Host "  added $dest to your user PATH (restart the terminal to pick it up)" -ForegroundColor Yellow
}

Write-Host @"

Next steps:
  trident host                  # this machine runs the hub + launches a session
  trident join http://IP:8790   # other machines: point at the hub's tailnet IP
"@
