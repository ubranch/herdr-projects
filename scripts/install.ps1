# Installs target/release/herdr-projects.exe without overwriting a running image.
# HERDR_PROJECTS_BUILD=source forces the locked Cargo build; downloads follow
# HERDR_PROJECTS_DOWNLOAD_URL or this checkout's GitHub origin, as on Unix.
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

function Download-Release([string]$Version, [string]$Temporary) {
    $architecture = $env:PROCESSOR_ARCHITECTURE
    if ($env:PROCESSOR_ARCHITEW6432) { $architecture = $env:PROCESSOR_ARCHITEW6432 }
    if ($architecture -ne 'AMD64') {
        Say "there is no prebuilt Windows binary for $architecture"
        return $null
    }
    $asset = 'herdr-projects-x86_64-pc-windows-msvc.exe'
    $tag = "v$Version"
    $origin = ''
    if (Test-Path -LiteralPath '.git') {
        $status = & git status --porcelain --untracked-files=no 2>$null
        if ($LASTEXITCODE -ne 0) { throw 'could not inspect this checkout' }
        if ($status) { throw 'this checkout has uncommitted changes' }
        $head = & git rev-parse HEAD 2>$null
        if ($LASTEXITCODE -ne 0) { throw 'could not read the checkout commit' }
        $release = & git rev-parse -q --verify "refs/tags/$tag^{commit}" 2>$null
        if ($LASTEXITCODE -ne 0) {
            $oldPrompt = $env:GIT_TERMINAL_PROMPT
            try {
                $env:GIT_TERMINAL_PROMPT = '0'
                $tags = & git ls-remote origin "refs/tags/$tag" "refs/tags/$tag^{}" 2>$null
                if ($LASTEXITCODE -ne 0) { throw "could not find $tag on origin" }
                $release = $null
                foreach ($line in $tags) {
                    if ($line -match '^(\S+)\s+refs/tags/.+$') { $release = $Matches[1] }
                    if ($line -match '^(\S+)\s+refs/tags/.+\^\{\}$') { $release = $Matches[1]; break }
                }
            } finally { $env:GIT_TERMINAL_PROMPT = $oldPrompt }
        }
        if (-not $release) { throw "could not find the $tag tag here or on origin" }
        if ($head -ne $release) { throw "this checkout is not the $tag release commit" }
        $origin = & git remote get-url origin 2>$null
        if ($LASTEXITCODE -ne 0) { throw 'could not read origin' }
    }
    if ($env:HERDR_PROJECTS_DOWNLOAD_URL) {
        $base = "$($env:HERDR_PROJECTS_DOWNLOAD_URL.TrimEnd('/'))/$tag"
    } else {
        $repo = 'ubranch/herdr-projects'
        if ($origin -match 'github\.com[:/]([^/]+/[^/]+?)(?:\.git)?$') { $repo = $Matches[1] }
        $base = "https://github.com/$repo/releases/download/$tag"
    }
    Say "downloading $asset $tag"
    $sums = Join-Path $Temporary 'SHA256SUMS'
    Invoke-WebRequest -UseBasicParsing -Uri "$base/SHA256SUMS" -OutFile $sums -TimeoutSec 30
    $expected = @(Get-Content -LiteralPath $sums | ForEach-Object {
        if ($_ -match ('^([0-9a-fA-F]{64})\s+\*?' + [regex]::Escape($asset) + '$')) { $Matches[1].ToLowerInvariant() }
    })
    if ($expected.Count -ne 1) { throw "the $tag release has no unique checksum for $asset" }
    $candidate = Join-Path $Temporary $asset
    Invoke-WebRequest -UseBasicParsing -Uri "$base/$asset" -OutFile $candidate -TimeoutSec 30
    $actual = File-Sha256 $candidate
    if ($actual -ne $expected[0]) { throw "the downloaded $asset does not match its SHA256SUMS entry (got $actual, expected $($expected[0]))" }
    $reported = & $candidate --version 2>$null
    if ($LASTEXITCODE -ne 0 -or $reported -notmatch ('^herdr-projects ' + [regex]::Escape($Version) + '(\+.*)?$')) {
        throw "the downloaded binary did not run or is not $Version (it said: $reported)"
    }
    return $candidate
}

