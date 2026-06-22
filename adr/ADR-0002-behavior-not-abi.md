# ADR-0002: Behavior Compatibility, Not ABI Compatibility

Status: accepted

The project targets clean-room behavior compatibility for formats, resource lookup, loading order, deterministic runtime behavior, and presentation command semantics. It does not reproduce original DLL boundaries, exports, calling conventions, object layouts, RVAs, or native singleton access patterns.
