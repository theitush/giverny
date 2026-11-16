param(
  [string]$Frames, [string]$Timeline, [string]$Out, [int]$Fps = 15,
  [int]$CropX = 38, [int]$CropY = 59, [int]$W = 1280, [int]$H = 820, [int]$Band = 64,
  [string]$Title = "", [string]$Subtitle = "", [string]$EndTitle = "", [string]$EndSubtitle = "",
  [double]$CardSeconds = 3.5
)
# giverny#530: the recorded frames ($Frames\<epoch ms>.jpg) into a constant-rate sequence $Out\NNNNNN.jpg,
# cropped to the window's content, with a caption band under it, following $Timeline (TSV: kind, epoch ms, arg):
#   start / end        the take's first and last instant
#   cap  "<tags>|<text>"  the caption from here on (tags: "#36" or "#38 #40"; empty text: none)
#   speed <factor>     play faster from here on (1 = real time)
#   cut / uncut        leave out what lies between
Add-Type -AssemblyName System.Drawing
$ErrorActionPreference = "Stop"
New-Item -ItemType Directory -Force -Path $Out | Out-Null
Get-ChildItem $Out -Filter *.jpg | Remove-Item

$files = Get-ChildItem $Frames -Filter *.jpg | ForEach-Object { [pscustomobject]@{ T = [int64]$_.BaseName; P = $_.FullName } } | Sort-Object T
$ft = [int64[]]($files | ForEach-Object { $_.T })
$ev = Get-Content $Timeline -Encoding UTF8 | Where-Object { $_ -match "\S" } | ForEach-Object {
  $p = $_ -split "`t", 3
  [pscustomobject]@{ K = $p[0]; T = [int64]$p[1]; A = $(if ($p.Count -gt 2) { $p[2] } else { "" }) }
} | Sort-Object T
$t0 = ($ev | Where-Object K -eq "start" | Select-Object -First 1).T
$t1 = ($ev | Where-Object K -eq "end" | Select-Object -Last 1).T

$codec = [Drawing.Imaging.ImageCodecInfo]::GetImageEncoders() | Where-Object { $_.MimeType -eq "image/jpeg" }
$ep = New-Object Drawing.Imaging.EncoderParameters 1
$ep.Param[0] = New-Object Drawing.Imaging.EncoderParameter ([Drawing.Imaging.Encoder]::Quality), ([long]90)
$fontCap = New-Object Drawing.Font("Segoe UI", 15, [Drawing.FontStyle]::Regular, [Drawing.GraphicsUnit]::Pixel)
$fontTag = New-Object Drawing.Font("Segoe UI Semibold", 14, [Drawing.FontStyle]::Regular, [Drawing.GraphicsUnit]::Pixel)
$fontSpeed = New-Object Drawing.Font("Segoe UI Semibold", 13, [Drawing.FontStyle]::Regular, [Drawing.GraphicsUnit]::Pixel)
$fontT1 = New-Object Drawing.Font("Segoe UI Semibold", 40, [Drawing.FontStyle]::Regular, [Drawing.GraphicsUnit]::Pixel)
$fontT2 = New-Object Drawing.Font("Segoe UI", 20, [Drawing.FontStyle]::Regular, [Drawing.GraphicsUnit]::Pixel)
$bandBg = [Drawing.Color]::FromArgb(255, 14, 16, 20)
$capFg = New-Object Drawing.SolidBrush ([Drawing.Color]::FromArgb(255, 226, 230, 236))
$tagBg = New-Object Drawing.SolidBrush ([Drawing.Color]::FromArgb(255, 122, 162, 247))
$tagFg = New-Object Drawing.SolidBrush ([Drawing.Color]::FromArgb(255, 14, 16, 20))
$speedFg = New-Object Drawing.SolidBrush ([Drawing.Color]::FromArgb(255, 230, 180, 80))
$dim = New-Object Drawing.SolidBrush ([Drawing.Color]::FromArgb(255, 150, 158, 170))
$H2 = $H + $Band
$n = 0