$temporary = $null
Push-Location (Split-Path -Parent $PSScriptRoot)
try {
    $manifest = Get-Content -LiteralPath 'herdr-plugin.toml' -Raw
    if ($manifest -notmatch '(?m)^version\s*=\s*"([^"]+)"') { throw 'herdr-plugin.toml has no version' }
    $version = $Matches[1]
    $releaseDirectory = Join-Path (Split-Path -Parent $PSScriptRoot) 'target/release'
    [IO.Directory]::CreateDirectory($releaseDirectory) | Out-Null
    $temporary = Join-Path $releaseDirectory ('.install-' + [guid]::NewGuid().ToString('N'))
    [IO.Directory]::CreateDirectory($temporary) | Out-Null
    $candidate = $null
    if ($env:HERDR_PROJECTS_BUILD -eq 'source') {
        Say 'HERDR_PROJECTS_BUILD=source is set'
    } else {
        try { $candidate = Download-Release $version $temporary }
        catch { Say $_.Exception.Message }
    }
    if (-not $candidate) {
        if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
            throw 'cargo is not installed. Install Rust 1.89 or newer (https://rustup.rs) and Visual Studio C++ Build Tools, then install again.'
        }
        Say 'building from source instead: cargo build --release --locked (this takes a minute or two)'
        # A separate target directory leaves the installed .exe intact if Cargo fails.
        & cargo build --release --locked --target-dir $temporary
        if ($LASTEXITCODE -ne 0) { throw "cargo build failed (exit $LASTEXITCODE); the installed binary was not changed" }
        $candidate = Join-Path $temporary 'release/herdr-projects.exe'
        $reported = & $candidate --version
        if ($LASTEXITCODE -ne 0 -or $reported -notmatch ('^herdr-projects ' + [regex]::Escape($version) + '(\+.*)?$')) {
            throw "the built binary did not run or is not $version (it said: $reported); the installed binary was not changed"
        }
    }
    $binary = Join-Path $releaseDirectory 'herdr-projects.exe'
    $previous = $null
    if (Test-Path -LiteralPath $binary) {
        $previous = Join-Path $releaseDirectory ('.herdr-projects.previous-' + [guid]::NewGuid().ToString('N') + '.exe')
        try { [IO.File]::Move($binary, $previous) }
        catch { throw "could not rename the installed binary: $($_.Exception.Message). Close plugin processes and try again; the installed binary was not changed" }
    }
    try { [IO.File]::Move($candidate, $binary) }
    catch {
        $failure = $_.Exception.Message
        if ($previous) {
            try { [IO.File]::Move($previous, $binary) }
            catch { throw "install failed: $failure; rollback failed: $($_.Exception.Message). The previous binary remains at $previous" }
        }
        $state = if ($previous) { 'the previous installation was restored' } else { 'no binary was installed' }
        throw "could not install the new binary: $failure; $state"
    }
    if ($previous) {
        try { [IO.File]::Delete($previous) }
        catch { Say "the previous binary is still in use at $previous; remove it after its processes exit" }
    }
    Say "installed herdr-projects $version at $binary"
    # Command-link failure is reported, but must not turn a successful binary
    # replacement into an update failure claiming the old version is installed.
    try { & (Join-Path $PSScriptRoot 'link-command.ps1') }
    catch { Say "could not link the command: $($_.Exception.Message); run herdr-projects doctor --fix" }
} catch {
    Say $_.Exception.Message
    exit 1
} finally {
    if ($temporary -and (Test-Path -LiteralPath $temporary)) {
        try { Remove-Item -LiteralPath $temporary -Recurse -Force }
        catch { Say "could not remove the temporary build folder $temporary`: $($_.Exception.Message)" }
    }
    Pop-Location
}
