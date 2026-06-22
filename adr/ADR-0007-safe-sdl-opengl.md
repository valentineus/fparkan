# ADR-0007: Safe SDL/OpenGL Boundary

Status: provisional

Workspace-owned code forbids `unsafe`. SDL/OpenGL adapters must use maintained
external crates behind a safe project API; local Objective-C/CGL/SDL/OpenGL FFI
inside FParkan is not an acceptable implementation strategy.

The current adapter crates are safe boundary stubs. They compile the intended
ports and deterministic command contracts, but they do not create SDL windows,
GL contexts, GPU resources, shaders, draw calls, swapchains, or presents. They
must not be treated as backend readiness evidence.

To close the macOS backend requirement, choose and vendor/lock a maintained
safe facade stack, then implement:

- SDL event source, window creation, GL context lifecycle, drawable size and
  present;
- GL shader compile/link, buffer/texture upload, render state, draw calls and
  diagnostics;
- game/viewer composition roots using those adapters;
- hidden-window/offscreen macOS smoke tests and licensed local model/terrain
  frame captures.

Until those are implemented, Desktop GL evidence may document external probes
only; it does not satisfy the permanent adapter requirement.
