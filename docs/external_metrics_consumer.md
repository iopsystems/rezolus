# Rezolus External Metrics Consumer Implementation

## Version 1.0

## Overview

This document contains implementation notes for the expected behaviors of
Rezolus when consuming external metrics. See the External Metrics Specification
for more details about the file format.

## Consumer Responsibilities

1. **Discovery**: Use inotify to detect new files
2. **Validation**: Check magic number, major version compatibility, and ready
flag
3. **Mapping**: mmap() the entire file as read-only
4. **Reading**: Poll data section periodically

### Atomicity Guarantees

On x86_64 and aarch64 platforms with 8-byte aligned data:
- All 64-bit reads and writes are naturally atomic (no special instructions
needed)
- Individual histogram buckets are consistent during updates
- Histogram may be transiently inconsistent across buckets during updates
(acceptable)
- Counter and gauge updates are always atomic
- No memory ordering guarantees between different metrics

## File Lifecycle

### Discovery Process

1. On new file detection, attempts to mmap()
2. Validates magic number and major version compatibility
3. Waits for ready flag = 1 (with timeout)
4. Parses catalog and begins periodic reading

### File Management

- Consumer should handle graceful unmapping when files disappear
- Files may use any naming convention (filename becomes metric attribute)
- Consumer only requires read access to files (maps with `PROT_READ`)
- Directory must be readable and executable by consumer process

### Error Recovery

**Consumer behavior:**
- Ignores files where ready flag is not set
- Should implement reasonable timeout for ready flag detection
- Skips malformed or incomplete files without blocking operation

**Partial file detection:**
Files are considered incomplete if:
- Ready flag remains 0 after reasonable timeout
- File size doesn't match header specifications
- Validation errors during catalog parsing

## Error Handling

### Invalid Files

Consumers should reject files with:
- Incorrect magic number
- Unsupported major version
- Invalid metric types
- Invalid UTF-8 in metric names
- More than 1024 metrics in catalog
- File size doesn't match expected size

### Runtime Errors

- File disappearance during reading (handle SIGBUS)
- Catalog parsing errors (skip malformed entries)
- Histogram inconsistency during updates (acceptable - each bucket is
individually consistent)
