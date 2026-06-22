# ADR-0005: Deterministic Reference Runtime

Status: accepted

Stages 0-5 use a single-threaded deterministic reference profile. Stable ordering, explicit ticks, named random streams, and canonical captures are part of the contract. Wall-clock time, pointer addresses, hash iteration order, and GPU handles are not semantic inputs.
