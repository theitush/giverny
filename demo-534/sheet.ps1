param([string]$Files, [string]$Labels, [string]$Out, [int]$Cols = 3, [int]$CW = 640, [int]$CX = 38, [int]$CY = 59, [int]$SW = 1280, [int]$SH = 820)
# sheet.ps1: raw frames (full paths, comma-separated) into one labelled grid, cropped to the window content.
Add-Type -AssemblyName System.Drawing
$list = $Files -split ","; $lab = $Labels -split ","
$ch = [int]($CW * $SH / $SW); $rows = [math]::Ceiling($list.Count / $Cols)
$bmp = New-Object Drawing.Bitmap ($Cols * $CW), ($rows * ($ch + 20))
$g = [Drawing.Graphics]::FromImage($bmp); $g.Clear([Drawing.Color]::Black)
$g.InterpolationMode = [Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
$f = New-Object Drawing.Font("Segoe UI", 12)
for ($i = 0; $i -lt $list.Count; $i++) {
  $x = ($i % $Cols) * $CW; $y = [math]::Floor($i / $Cols) * ($ch + 20)
  $img = [Drawing.Image]::FromFile($list[$i])
  $g.DrawImage($img, (New-Object Drawing.Rectangle($x, ($y + 20), $CW, $ch)), (New-Object Drawing.Rectangle($CX, $CY, $SW, $SH)), [Drawing.GraphicsUnit]::Pixel); $img.Dispose()
  $g.DrawString($lab[$i], $f, [Drawing.Brushes]::Yellow, $x, $y)
}
$bmp.Save($Out, [Drawing.Imaging.ImageFormat]::Jpeg)
