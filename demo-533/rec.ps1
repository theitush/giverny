param([string]$Out, [int]$Width = 0, [string]$Stop, [int]$IntervalMs = 100, [int]$MaxSeconds = 900, [string]$Match = "iverny")
# giverny#533: film one window (title *$Match*, outer width $Width px) with PrintWindow into
# $Out\<ms since start>.jpg, every $IntervalMs, until the file $Stop exists or $MaxSeconds pass.
Add-Type -AssemblyName System.Drawing
Add-Type @"
using System; using System.Text; using System.Runtime.InteropServices;
public class W {
  public delegate bool EnumProc(IntPtr h, IntPtr l);
  [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc f, IntPtr l);
  [DllImport("user32.dll")] public static extern int GetWindowText(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr dc, uint f);
  [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
}
"@
[W]::SetProcessDPIAware() | Out-Null
$script:found = New-Object System.Collections.ArrayList
[W]::EnumWindows({ param($h, $l)
  if ([W]::IsWindowVisible($h)) { $sb = New-Object Text.StringBuilder 256; [W]::GetWindowText($h, $sb, 256) | Out-Null
    if ($sb.ToString() -like "*$Match*") { [void]$script:found.Add($h) } }
  return $true }, [IntPtr]::Zero) | Out-Null
$h = [IntPtr]::Zero
foreach ($f in $script:found) {
  $q = New-Object W+RECT; [W]::GetWindowRect($f, [ref]$q) | Out-Null
  "window $f $($q.R-$q.L)x$($q.B-$q.T) at $($q.L),$($q.T)"
  if ($Width -eq 0 -or ($q.R - $q.L) -eq $Width) { $h = $f }
}
if ($h -eq [IntPtr]::Zero) { "no window"; exit 1 }
New-Item -ItemType Directory -Force -Path $Out | Out-Null
$codec = [Drawing.Imaging.ImageCodecInfo]::GetImageEncoders() | Where-Object { $_.MimeType -eq "image/jpeg" }
$ep = New-Object Drawing.Imaging.EncoderParameters 1
$ep.Param[0] = New-Object Drawing.Imaging.EncoderParameter ([Drawing.Imaging.Encoder]::Quality), ([long]92)
$sw = [Diagnostics.Stopwatch]::StartNew(); $n = 0; $epoch0 = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
while ($sw.Elapsed.TotalSeconds -lt $MaxSeconds -and -not ($Stop -and (Test-Path $Stop))) {
  $t0 = $sw.ElapsedMilliseconds
  $r = New-Object W+RECT; [W]::GetWindowRect($h, [ref]$r) | Out-Null
  $w = $r.R - $r.L; $ht = $r.B - $r.T
  if ($w -le 0) { break }
  $bmp = New-Object Drawing.Bitmap $w, $ht
  $g = [Drawing.Graphics]::FromImage($bmp); $dc = $g.GetHdc()
  [W]::PrintWindow($h, $dc, 2) | Out-Null
  $g.ReleaseHdc($dc); $g.Dispose()
  $bmp.Save(("{0}\{1}.jpg" -f $Out, ($epoch0 + $t0)), $codec, $ep); $bmp.Dispose(); $n++
  $left = $IntervalMs - ($sw.ElapsedMilliseconds - $t0)
  if ($left -gt 0) { Start-Sleep -Milliseconds $left }
}
"frames $n in $([int]$sw.Elapsed.TotalSeconds)s"
