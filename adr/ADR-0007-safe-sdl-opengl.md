# ADR-0007: Safe SDL/OpenGL Boundary

Status: provisional

Workspace-owned code forbids `unsafe`. SDL/OpenGL adapters must use maintained external crates behind a safe project API. The current repository still contains legacy demo code scheduled for replacement; new adapter crates are placeholders until a fully audited safe facade is selected and proven on all target profiles.
