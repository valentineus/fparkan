[CmdletBinding(DefaultParameterSetName = 'Convert')]
param(
    [Parameter(Mandatory = $true, ParameterSetName = 'Convert')]
    [string]$InputRaw,

    [Parameter(Mandatory = $true, ParameterSetName = 'Convert')]
    [string]$CameraJson,

    [Parameter(Mandatory = $true, ParameterSetName = 'Convert')]
    [ValidateSet('37', '43', '44', '50')]
    [int]$VkFormat,

    [Parameter(Mandatory = $true, ParameterSetName = 'Convert')]
    [string]$OutputPng,

    [Parameter(Mandatory = $true, ParameterSetName = 'SelfTest')]
    [switch]$SelfTest
)

$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing

function Assert-ReadbackByteLength {
    param(
        [long]$ActualLength,
        [int]$Width,
        [int]$Height
    )

    $maximumLength = [long]64 * 1024 * 1024
    if ($Width -le 0 -or $Height -le 0) {
        throw [System.IO.InvalidDataException]::new('Readback dimensions must be positive.')
    }
    $expectedLength = [long]$Width * [long]$Height * 4
    if ($expectedLength -gt $maximumLength) {
        throw [System.IO.InvalidDataException]::new("Readback exceeds the 64 MiB limit: $expectedLength bytes.")
    }
    if ($ActualLength -ne $expectedLength) {
        throw [System.IO.InvalidDataException]::new(
            "Readback must contain exactly width * height * 4 bytes; got $ActualLength for ${Width}x${Height} (expected $expectedLength)."
        )
    }
}

function ConvertTo-ViewportCoordinate {
    param(
        [object]$Value,
        [string]$Name
    )

    try {
        $number = [System.Convert]::ToDecimal($Value, [System.Globalization.CultureInfo]::InvariantCulture)
    }
    catch {
        throw "Camera JSON viewport $Name must be an integer."
    }
    if ($number -ne [decimal]::Truncate($number) -or
        $number -lt [int]::MinValue -or $number -gt [int]::MaxValue) {
        throw "Camera JSON viewport $Name must be an in-range 32-bit integer."
    }
    return [int]$number
}

function Convert-ReadbackToBgra {
    param(
        [byte[]]$Source,
        [int]$Format,
        [int]$Width,
        [int]$Height
    )

    Assert-ReadbackByteLength -ActualLength $Source.Length -Width $Width -Height $Height

    # VK_FORMAT_R8G8B8A8_* (37, 43) needs R/B swapped for System.Drawing's
    # little-endian Format32bppArgb storage. VK_FORMAT_B8G8R8A8_* (44, 50)
    # already has the byte order expected by that bitmap format.
    if ($Format -eq 44 -or $Format -eq 50) {
        return ,$Source
    }

    $converted = New-Object byte[] $Source.Length
    for ($offset = 0; $offset -lt $Source.Length; $offset += 4) {
        $converted[$offset] = $Source[$offset + 2]
        $converted[$offset + 1] = $Source[$offset + 1]
        $converted[$offset + 2] = $Source[$offset]
        $converted[$offset + 3] = $Source[$offset + 3]
    }

    return ,$converted
}

function Assert-ViewportWithinCapture {
    param(
        [int]$Left,
        [int]$Top,
        [int]$Right,
        [int]$Bottom,
        [int]$CaptureWidth,
        [int]$CaptureHeight
    )

    if ($CaptureWidth -le 0 -or $CaptureHeight -le 0) {
        throw [System.IO.InvalidDataException]::new('Camera JSON pixel_capture dimensions must be positive.')
    }
    if ($Left -lt 0 -or $Top -lt 0 -or $Right -gt $CaptureWidth -or $Bottom -gt $CaptureHeight) {
        throw [System.IO.InvalidDataException]::new('Camera JSON viewport must fit within the full pixel_capture image bounds.')
    }
}

function Save-BgraPngToStream {
    param(
        [byte[]]$Pixels,
        [int]$Width,
        [int]$Height,
        [System.IO.Stream]$Stream
    )

    $bitmap = New-Object System.Drawing.Bitmap(
        $Width,
        $Height,
        [System.Drawing.Imaging.PixelFormat]::Format32bppArgb
    )
    $rectangle = New-Object System.Drawing.Rectangle(0, 0, $Width, $Height)
    $bitmapData = $null
    try {
        $bitmapData = $bitmap.LockBits(
            $rectangle,
            [System.Drawing.Imaging.ImageLockMode]::WriteOnly,
            [System.Drawing.Imaging.PixelFormat]::Format32bppArgb
        )

        $rowBytes = $Width * 4
        if ([Math]::Abs($bitmapData.Stride) -lt $rowBytes) {
            throw 'Bitmap stride is shorter than a pixel row.'
        }

        for ($row = 0; $row -lt $Height; $row++) {
            $destination = [IntPtr]::Add($bitmapData.Scan0, $row * $bitmapData.Stride)
            [System.Runtime.InteropServices.Marshal]::Copy(
                $Pixels,
                $row * $rowBytes,
                $destination,
                $rowBytes
            )
        }

        $bitmap.UnlockBits($bitmapData)
        $bitmapData = $null
        $bitmap.Save($Stream, [System.Drawing.Imaging.ImageFormat]::Png)
    }
    finally {
        if ($null -ne $bitmapData) {
            $bitmap.UnlockBits($bitmapData)
        }
        $bitmap.Dispose()
    }
}