function Save($bmp) {
  $script:n++
  $bmp.Save(("{0}\{1:D6}.jpg" -f $Out, $script:n), $codec, $ep)
}
function Card($t1, $t2) {
  $bmp = New-Object Drawing.Bitmap $W, $H2
  $g = [Drawing.Graphics]::FromImage($bmp)
  $g.TextRenderingHint = [Drawing.Text.TextRenderingHint]::AntiAliasGridFit
  $g.Clear($bandBg)
  $sf = New-Object Drawing.StringFormat; $sf.Alignment = "Center"; $sf.LineAlignment = "Center"
  $g.DrawString($t1, $fontT1, $capFg, (New-Object Drawing.RectangleF(0, 0, $W, ($H2 / 2 - 10))), $sf)
  $sf2 = New-Object Drawing.StringFormat; $sf2.Alignment = "Center"; $sf2.LineAlignment = "Near"
  $g.DrawString($t2, $fontT2, $dim, (New-Object Drawing.RectangleF(80, ($H2 / 2 + 10), ($W - 160), ($H2 / 2 - 20))), $sf2)
  $g.Dispose()
  for ($i = 0; $i -lt [int]($CardSeconds * $Fps); $i++) { Save $bmp }
  $bmp.Dispose()
}

if ($Title) { Card $Title $Subtitle }

$cap = ""; $speed = 1.0; $cutting = $false; $ei = 0
$s = [double]$t0; $fi = 0
$lastKey = ""; $lastFile = ""
$src = $null; $srcPath = ""
while ($s -le $t1) {
  while ($ei -lt $ev.Count -and $ev[$ei].T -le $s) {
    $e = $ev[$ei]; $ei++
    switch ($e.K) {
      "cap" { $cap = $e.A }
      "speed" { $speed = [double]$e.A }
      "cut" { $cutting = $true }
      "uncut" { $cutting = $false }
    }
  }
  if ($cutting) {
    $next = $ev | Select-Object -Skip $ei | Where-Object K -eq "uncut" | Select-Object -First 1
    if (-not $next) { break }
    $s = [double]$next.T; continue
  }
  while ($fi + 1 -lt $ft.Count -and $ft[$fi + 1] -le $s) { $fi++ }
  $path = $files[$fi].P
  $key = "$path|$cap|$speed"
  if ($key -eq $lastKey) {
    $script:n++
    [IO.File]::Copy($lastFile, ("{0}\{1:D6}.jpg" -f $Out, $script:n))
  } else {
    if ($path -ne $srcPath) { if ($src) { $src.Dispose() }; $src = [Drawing.Image]::FromFile($path); $srcPath = $path }
    $bmp = New-Object Drawing.Bitmap $W, $H2
    $g = [Drawing.Graphics]::FromImage($bmp)
    $g.TextRenderingHint = [Drawing.Text.TextRenderingHint]::AntiAliasGridFit
    $g.SmoothingMode = [Drawing.Drawing2D.SmoothingMode]::AntiAlias
    $g.Clear($bandBg)
    $g.DrawImage($src, (New-Object Drawing.Rectangle(0, 0, $W, $H)), (New-Object Drawing.Rectangle($CropX, $CropY, $W, $H)), [Drawing.GraphicsUnit]::Pixel)
    $x = 16.0
    $parts = $cap -split "\|", 2
    $tags = $(if ($parts.Count -gt 1) { $parts[0].Trim() } else { "" })
    $text = $(if ($parts.Count -gt 1) { $parts[1].Trim() } else { $parts[0].Trim() })
    foreach ($tag in ($tags -split " " | Where-Object { $_ })) {
      $sz = $g.MeasureString($tag, $fontTag)
      $r = New-Object Drawing.RectangleF($x, ($H + ($Band - 24) / 2), ($sz.Width + 10), 24)
      $g.FillRectangle($tagBg, $r)
      $g.DrawString($tag, $fontTag, $tagFg, ($x + 5), ($H + ($Band - 24) / 2 + 3))
      $x += $sz.Width + 18
    }
    $right = $W - 16
    if ($speed -gt 1.01) {
      $st = ("{0}x speed" -f $speed)
      $sz = $g.MeasureString($st, $fontSpeed)
      $g.DrawString($st, $fontSpeed, $speedFg, ($W - 16 - $sz.Width), ($H + ($Band - $sz.Height) / 2))
      $right = $W - 32 - $sz.Width
    }
    if ($text) {
      $sf = New-Object Drawing.StringFormat; $sf.LineAlignment = "Center"
      $g.DrawString($text, $fontCap, $capFg, (New-Object Drawing.RectangleF($x, $H, ($right - $x), $Band)), $sf)
    }
    $g.Dispose()
    Save $bmp
    $bmp.Dispose()
    $lastKey = $key; $lastFile = ("{0}\{1:D6}.jpg" -f $Out, $script:n)
  }
  $s += (1000.0 / $Fps) * $speed
}
if ($src) { $src.Dispose() }
if ($EndTitle) { Card $EndTitle $EndSubtitle }
"frames $n"
