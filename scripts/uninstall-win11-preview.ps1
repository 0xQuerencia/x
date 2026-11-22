$ErrorActionPreference = 'Stop'
if (-not $env:LOCALAPPDATA -or -not $env:APPDATA) {
    throw 'The current Windows user profile is unavailable.'
}
$folder = Join-Path $env:LOCALAPPDATA 'XPreview'
$marker = Join-Path $folder '.x-preview-owned'
$target = Join-Path $folder 'x.exe'
$shortcut = Join-Path $env:APPDATA 'Microsoft\Windows\Start Menu\Programs\X Preview.lnk'

if (-not (Test-Path -LiteralPath $folder -PathType Container)) {
    Write-Output 'X Preview is not installed.'
    exit 0
}
$existingFolder = Get-Item -LiteralPath $folder -Force
if ($existingFolder.Attributes -band [IO.FileAttributes]::ReparsePoint) {
    throw 'Refusing to uninstall a linked X preview folder.'
}
if (-not (Test-Path -LiteralPath $marker -PathType Leaf) -or
    (Get-Content -LiteralPath $marker -Raw).Trim() -ne 'x-preview-v1') {
    throw 'Missing or unexpected X preview ownership marker; nothing was removed.'
}
foreach ($name in @('x.exe', 'x.previous.exe', '.x-preview-owned')) {
    $path = Join-Path $folder $name
    if (Test-Path -LiteralPath $path) {
        $entry = Get-Item -LiteralPath $path -Force
        if ($entry.PSIsContainer -or ($entry.Attributes -band [IO.FileAttributes]::ReparsePoint)) {
            throw 'Unexpected X preview content; nothing was removed.'
        }
    }
}
if (Test-Path -LiteralPath $shortcut) {
    $wsh = New-Object -ComObject WScript.Shell
    $link = $wsh.CreateShortcut($shortcut)
    if ([string]::Equals($link.TargetPath, $target, [StringComparison]::OrdinalIgnoreCase)) {
        Remove-Item -LiteralPath $shortcut -Force
    }
}
foreach ($name in @('x.exe', 'x.previous.exe', '.x-preview-owned')) {
    $path = Join-Path $folder $name
    if (Test-Path -LiteralPath $path) {
        Remove-Item -LiteralPath $path -Force
    }
}
if (@(Get-ChildItem -LiteralPath $folder -Force).Count -eq 0) {
    Remove-Item -LiteralPath $folder
}
Write-Output 'Removed the managed X preview; official Hex and personal X data were not changed.'