function Get-CameraExtentFromObject {
    param([object]$Camera)

    $camera = $Camera
    if ($camera.schema -ne 'fparkan-legacy-camera-v1' -or $null -eq $camera.viewport -or $camera.viewport.Count -ne 4) {
        throw 'Camera JSON must be fparkan-legacy-camera-v1 and contain a four-value viewport.'
    }

    $left = ConvertTo-ViewportCoordinate -Value $camera.viewport[0] -Name 'left'
    $top = ConvertTo-ViewportCoordinate -Value $camera.viewport[1] -Name 'top'
    $right = ConvertTo-ViewportCoordinate -Value $camera.viewport[2] -Name 'right'
    $bottom = ConvertTo-ViewportCoordinate -Value $camera.viewport[3] -Name 'bottom'
    $widthLong = [long]$right - [long]$left
    $heightLong = [long]$bottom - [long]$top
    if ($widthLong -gt [int]::MaxValue -or $heightLong -gt [int]::MaxValue) {
        throw 'Camera JSON viewport extent exceeds 32-bit dimensions.'
    }
    $width = [int]$widthLong
    $height = [int]$heightLong
    if ($width -le 0 -or $height -le 0) {
        throw 'Camera JSON viewport has an empty or inverted extent.'
    }

    $expectedBytes = [long]$width * [long]$height * 4
    if ($expectedBytes -gt ([long]64 * 1024 * 1024)) {
        throw "Camera JSON viewport exceeds the 64 MiB readback limit: $expectedBytes bytes."
    }
    if ($null -ne $camera.pixel_capture) {
        $captureWidth = ConvertTo-ViewportCoordinate -Value $camera.pixel_capture.width -Name 'pixel_capture.width'
        $captureHeight = ConvertTo-ViewportCoordinate -Value $camera.pixel_capture.height -Name 'pixel_capture.height'
        Assert-ViewportWithinCapture -Left $left -Top $top -Right $right -Bottom $bottom -CaptureWidth $captureWidth -CaptureHeight $captureHeight
    }

    return [pscustomobject]@{ Width = $width; Height = $height }
}

function Get-CameraExtent {
    param([string]$Path)

    $camera = Get-Content -LiteralPath $Path -Raw | ConvertFrom-Json
    return Get-CameraExtentFromObject -Camera $camera
}

