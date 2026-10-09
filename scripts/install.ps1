# Giverny installer for Windows.
#
#   irm https://github.com/y0av/giverny/releases/latest/download/install.ps1 | iex
#
# Downloads the release binary, installs it to %LOCALAPPDATA%\Giverny\bin
# (override with $env:GIVERNY_BIN_DIR), and puts that directory on your PATH.

$ErrorActionPreference = 'Stop'

$repo    = 'y0av/giverny'
$binDir  = if ($env:GIVERNY_BIN_DIR) { $env:GIVERNY_BIN_DIR } else { "$env:LOCALAPPDATA\Giverny\bin" }
$version = if ($env:GIVERNY_VERSION) { $env:GIVERNY_VERSION } else { 'latest' }

# Colour and drawing only for someone watching; a redirected run (CI, a log)
# gets the same plain lines as ever. Every glyph is built from its code point:
# this file reaches PowerShell through `irm | iex`, which can mangle anything
# outside ASCII.
$fancy = (-not $env:NO_COLOR) -and (-not [Console]::IsOutputRedirected)
$vt    = $fancy -and $Host.UI.SupportsVirtualTerminal
$esc   = [char]27
$block = [char]0x2588
$tick  = [char]0x2713
$down  = [char]0x2193
$arrow = [char]0x2192

# The splash's wordmark (crates/app/src/splash.rs), washing from wisteria to
# cream down the letters, with '#' standing in for a block. A test there keeps
# the two drawings identical.
$wordmark = @(
    @('135;116;164', ' ###  ##### #   # ##### ####  #   # #   #'),
    @('154;134;184', '#   #   #   #   # #     #   # ##  # #   #'),
    @('168;151;194', '#       #   #   # #     #   # ##  #  # #'),
    @('182;166;206', '# ###   #   #   # ####  ####  # # #   #'),
    @('195;182;216', '#   #   #   #   # #     # #   #  ##   #'),
    @('213;203;226', '#   #   #    # #  #     #  #  #  ##   #'),
    @('231;224;238', ' ###  #####   #   ##### #   # #   #   #')
)

function Write-Wordmark {
    Write-Host ''
    foreach ($row in $wordmark) {
        $art = $row[1].Replace('#', $block)
        if ($vt) {
            Write-Host "  ${esc}[38;2;$($row[0])m$art${esc}[0m"
        } else {
            Write-Host "  $art" -ForegroundColor Magenta
        }
    }
    Write-Host ''
}

# A finished step. Plain output says $plain instead, or nothing without one.
function Write-Ok($text, $plain) {
    if ($fancy) {
        Write-Host "  $tick " -ForegroundColor Green -NoNewline
        Write-Host $text
    } elseif ($plain) {
        Write-Host $plain
    }
}

# What a binary says it is, or nothing. A build too old to know --version
# opens its window instead, so it gets five seconds and is then stopped.
function Get-GivernyVersion($exe) {
    if (-not (Test-Path $exe)) { return '' }
    $out = Join-Path $tmp 'version.out'
    try {
        $p = Start-Process -FilePath $exe -ArgumentList '--version' -NoNewWindow -PassThru `
            -RedirectStandardOutput $out -RedirectStandardError (Join-Path $tmp 'version.err')
        if (-not $p.WaitForExit(5000)) { $p.Kill(); return '' }
        $line = Get-Content $out | Select-Object -First 1
        if ($line -match '^giverny (\S+)') { return $Matches[1] }
    } catch { }
    return ''
}

if ([System.Environment]::Is64BitOperatingSystem -eq $false) {
    throw 'Giverny requires 64-bit Windows.'
}
$target = 'x86_64-pc-windows-msvc'
$asset  = "giverny-$target.zip"
$url    = if ($version -eq 'latest') {
    "https://github.com/$repo/releases/latest/download/$asset"
} else {
    "https://github.com/$repo/releases/download/$version/$asset"
}

$tmp = Join-Path ([System.IO.Path]::GetTempPath()) ([System.Guid]::NewGuid().ToString())
New-Item -ItemType Directory -Path $tmp | Out-Null
$encoding = [Console]::OutputEncoding
try {
    if ($fancy) {
        # Put back in `finally`: under `iex` this is the user's own session.
        try { [Console]::OutputEncoding = [System.Text.Encoding]::UTF8 } catch { }
        Write-Wordmark
        Write-Ok $target
        Write-Host "  $down " -ForegroundColor Magenta -NoNewline
        Write-Host $asset
    } else {
        Write-Host "downloading $asset"
    }
    Invoke-WebRequest -Uri $url -OutFile (Join-Path $tmp $asset) -UseBasicParsing
    Expand-Archive -Path (Join-Path $tmp $asset) -DestinationPath $tmp -Force

    $exe = Join-Path $tmp 'giverny.exe'
    if (-not (Test-Path $exe)) { throw 'archive did not contain giverny.exe' }

    New-Item -ItemType Directory -Path $binDir -Force | Out-Null
    $dest = Join-Path $binDir 'giverny.exe'
    $from = Get-GivernyVersion $dest
    $to   = Get-GivernyVersion $exe
    # A running exe cannot be overwritten on Windows; move it aside first.
    if (Test-Path $dest) {
        $old = "$dest.old"
        Remove-Item $old -ErrorAction SilentlyContinue
        try { Move-Item $dest $old -Force } catch { }
    }
    Move-Item $exe $dest -Force
    Write-Ok "installed $dest" "installed $dest"

    $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    if ($userPath -notlike "*$binDir*") {
        [Environment]::SetEnvironmentVariable('Path', "$userPath;$binDir", 'User')
        $note = "added $binDir to your PATH (restart your terminal to pick it up)"
        Write-Ok $note $note
    }

    Write-Host ''
    if (-not $fancy) {
        Write-Host "run: giverny        (and 'giverny doctor' if Claude states look wrong)"
    } else {
        if ($to -and $from -and $from -ne $to) {
            Write-Host "  $from $arrow " -ForegroundColor DarkGray -NoNewline
            Write-Host $to -ForegroundColor Yellow
        } elseif ($to -and $from -eq $to) {
            Write-Host '  reinstalled ' -ForegroundColor DarkGray -NoNewline
            Write-Host $to -ForegroundColor Yellow
        } elseif ($to) {
            Write-Host '  installed ' -ForegroundColor DarkGray -NoNewline
            Write-Host $to -ForegroundColor Yellow
        }
        Write-Host '  run: ' -ForegroundColor DarkGray -NoNewline
        Write-Host 'giverny ' -NoNewline
        Write-Host '(if it is open, restart it from the rail to finish)' -ForegroundColor DarkGray
        Write-Host ''
    }
} finally {
    try { [Console]::OutputEncoding = $encoding } catch { }
    Remove-Item $tmp -Recurse -Force -ErrorAction SilentlyContinue
}
