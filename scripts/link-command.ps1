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

function Plain-File($Item) {
    return $Item -and -not $Item.PSIsContainer -and -not ($Item.Attributes -band [IO.FileAttributes]::ReparsePoint)
}

function Command-State([string]$Path, [string]$Marker) {
    $item = Get-Item -LiteralPath $Path -Force -ErrorAction SilentlyContinue
    if (-not $item) { return 'Missing' }
    $markerItem = Get-Item -LiteralPath $Marker -Force -ErrorAction SilentlyContinue
    if ((Plain-File $item) -and (Plain-File $markerItem)) {
        try {
            $record = Get-Content -LiteralPath $Marker -Raw | ConvertFrom-Json
            $currentHash = File-Sha256 $Path
            if ($record.binary -is [string] -and $record.sha256 -is [string] -and $record.sha256 -ceq $currentHash) { return 'Owned' }
        } catch {}
    }
    return 'Foreign'
}

function Marker-Snapshot([string]$Path) {
    $item = Get-Item -LiteralPath $Path -Force -ErrorAction SilentlyContinue
    if (-not $item) { return $null }
    if (-not (Plain-File $item)) { throw "foreign command marker: $Path" }
    return [Convert]::ToBase64String([IO.File]::ReadAllBytes($Path))
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

# FileStream.Lock fails immediately on contention. A synchronous LockFileEx
# instead blocks on exactly the range used by Rust's std::fs::File::lock.
if (-not ('HerdrProjects.CommandLink.NativeWinFileLock' -as [type])) {
    Add-Type -TypeDefinition @'
using System;
using System.ComponentModel;
using System.IO;
using System.Runtime.InteropServices;
using System.Text;
using Microsoft.Win32.SafeHandles;

namespace HerdrProjects.CommandLink {
    public sealed class NativeWinFileLock : IDisposable {
        [StructLayout(LayoutKind.Sequential)]
        private struct Overlapped {
            public UIntPtr Internal, InternalHigh;
            public uint Offset, OffsetHigh;
            public IntPtr Event;
        }
        [StructLayout(LayoutKind.Sequential)]
        private struct FileInformation {
            public uint Attributes;
            public System.Runtime.InteropServices.ComTypes.FILETIME Creation, Access, Write;
            public uint Volume, SizeHigh, SizeLow, Links, IndexHigh, IndexLow;
        }
        [DllImport("kernel32.dll", CharSet = CharSet.Unicode, ExactSpelling = true, SetLastError = true)]
        private static extern SafeFileHandle CreateFileW(string path, uint access, uint share,
            IntPtr security, uint disposition, uint flags, IntPtr template);
        [DllImport("kernel32.dll", SetLastError = true)]
        private static extern bool GetFileInformationByHandle(SafeFileHandle file, out FileInformation info);
        [DllImport("kernel32.dll", SetLastError = true)]
        private static extern bool LockFileEx(SafeFileHandle file, uint flags, uint reserved,
            uint lengthLow, uint lengthHigh, ref Overlapped overlapped);
        private readonly FileStream stream;

        private static SafeFileHandle OpenPlain(string path, uint access, uint share, out FileInformation info) {
            // OPEN_EXISTING + OPEN_REPARSE_POINT: inspect the actual leaf, never its target.
            SafeFileHandle handle = CreateFileW(path, access, share, IntPtr.Zero, 3, 0x00200000, IntPtr.Zero);
            try {
                if (handle.IsInvalid || !GetFileInformationByHandle(handle, out info))
                    throw new Win32Exception(Marshal.GetLastWin32Error());
                if ((info.Attributes & (0x10 | 0x400)) != 0)
                    throw new IOException("not a regular non-reparse file: " + path);
                return handle;
            } catch { handle.Dispose(); throw; }
        }

        public NativeWinFileLock(string path) {
            FileInformation info;
            // Read/write sharing only: nobody may rename/delete a token while any waiter has it open.
            SafeFileHandle handle = OpenPlain(path, 0xC0000000, 3, out info);
            try {
                stream = new FileStream(handle, FileAccess.ReadWrite);
                Overlapped overlapped = new Overlapped();
                if (!LockFileEx(handle, 2, 0, uint.MaxValue, uint.MaxValue, ref overlapped))
                    throw new Win32Exception(Marshal.GetLastWin32Error());
                const string signature = "herdr-projects command transaction lock\n";
                if (stream.Length != Encoding.UTF8.GetByteCount(signature))
                    throw new IOException("foreign command lock: " + path);
                using (StreamReader reader = new StreamReader(stream, Encoding.UTF8, false, 1024, true)) {
                    if (!String.Equals(reader.ReadToEnd(), signature, StringComparison.Ordinal))
                        throw new IOException("foreign command lock: " + path);
                }
            } catch {
                if (stream != null) stream.Dispose(); else handle.Dispose();
                throw;
            }
        }

        public static string Identity(string path) {
            FileInformation info;
            using (SafeFileHandle handle = OpenPlain(path, 0x80000000, 7, out info)) {
                return String.Format("{0:X8}{1:X8}{2:X8}", info.Volume, info.IndexHigh, info.IndexLow);
            }
        }

        public void Dispose() { stream.Dispose(); }
    }
}
'@
}

[IO.Directory]::CreateDirectory($bin) | Out-Null
$lockPath = Join-Path $bin '.herdr-projects-command.lock'
if (-not (Get-Item -LiteralPath $lockPath -Force -ErrorAction SilentlyContinue)) {
    # Publish a fully signed token create-only, so first-time concurrent openers
    # cannot mistake an incompletely initialized token for a foreign file.
    $lockTemporary = Join-Path $bin ('.herdr-projects-command.' + [guid]::NewGuid().ToString('N') + '.lock.tmp')
    $lockStream = $null
    $lockStaged = $false
    try {
        $lockStream = [IO.File]::Open($lockTemporary, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
        $lockStaged = $true
        $signature = [Text.Encoding]::UTF8.GetBytes("herdr-projects command transaction lock`n")
        $lockStream.Write($signature, 0, $signature.Length)
        $lockStream.Flush($true)
        $lockStream.Dispose()
        $lockStream = $null
        try { [IO.File]::Move($lockTemporary, $lockPath) }
        catch {
            if (-not (Get-Item -LiteralPath $lockPath -Force -ErrorAction SilentlyContinue)) { throw }
        }
    } finally {
        if ($lockStream) { $lockStream.Dispose() }
        if ($lockStaged -and [IO.File]::Exists($lockTemporary)) { [IO.File]::Delete($lockTemporary) }
    }
}
# This persistent token must never be removed: all writers/waiters share its identity.
$transaction = New-Object HerdrProjects.CommandLink.NativeWinFileLock($lockPath)
try {
if ((Command-State $link $marker) -eq 'Foreign') {
    Say "left $link alone: it is not an unchanged managed command copy"
    return
}

$id = [guid]::NewGuid().ToString('N')
$temporary = Join-Path $bin ".herdr-projects.command-$id.exe"
$temporaryMarker = Join-Path $bin ".herdr-projects.command-$id.json"
$previous = $null
$published = $false
$commandStaged = $false
$markerStaged = $false
try {
    $stage = [IO.File]::Open($temporary, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
    $stage.Dispose()
    $commandStaged = $true
    [IO.File]::Copy($source, $temporary, $true)
    $newHash = File-Sha256 $temporary
    $record = @{ binary = [IO.Path]::GetFullPath($target); sha256 = $newHash } | ConvertTo-Json -Compress
    $stage = [IO.File]::Open($temporaryMarker, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
    $stage.Dispose()
    $markerStaged = $true
    [IO.File]::WriteAllText($temporaryMarker, $record, (New-Object Text.UTF8Encoding($false)))
    $publishedIdentity = [HerdrProjects.CommandLink.NativeWinFileLock]::Identity($temporary)
    # Recheck after staging, before replacing any destination.
    $current = Command-State $link $marker
    if ($current -eq 'Foreign') {
        Say "left $link alone: it is not an unchanged managed command copy"
        return
    }
    $previousMarker = Marker-Snapshot $marker
    if ($current -eq 'Owned') {
        $previous = Join-Path $bin ".herdr-projects.previous-$id.exe"
        [IO.File]::Move($link, $previous)
    }
    try {
        [IO.File]::Move($temporary, $link)
        $published = $true
        if ([IO.File]::Exists($marker)) { [IO.File]::Replace($temporaryMarker, $marker, [NullString]::Value) }
        else { [IO.File]::Move($temporaryMarker, $marker) }
    } catch {
        $failure = $_.Exception.Message
        try {
            if ($published) {
                if ([HerdrProjects.CommandLink.NativeWinFileLock]::Identity($link) -cne $publishedIdentity -or
                    (File-Sha256 $link) -cne $newHash -or
                    (Marker-Snapshot $marker) -cne $previousMarker) {
                    throw 'command or marker changed; the current publication was preserved'
                }
                [IO.File]::Delete($link)
            }
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
    if ($commandStaged -and [IO.File]::Exists($temporary)) { [IO.File]::Delete($temporary) }
    if ($markerStaged -and [IO.File]::Exists($temporaryMarker)) { [IO.File]::Delete($temporaryMarker) }
}
} finally {
    $transaction.Dispose()
}
