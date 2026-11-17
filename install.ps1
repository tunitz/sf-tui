# Install sftui into a user-writable directory and register it in the user PATH.
# No elevation, administrator rights or system directories are used.

$ErrorActionPreference = 'Stop'

$Repository = 'tunitz/sf-tui'
$Target = 'x86_64-pc-windows-msvc'
$Version = if ($args.Count -gt 0) { $args[0].TrimStart('v') } else { $null }
$InstallDir = if ($env:SFTUI_INSTALL_DIR) { $env:SFTUI_INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA 'Programs\sftui' }

if (-not $Version) {
  $release = Invoke-RestMethod -Uri "https://api.github.com/repos/$Repository/releases/latest" -Headers @{ 'User-Agent' = 'sftui-installer' }
  $Version = $release.tag_name.TrimStart('v')
}

if (-not $Version) {
  throw 'Could not determine the latest sftui version. Pass a version argument and retry.'
}

$asset = "sftui-$Version-$Target.zip"
$url = "https://github.com/$Repository/releases/download/v$Version/$asset"
$temp = Join-Path ([System.IO.Path]::GetTempPath()) ([guid]::NewGuid().ToString())
New-Item -ItemType Directory -Path $temp -Force | Out-Null
try {
  $zip = Join-Path $temp $asset
  Write-Host "Downloading $asset…"
  Invoke-WebRequest -Uri $url -OutFile $zip -UseBasicParsing
  $sumsUrl = "https://github.com/$Repository/releases/download/v$Version/SHA256SUMS"
  $sums = Join-Path $temp 'SHA256SUMS'
  try {
    Invoke-WebRequest -Uri $sumsUrl -OutFile $sums -UseBasicParsing
  } catch {
    $sums = $null
  }
  if ($sums -and (Test-Path $sums)) {
    $line = Get-Content $sums | Where-Object { $_ -match "  $([regex]::Escape($asset))$" } | Select-Object -First 1
    if ($line) {
      $expected = ($line -split '\s+')[0].ToLowerInvariant()
      $actual = (Get-FileHash -Path $zip -Algorithm SHA256).Hash.ToLowerInvariant()
      if ($expected -ne $actual) {
        throw 'Checksum verification failed for the downloaded archive.'
      }
    }
  }
  Expand-Archive -Path $zip -DestinationPath $temp -Force
  $binary = Join-Path $temp 'sftui.exe'
  if (-not (Test-Path $binary)) {
    throw 'The release archive did not contain sftui.exe'
  }
  New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
  Copy-Item -Path $binary -Destination (Join-Path $InstallDir 'sftui.exe') -Force
} finally {
  Remove-Item -Path $temp -Recurse -Force -ErrorAction SilentlyContinue
}

$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
$entries = if ($userPath) { $userPath -split ';' | Where-Object { $_ } } else { @() }
if (-not ($entries | Where-Object { $_.TrimEnd('\') -ieq $InstallDir.TrimEnd('\') })) {
  $entries += $InstallDir
  [Environment]::SetEnvironmentVariable('Path', ($entries -join ';'), 'User')
  Write-Host "Added $InstallDir to your user PATH"
}

Write-Host "Installed sftui $Version to $InstallDir\sftui.exe"
Write-Host 'Open a new terminal, then type sftui'
