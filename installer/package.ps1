# Build the installer and the portable zip (with SHA-256 checksums) from a
# built MdNotePad.exe and mdnotepad_preview.dll.  Used by CI and releases.
#
#   installer\package.ps1 -SourceDir target\release -OutDir dist
param(
  [Parameter(Mandatory = $true)] [string] $SourceDir,
  [string] $OutDir = 'dist'
)
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$version = (Select-String -Path (Join-Path $root 'Cargo.toml') -Pattern '^version = "(.+)"').Matches[0].Groups[1].Value
New-Item -ItemType Directory -Force $OutDir | Out-Null
$OutDir = (Resolve-Path $OutDir).Path
$SourceDir = (Resolve-Path $SourceDir).Path

# Installer: Inno Setup 6 (installed when the machine has none).
$iscc = Join-Path ${env:ProgramFiles(x86)} 'Inno Setup 6\ISCC.exe'
if (-not (Test-Path $iscc)) {
  choco install innosetup --no-progress -y | Out-Null
}
& $iscc "/DAppVersion=$version" "/DSourceDir=$SourceDir" "/O$OutDir" (Join-Path $PSScriptRoot 'mdnotepad.iss')
if ($LASTEXITCODE -ne 0) { throw "Inno Setup failed ($LASTEXITCODE)" }

# Portable zip: the exe and the portable marker (no preview DLL).
$staging = Join-Path ([IO.Path]::GetTempPath()) 'mdnotepad-portable'
Remove-Item -Recurse -Force $staging -ErrorAction SilentlyContinue
New-Item -ItemType Directory $staging | Out-Null
Copy-Item (Join-Path $SourceDir 'MdNotePad.exe') $staging
Copy-Item (Join-Path $PSScriptRoot 'portable.txt') $staging
Copy-Item (Join-Path $root 'LICENSE'), (Join-Path $root 'THIRD-PARTY-NOTICES.md') $staging
Compress-Archive -Path (Join-Path $staging '*') -DestinationPath (Join-Path $OutDir "MdNotePad-$version-portable.zip") -Force

# Checksums.
Get-ChildItem $OutDir -File | Where-Object { $_.Extension -in '.exe', '.zip' } | ForEach-Object {
  "$((Get-FileHash $_.FullName -Algorithm SHA256).Hash.ToLower())  $($_.Name)" |
    Out-File -Encoding ascii "$($_.FullName).sha256"
}
Get-ChildItem $OutDir | Format-Table Name, Length
