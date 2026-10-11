# Native frame capture

This x86 helper reads one live frame from the isolated GOG-compatible scratch copy of `Parkan - Iron Strategy`. It writes a PNG and a `fparkan-legacy-camera-v1` JSON file. The 64-byte camera matrix is sampled at the World3D render entry and reread at the projection-building boundary; only a same-thread, byte-compared snapshot paired with perspective values can produce usable renderer input. A passive `iron3d.dll` callsite probe records a path only when the active call arguments and path string are verified. It also samples `Terrain+0x421DC` after verifying the GOG module hash and the `EBP - [ESI+0x144]`, remainder by `[ESI+0x150]` instruction bytes. `atmosphere_seconds` is emitted only if the sampled `EDX` phase matches that arithmetic, the raw clock matches `World3D+0x32A38`, and sample, camera, and projection share the same render thread and generation. Otherwise it is `null`. This phase does not establish a general simulation-time, weather, or RNG value.

The JSON keeps the `fparkan-legacy-camera-v1` schema and adds `native_fog_status`, diagnostic counts, and `native_fog`. The latter is an optional passive CPU-side snapshot of the effective sink flags/RGB/range plus the paired calls to the `Terrain+0x2F230` consumer. The flags byte is raw and not a decoded fog mode. Each draw record preserves the distance argument, raw bypass dword and effective low byte, source color, output pointers, and return address. Geometry at `EBP` is included only for the verified `Terrain+0x18A6A` caller with the expected geometry vtable and slot+0x14 target; other callers have `distance_geometry: null`. The fog object is `null` unless the samples match the same render thread, camera, generation, and projection matrix and agree on sink settings. `native_fog_status` is `paired`, `conflict`, `unpaired`, or `incomplete`; `native_fog_status_reason` explains an unpaired, conflicting, or incomplete result. The counts include processed, overflow, paired, rejected, other-thread, and matrix-mismatch calls. At most 512 calls are processed per camera generation; reaching the limit stops sampling and reports an incomplete result. These fields are additive: camera-input validation continues to accept older JSON without them. This probe observes CPU consumer state; it does not set fog or change GPU state.

The capture helper expects an isolated copy at `target\shadow-probe\Parkan - Iron Strategy`; the original GOG installation stays untouched. For the default local install path, create that scratch copy from the repository root with:

```powershell
$originalGame = 'C:\GOG Games\Parkan - Iron Strategy'
$scratchGame = 'target\shadow-probe\Parkan - Iron Strategy'
New-Item -ItemType Directory -Force -Path (Split-Path $scratchGame) | Out-Null
robocopy $originalGame $scratchGame /E /COPY:DAT /R:1 /W:1
if ($LASTEXITCODE -ge 8) { throw 'Scratch copy failed; inspect robocopy output.' }
```

Build the viewer from the repository root with `cargo build --release -p fparkan-game`. No package installation is needed.

Build and run the ABI self-check from the repository root:

```powershell
& 'C:\Windows\Microsoft.NET\Framework\v4.0.30319\csc.exe' /nologo /platform:x86 /r:System.Drawing.dll /r:System.Web.Extensions.dll /out:target\native-frame-capture.exe tools\native-frame-capture\NativeFrameCapture.cs tools\native-frame-capture\NativeFrameCapture.Readback.cs
if ($LASTEXITCODE -ne 0) { throw 'Native capture helper build failed; inspect compiler output.' }
& 'target\native-frame-capture.exe' --self-check
if ($LASTEXITCODE -ne 0) { throw 'Native capture helper self-check failed; inspect its output.' }
```

Start the scratch executable from its own directory, then pass its PID and UTC start ticks to the helper. Keep the game visible while it waits; the helper does not synthesize input. It waits up to five minutes for a live world to reach the verified camera, projection, and Present boundaries. The default startup can remain at the shell until a mission begins, so a timeout is not a successful capture. The helper rejects other executable paths, stale PIDs, and mismatched game DLLs. Output must remain under `target`.

```powershell
$game = (Resolve-Path 'target\shadow-probe\Parkan - Iron Strategy').Path
$process = Start-Process -FilePath (Join-Path $game 'iron_3d.exe') -WorkingDirectory $game -PassThru
& 'target\native-frame-capture.exe' --capture $process.Id $process.StartTime.ToUniversalTime().Ticks target\native-frame-capture\frame.json
if ($LASTEXITCODE -ne 0) { throw 'Native frame capture failed; inspect its output and choose fresh output paths before retrying.' }
```

After a passive capture succeeds, its JSON can serve as the optional selected-camera input. The following example uses the matrix and projection values from `frame.json` and writes a new basename:

