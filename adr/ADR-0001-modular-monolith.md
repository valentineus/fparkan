# ADR-0001: Modular Monolith

Status: accepted

FParkan is implemented as one Cargo workspace with local crates grouped by domain. Binaries and adapters compose domain crates; domain crates do not import platform, windowing, OpenGL, GUI, or application packages.
