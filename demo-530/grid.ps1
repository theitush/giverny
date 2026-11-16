param([string]$Dir, [string]$Frames, [string]$Out, [int]$Cols = 4, [int]$CW = 480)
Add-Type -AssemblyName System.Drawing
$list = $Frames -split ","
$ch = [int]($CW * 884 / 1280); $rows = [math]::Ceiling($list.Count / $Cols)
$bmp = New-Object Drawing.Bitmap ($Cols * $CW), ($rows * ($ch + 18))
$g = [Drawing.Graphics]::FromImage($bmp); $g.Clear([Drawing.Color]::Black)
$g.InterpolationMode = [Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
$f = New-Object Drawing.Font("Segoe UI", 12)
for ($i = 0; $i -lt $list.Count; $i++) {
  $x = ($i % $Cols) * $CW; $y = [math]::Floor($i / $Cols) * ($ch + 18)
  $img = [Drawing.Image]::FromFile(("{0}\{1:D6}.jpg" -f $Dir, [int]$list[$i]))
  $g.DrawImage($img, $x, ($y + 18), $CW, $ch); $img.Dispose()
  $g.DrawString($list[$i], $f, [Drawing.Brushes]::Yellow, $x, $y)
}
$bmp.Save($Out, [Drawing.Imaging.ImageFormat]::Jpeg)