```powershell
$cameraInput = 'target\native-frame-capture\frame.json'
$selectedFrame = 'target\native-frame-capture\selected-frame.json'
& 'target\native-frame-capture.exe' --validate-camera-input $cameraInput
if ($LASTEXITCODE -ne 0) { throw 'Camera JSON is not a usable fparkan-legacy-camera-v1 input.' }
& 'target\native-frame-capture.exe' --capture $process.Id $process.StartTime.ToUniversalTime().Ticks $selectedFrame --camera-input $cameraInput
if ($LASTEXITCODE -ne 0) { throw 'Selected-camera capture failed; inspect its log before retrying.' }
```

`--validate-camera-input` parses and checks the JSON before opening a game process. The input must contain a valid `fparkan-legacy-camera-v1` matrix and projection values. At runtime the helper first observes an unmodified camera render and requires its viewport, near/far planes, and FOV to match the input. It applies only the 64-byte camera matrix to that same camera on the next same-thread render and reads the resulting surface at the verified Present boundary while the selected matrix is still active. At the projection boundary it verifies that the active matrix still exactly matches the requested input, as well as the projection values. It then restores the original matrix through the verified setter at that render invocation's return boundary. Pixels remain in memory until the original Present context, camera matrix, armed breakpoints, and debugger attachment have all been restored; the helper creates the PNG and JSON only after confirmed detach. It rejects output if the projection or requested matrix no longer matches, the selected matrix changes before return, or another render invalidates pixel attribution. Two repeated captures verified the selected matrix, projection, exact restoration, readback, and detach.

This verifies repeatable camera handling, not deterministic whole-scene pixels. The helper has no setter for FOV, near/far planes, or fog, and does not freeze simulation state; `atmosphere_seconds` remains an observed phase sample rather than a restored simulation clock. If the callsite probe cannot verify a mission path, `mission_path` remains `null` and `mission_identity` remains `unknown`.

For a second capture, reuse the still-running PID only after the first run detached successfully, and choose a new output basename. Reuse the same validated camera input if comparing the same pose. The helper rejects any existing JSON, PNG, or log with the chosen basename and never overwrites those files. After a successful one-shot capture it detaches and leaves the scratch game running. It explicitly closes only image-file handles from `CREATE_PROCESS` and `LOAD_DLL`; the system owns the process/thread handles supplied by `CREATE_PROCESS` and `CREATE_THREAD` until the corresponding exit event is continued. If guarded recovery must terminate the scratch process, the helper drains its outstanding and queued debug events through `EXIT_PROCESS` within five seconds, then verifies the process handle before detaching. It withholds output when process exit cannot be confirmed.

Live verification on 2026-10-12 produced two sequential AutoDemo captures on PID 15792. Both detached successfully and left the same process running. The first paired 157 of 157 native fog calls; the second paired 250 of 250. Both had zero rejected, cross-thread, or camera/projection-mismatch samples, and both verified the Terrain module hash and same-generation camera/projection pairing.

To compare a viewer frame with a native capture, use the same phase JSON for the camera and the PNG conversion. When `atmosphere_seconds` is present, the viewer uses it unless `--atmosphere-seconds` is given, and holds the atmosphere schedule at that sample for the fixed-camera readback. Choose fresh `viewer.bin` and `viewer.png` paths because the viewer writes the raw file and conversion refuses to overwrite an existing PNG.

```powershell
$cameraJson = 'target\native-frame-capture\frame.json'
$viewerRaw = 'target\native-frame-capture\viewer.bin'
$viewerPng = 'target\native-frame-capture\viewer.png'

cargo build --release -p fparkan-game
if ($LASTEXITCODE -ne 0) { throw 'Viewer build failed; inspect cargo output.' }
& '.\tools\native-frame-capture\convert-viewer-readback.ps1' -SelfTest
& 'target\release\fparkan-game.exe' `
  --root 'target\shadow-probe\Parkan - Iron Strategy' `
  --mission 'MISSIONS\Autodemo.00\data.tma' `
  --legacy-camera-capture $cameraJson `
  --frames 3 `
  --readback-out $viewerRaw
if ($LASTEXITCODE -ne 0) { throw 'Viewer readback failed; inspect its output before converting any raw file.' }
& '.\tools\native-frame-capture\convert-viewer-readback.ps1' `
  -InputRaw $viewerRaw `
  -CameraJson $cameraJson `
  -VkFormat 37 `
  -OutputPng $viewerPng
```

The converter derives raw image dimensions from the `viewport` rectangle. If
`pixel_capture` is present, its width and height describe the full native PNG;
the viewport must fit within those bounds, but can be a smaller crop with a
nonzero origin. The viewer readback itself contains only the viewport extent.
The converter checks the exact raw byte length (up to 64 MiB), refuses outputs
outside `target` or existing PNG paths, and accepts VkFormat `37`/`43` for RGBA
or `44`/`50` for BGRA. Pass the viewer log's `readback_format` value; `37` is
the verified value for the run above, not a universal surface format. The
converter uses the installed `System.Drawing` PNG encoder and adds no package
dependency.
