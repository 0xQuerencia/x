param(
    [Parameter(Mandatory = $false)]
    [string] $Executable = (Join-Path $PSScriptRoot 'x.exe')
)
$ErrorActionPreference = 'Stop'
if (-not $env:LOCALAPPDATA -or -not $env:APPDATA) {
    throw 'The current Windows user profile is unavailable.'
}
if (-not (Test-Path -LiteralPath $Executable -PathType Leaf)) {
    throw 'Supply the x.exe from this same Windows preview ZIP.'
}
$source = Get-Item -LiteralPath $Executable -Force
if ($source.Attributes -band [IO.FileAttributes]::ReparsePoint) {
    throw 'A linked executable cannot be installed.'
}

$folder = Join-Path $env:LOCALAPPDATA 'XPreview'
$target = Join-Path $folder 'x.exe'
$backup = Join-Path $folder 'x.previous.exe'
$marker = Join-Path $folder '.x-preview-owned'
$shortcut = Join-Path $env:APPDATA 'Microsoft\Windows\Start Menu\Programs\X Preview.lnk'

if (Test-Path -LiteralPath $folder) {
    $existingFolder = Get-Item -LiteralPath $folder -Force
    if (-not $existingFolder.PSIsContainer -or ($existingFolder.Attributes -band [IO.FileAttributes]::ReparsePoint)) {
        throw 'Refusing to use a linked or unexpected X preview folder.'
    }
    if (-not (Test-Path -LiteralPath $marker -PathType Leaf)) {
        $contents = @(Get-ChildItem -LiteralPath $folder -Force)
        if ($contents.Count -gt 0) {
            throw 'Refusing to overwrite an unowned XPreview directory.'
        }
    } elseif ((Get-Content -LiteralPath $marker -Raw).Trim() -ne 'x-preview-v1') {
        throw 'Unrecognized X preview ownership marker.'
    }
}
foreach ($path in @($target, $backup, $marker)) {
    if (Test-Path -LiteralPath $path) {
        $item = Get-Item -LiteralPath $path -Force
        if ($item.PSIsContainer -or ($item.Attributes -band [IO.FileAttributes]::ReparsePoint)) {
            throw 'Refusing to replace an unexpected X preview file.'
        }
    }
}
$wsh = New-Object -ComObject WScript.Shell
if (Test-Path -LiteralPath $shortcut) {
    $existing = $wsh.CreateShortcut($shortcut)
    if (-not [string]::Equals($existing.TargetPath, $target, [StringComparison]::OrdinalIgnoreCase)) {
        throw 'An unrelated X Preview Start Menu shortcut already exists.'
    }
}

New-Item -ItemType Directory -Force $folder | Out-Null
$stage = Join-Path $folder ('x-stage-' + [guid]::NewGuid().ToString('N') + '.exe')
try {
    Copy-Item -LiteralPath $source.FullName -Destination $stage
    if ((Get-Item -LiteralPath $stage).Length -le 0) {
        throw 'The preview executable was empty.'
    }
    if (Test-Path -LiteralPath $target) {
        [IO.File]::Replace($stage, $target, $backup)
    } else {
        [IO.File]::Move($stage, $target)
    }
    Set-Content -LiteralPath $marker -Value 'x-preview-v1' -Encoding ascii
    $shortcutFolder = Split-Path -Parent $shortcut
    New-Item -ItemType Directory -Force $shortcutFolder | Out-Null
    $link = $wsh.CreateShortcut($shortcut)
    $link.TargetPath = $target
    $link.WorkingDirectory = $folder
    $link.Description = 'X experimental Windows preview'
    $link.Save()
    Write-Output "Installed unsigned X preview at $target (official Hex was not modified)."
} finally {
    Remove-Item -LiteralPath $stage -Force -ErrorAction SilentlyContinue
}
