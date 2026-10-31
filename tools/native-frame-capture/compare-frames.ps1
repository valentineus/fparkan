[CmdletBinding(DefaultParameterSetName = 'Compare')]
param(
    [Parameter(Mandatory = $true, ParameterSetName = 'Compare')][string]$NativePng,
    [Parameter(Mandatory = $true, ParameterSetName = 'Compare')][string]$ViewerPng,
    [Parameter(Mandatory = $true, ParameterSetName = 'Compare')][string]$CameraJson,
    [Parameter(Mandatory = $true, ParameterSetName = 'Compare')][string]$RegionsJson,
    [Parameter(Mandatory = $true, ParameterSetName = 'Compare')][string]$OutputPrefix,
    [Parameter(Mandatory = $true, ParameterSetName = 'SelfTest')][switch]$SelfTest
)

$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing

function Get-MeanRgb {
    param([System.Drawing.Bitmap]$Bitmap, [System.Drawing.Rectangle]$Region)
    if ($Region.X -lt 0 -or $Region.Y -lt 0 -or $Region.Width -le 0 -or $Region.Height -le 0 -or
        $Region.Right -gt $Bitmap.Width -or $Region.Bottom -gt $Bitmap.Height) {
        throw 'Region must be a nonempty rectangle within the image.'
    }
    $sum = [long[]]@(0, 0, 0)
    for ($y = $Region.Top; $y -lt $Region.Bottom; $y++) {
        for ($x = $Region.Left; $x -lt $Region.Right; $x++) {
            $pixel = $Bitmap.GetPixel($x, $y)
            $sum[0] += $pixel.R
            $sum[1] += $pixel.G
            $sum[2] += $pixel.B
        }
    }
    $count = [long]$Region.Width * $Region.Height
    return ,@($sum | ForEach-Object { [Math]::Round($_ / $count, 4) })
}

if ($SelfTest) {
    $test = [System.Drawing.Bitmap]::new(2, 1)
    try {
        $test.SetPixel(0, 0, [System.Drawing.Color]::FromArgb(10, 20, 30))
        $test.SetPixel(1, 0, [System.Drawing.Color]::FromArgb(30, 40, 50))
        $mean = Get-MeanRgb $test ([System.Drawing.Rectangle]::new(0, 0, 2, 1))
        if (($mean -join ',') -ne '20,30,40') { throw 'Mean RGB self-test failed.' }
        $rejected = $false
        try { Get-MeanRgb $test ([System.Drawing.Rectangle]::new(1, 0, 2, 1)) | Out-Null }
        catch { $rejected = $true }
        if (-not $rejected) { throw 'Out-of-bounds region was accepted.' }
        Write-Output 'self-test: exact region RGB means and bounds rejection OK'
    } finally { $test.Dispose() }
    return
}

$pngPath = [System.IO.Path]::GetFullPath($OutputPrefix + '.png')
$jsonPath = [System.IO.Path]::GetFullPath($OutputPrefix + '.json')
if ((Test-Path -LiteralPath $pngPath) -or (Test-Path -LiteralPath $jsonPath)) {
    throw 'Comparison output already exists; choose a new prefix.'
}
$camera = Get-Content -LiteralPath $CameraJson -Raw | ConvertFrom-Json
if ($camera.schema -ne 'fparkan-legacy-camera-v1' -or $camera.viewport.Count -ne 4) {
    throw 'Expected a legacy camera JSON with a four-value viewport.'
}
$viewport = @($camera.viewport)
foreach ($value in $viewport) {
    if ($value -lt 0 -or $value -gt [int]::MaxValue -or [Math]::Truncate($value) -ne $value) {
        throw 'Viewport coordinates must be nonnegative integers.'
    }
}
$crop = [System.Drawing.Rectangle]::new($viewport[0], $viewport[1], $viewport[2] - $viewport[0], $viewport[3] - $viewport[1])
$native = [System.Drawing.Bitmap]::new((Resolve-Path -LiteralPath $NativePng).Path)
$viewer = $nativeCrop = $pair = $graphics = $null
try {
    $viewer = [System.Drawing.Bitmap]::new((Resolve-Path -LiteralPath $ViewerPng).Path)
    if ($crop.Width -le 0 -or $crop.Height -le 0 -or $crop.Right -gt $native.Width -or $crop.Bottom -gt $native.Height -or
        $viewer.Width -ne $crop.Width -or $viewer.Height -ne $crop.Height) {
        throw 'Native crop and viewer dimensions must match without scaling.'
    }
    $nativeCrop = $native.Clone($crop, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $pair = [System.Drawing.Bitmap]::new($viewer.Width * 2, $viewer.Height + 24)
    $graphics = [System.Drawing.Graphics]::FromImage($pair)
    $graphics.Clear([System.Drawing.Color]::Black)
    $graphics.DrawImageUnscaled($nativeCrop, 0, 24)
    $graphics.DrawImageUnscaled($viewer, $viewer.Width, 24)
    $font = [System.Drawing.SystemFonts]::DefaultFont
    $graphics.DrawString('Original', $font, [System.Drawing.Brushes]::White, 4, 4)
    $graphics.DrawString('FParkan', $font, [System.Drawing.Brushes]::White, $viewer.Width + 4, 4)
    $regions = @(Get-Content -LiteralPath $RegionsJson -Raw | ConvertFrom-Json)
    if ($regions.Count -eq 0) { throw 'At least one named region is required.' }
    $report = foreach ($region in $regions) {
        if ([string]::IsNullOrWhiteSpace($region.name) -or $region.rect.Count -ne 4) { throw 'Each region needs name and rect [x,y,width,height].' }
        foreach ($value in $region.rect) {
            if ($value -lt 0 -or $value -gt [int]::MaxValue -or [Math]::Truncate($value) -ne $value) { throw 'Region coordinates must be nonnegative integers.' }
        }
        $rect = [System.Drawing.Rectangle]::new($region.rect[0], $region.rect[1], $region.rect[2], $region.rect[3])
        $originalMean = Get-MeanRgb $nativeCrop $rect
        $viewerMean = Get-MeanRgb $viewer $rect
        foreach ($offset in @(0, $viewer.Width)) {
            $graphics.DrawRectangle([System.Drawing.Pens]::Yellow, $rect.X + $offset, $rect.Y + 24, $rect.Width, $rect.Height)
            $graphics.DrawString($region.name, $font, [System.Drawing.Brushes]::Yellow, $rect.X + $offset, $rect.Y + 24)
        }
        [pscustomobject]@{ name = $region.name; rect = $region.rect; original_rgb = $originalMean; fparkan_rgb = $viewerMean }
    }
    $pair.Save($pngPath, [System.Drawing.Imaging.ImageFormat]::Png)
    [pscustomobject]@{ native = $NativePng; viewer = $ViewerPng; camera = $CameraJson; rgb_encoding = 'stored image bytes, 0..255'; regions = @($report) } |
        ConvertTo-Json -Depth 6 | Set-Content -LiteralPath $jsonPath -Encoding UTF8
    Write-Output "Wrote $pngPath and $jsonPath"
} finally {
    foreach ($resource in @($graphics, $pair, $nativeCrop, $viewer, $native)) {
        if ($null -ne $resource) { $resource.Dispose() }
    }
}
