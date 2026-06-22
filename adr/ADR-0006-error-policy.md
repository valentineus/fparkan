# ADR-0006: Error Policy

Status: accepted

Missing required resources, malformed bytes, unsupported documented branches, capability mismatches, and budget failures are structured errors. Runtime code must not silently skip mandatory objects or convert corrupted data into empty success values.
