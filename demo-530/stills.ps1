param([string]$Dir, [string]$Spec, [string]$Out, [int]$W = 1280, [int]$H = 820)
# stills.ps1: frames of the composed take, cut to the window (the caption band left off), as $Out\<name>.jpg.
Add-Type -AssemblyName System.Drawing
New-Item -ItemType Directory -Force -Path $Out | Out-Null
$codec = [Drawing.Imaging.ImageCodecInfo]::GetImageEncoders() | Where-Object { $_.MimeType -eq "image/jpeg" }
$ep = New-Object Drawing.Imaging.EncoderParameters 1
$ep.Param[0] = New-Object Drawing.Imaging.EncoderParameter ([Drawing.Imaging.Encoder]::Quality), ([long]86)
foreach ($pair in ($Spec -split ",")) {
  $n, $name = $pair -split "="
  $img = [Drawing.Image]::FromFile(("{0}\{1:D6}.jpg" -f $Dir, [int]$n))
  $bmp = New-Object Drawing.Bitmap $W, $H
  $g = [Drawing.Graphics]::FromImage($bmp)
  $g.DrawImage($img, (New-Object Drawing.Rectangle(0, 0, $W, $H)), (New-Object Drawing.Rectangle(0, 0, $W, $H)), [Drawing.GraphicsUnit]::Pixel)
  $g.Dispose(); $img.Dispose()
  $bmp.Save(("{0}\{1}.jpg" -f $Out, $name), $codec, $ep); $bmp.Dispose()
}
