# Mirrors link-command.sh for Herdr's temporary managed checkout. Windows uses
# a verified command copy and marker instead of requiring symlink privileges.
param([string]$Checkout = (Get-Location).Path)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

function Say([string]$Message) {
    [Console]::Error.WriteLine("herdr-projects install: $Message")
}

function File-Sha256([string]$Path) {
    $sha = [Security.Cryptography.SHA256]::Create()
    $stream = $null
    try {
        $stream = [IO.File]::OpenRead($Path)
        return ([BitConverter]::ToString($sha.ComputeHash($stream))).Replace('-', '').ToLowerInvariant()
    } finally {
        if ($stream) { $stream.Dispose() }
        $sha.Dispose()
    }
}

$checkoutPath = [IO.Path]::GetFullPath($Checkout).TrimEnd('\', '/')
if ($checkoutPath -notmatch '^(.*[\\/]plugins)[\\/]\.tmp-install-[^\\/]+[\\/]checkout$') { return }
$plugins = $Matches[1]
$sha = [Security.Cryptography.SHA256]::Create()
try { $hash = ([BitConverter]::ToString($sha.ComputeHash([Text.Encoding]::UTF8.GetBytes('herdr-projects')))).Replace('-', '').ToLowerInvariant().Substring(0, 12) }
finally { $sha.Dispose() }
$target = Join-Path $plugins "github/herdr-projects-$hash/target/release/herdr-projects.exe"
$source = Join-Path $checkoutPath 'target/release/herdr-projects.exe'
$homeDirectory = $env:HOME
if (-not $homeDirectory) { $homeDirectory = $env:USERPROFILE }
if ($env:XDG_BIN_HOME) { $bin = $env:XDG_BIN_HOME }
elseif ($homeDirectory) { $bin = Join-Path $homeDirectory '.local/bin' }
else { Say 'neither HOME nor USERPROFILE is set; the command was not linked'; return }
$bin = [IO.Path]::GetFullPath($bin)
$link = Join-Path $bin 'herdr-projects.exe'
$marker = Join-Path $bin '.herdr-projects-command.json'
$item = Get-Item -LiteralPath $link -Force -ErrorAction SilentlyContinue
if ($item) {
    $owned = $false
    if (-not $item.PSIsContainer -and -not ($item.Attributes -band [IO.FileAttributes]::ReparsePoint)) {
        try {
            $record = Get-Content -LiteralPath $marker -Raw | ConvertFrom-Json
            $currentHash = File-Sha256 $link
            $owned = $record.binary -is [string] -and $record.sha256 -is [string] -and $record.sha256 -ceq $currentHash
        } catch { $owned = $false }
    }
    if (-not $owned) { Say "left $link alone: it is not an unchanged managed command copy"; return }
}

[IO.Directory]::CreateDirectory($bin) | Out-Null
$id = [guid]::NewGuid().ToString('N')
$temporary = Join-Path $bin ".herdr-projects.command-$id.exe"
$temporaryMarker = Join-Path $bin ".herdr-projects.command-$id.json"
$previous = $null
$published = $false
try {
    [IO.File]::Copy($source, $temporary)
    $newHash = File-Sha256 $temporary
    $record = @{ binary = [IO.Path]::GetFullPath($target); sha256 = $newHash } | ConvertTo-Json -Compress
    [IO.File]::WriteAllText($temporaryMarker, $record, (New-Object Text.UTF8Encoding($false)))
    if ($item) {
        $previous = Join-Path $bin ".herdr-projects.previous-$id.exe"
        [IO.File]::Move($link, $previous)
    }
    try {
        [IO.File]::Move($temporary, $link)
        $published = $true
        if ([IO.File]::Exists($marker)) { [IO.File]::Replace($temporaryMarker, $marker, $null) }
        else { [IO.File]::Move($temporaryMarker, $marker) }
    } catch {
        $failure = $_.Exception.Message
        try {
            if ($published) { [IO.File]::Delete($link) }
            if ($previous) { [IO.File]::Move($previous, $link); $previous = $null }
        } catch { throw "command link failed: $failure; rollback failed: $($_.Exception.Message); previous command: $previous" }
        throw "could not link $link`: $failure; the previous command was restored"
    }
    if ($previous) {
        try { [IO.File]::Delete($previous) }
        catch { Say "the previous command is still in use at $previous; remove it after its processes exit" }
    }
    if (@($env:PATH -split ';' | Where-Object { $_.TrimEnd('\', '/') -ieq $bin.TrimEnd('\', '/') }).Count) {
        Say "linked $link"
    } else {
        Say "linked $link, but $bin is not on your PATH: add it in your shell profile"
    }
} finally {
    foreach ($path in @($temporary, $temporaryMarker)) {
        if ([IO.File]::Exists($path)) { [IO.File]::Delete($path) }
    }
}