function Invoke-SelfTest {
    $rgba = [byte[]]@(
        255, 0, 0, 255,  0, 255, 0, 255,
        0, 0, 255, 255,  255, 255, 255, 255
    )
    $expectedBgra = [byte[]]@(
        0, 0, 255, 255,  0, 255, 0, 255,
        255, 0, 0, 255,  255, 255, 255, 255
    )

    $rgbaResult = Convert-ReadbackToBgra -Source $rgba -Format 37 -Width 2 -Height 2
    $bgraResult = Convert-ReadbackToBgra -Source $expectedBgra -Format 44 -Width 2 -Height 2
    for ($index = 0; $index -lt $expectedBgra.Length; $index++) {
        if ($rgbaResult[$index] -ne $expectedBgra[$index] -or $bgraResult[$index] -ne $expectedBgra[$index]) {
            throw 'Self-check failed: RGBA/BGRA channel mapping.'
        }
    }

    $mismatchRejected = $false
    try {
        Assert-ReadbackByteLength -ActualLength 15 -Width 2 -Height 2
    }
    catch [System.IO.InvalidDataException] {
        $mismatchRejected = $true
    }
    if (-not $mismatchRejected) {
        throw 'Self-check failed: wrong byte length was accepted.'
    }

    Assert-ReadbackByteLength -ActualLength ([long]64 * 1024 * 1024) -Width 4096 -Height 4096
    $oversizeRejected = $false
    try {
        Assert-ReadbackByteLength -ActualLength ([long]64 * 1024 * 1024) -Width 4097 -Height 4096
    }
    catch [System.IO.InvalidDataException] {
        $oversizeRejected = $true
    }
    if (-not $oversizeRejected) {
        throw 'Self-check failed: readback over the 64 MiB boundary was accepted.'
    }

    foreach ($invalidCoordinate in @([double]1.5, [long]2147483648)) {
        $coordinateRejected = $false
        try {
            ConvertTo-ViewportCoordinate -Value $invalidCoordinate -Name 'self-check'
        }
        catch {
            $coordinateRejected = $true
        }
        if (-not $coordinateRejected) {
            throw 'Self-check failed: fractional or out-of-range viewport coordinate was accepted.'
        }
    }

    # A crop can be smaller than the full native PNG and start away from (0,0).
    # Raw viewer readback is the crop only, so its dimensions stay 4x4 here.
    $cropCamera = [pscustomobject]@{
        schema = 'fparkan-legacy-camera-v1'
        viewport = @(4, 6, 8, 10)
        pixel_capture = [pscustomobject]@{ width = 12; height = 16 }
    }
    $cropExtent = Get-CameraExtentFromObject -Camera $cropCamera
    if ($cropExtent.Width -ne 4 -or $cropExtent.Height -ne 4) {
        throw 'Self-check failed: cropped readback extent was not derived from the viewport rectangle.'
    }
    Assert-ReadbackByteLength -ActualLength 64 -Width 4 -Height 4
    $cropOutsideCaptureRejected = $false
    try {
        $cropCamera.viewport = @(4, 6, 13, 10)
        Get-CameraExtentFromObject -Camera $cropCamera | Out-Null
    }
    catch [System.IO.InvalidDataException] {
        $cropOutsideCaptureRejected = $true
    }
    if (-not $cropOutsideCaptureRejected) {
        throw 'Self-check failed: viewport extending beyond the full pixel_capture image was accepted.'
    }

    $stream = New-Object System.IO.MemoryStream
    $decoded = $null
    try {
        Save-BgraPngToStream -Pixels $expectedBgra -Width 2 -Height 2 -Stream $stream
        $stream.Position = 0
        $decoded = [System.Drawing.Bitmap]::new($stream)
        $expectedColors = @(
            [System.Drawing.Color]::Red,
            [System.Drawing.Color]::FromArgb(255, 0, 255, 0),
            [System.Drawing.Color]::Blue,
            [System.Drawing.Color]::White
        )
        $points = @(@(0, 0), @(1, 0), @(0, 1), @(1, 1))
        for ($index = 0; $index -lt $points.Count; $index++) {
            $point = $points[$index]
            $actualColor = $decoded.GetPixel($point[0], $point[1])
            if ($actualColor.ToArgb() -ne $expectedColors[$index].ToArgb()) {
                throw "Self-check failed: PNG pixel $index expected $($expectedColors[$index]) but read $actualColor."
            }
        }
    }
    finally {
        if ($null -ne $decoded) { $decoded.Dispose() }
        $stream.Dispose()
    }

    'Self-check passed (2x2 RGBA/BGRA PNG, byte-length/64 MiB limits, viewport-coordinate bounds, and cropped pixel_capture bounds).'
}

if ($SelfTest) {
    Invoke-SelfTest
    exit 0
}

$repositoryRoot = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
$targetRoot = [System.IO.Path]::GetFullPath((Join-Path $repositoryRoot 'target'))
$rawPath = (Resolve-Path -LiteralPath $InputRaw).Path
$cameraPath = (Resolve-Path -LiteralPath $CameraJson).Path

if (-not (Test-Path -LiteralPath $rawPath -PathType Leaf)) { throw "Raw readback is not a file: $rawPath" }
if (-not (Test-Path -LiteralPath $cameraPath -PathType Leaf)) { throw "Camera JSON is not a file: $cameraPath" }

$outputPath = [System.IO.Path]::GetFullPath($OutputPng)
$targetPrefix = $targetRoot.TrimEnd('\', '/') + [System.IO.Path]::DirectorySeparatorChar
if (-not $outputPath.StartsWith($targetPrefix, [System.StringComparison]::OrdinalIgnoreCase)) {
    throw "Output must be beneath the repository target directory: $targetRoot"
}
if ([System.IO.Path]::GetExtension($outputPath) -ine '.png') { throw 'Output filename must end in .png.' }
if (Test-Path -LiteralPath $outputPath) { throw "Refusing to overwrite existing output: $outputPath" }
$outputDirectory = [System.IO.Path]::GetDirectoryName($outputPath)
if (-not (Test-Path -LiteralPath $outputDirectory -PathType Container)) {
    throw "Output directory does not exist: $outputDirectory"
}

$extent = Get-CameraExtent -Path $cameraPath
$rawInfo = Get-Item -LiteralPath $rawPath
Assert-ReadbackByteLength -ActualLength $rawInfo.Length -Width $extent.Width -Height $extent.Height
$source = [System.IO.File]::ReadAllBytes($rawPath)
$pixels = Convert-ReadbackToBgra -Source $source -Format $VkFormat -Width $extent.Width -Height $extent.Height
$stream = [System.IO.File]::Open($outputPath, [System.IO.FileMode]::CreateNew, [System.IO.FileAccess]::Write, [System.IO.FileShare]::None)
try {
    Save-BgraPngToStream -Pixels $pixels -Width $extent.Width -Height $extent.Height -Stream $stream
}
finally {
    $stream.Dispose()
}
"Wrote $($extent.Width)x$($extent.Height) PNG to $outputPath (VkFormat $VkFormat)."
