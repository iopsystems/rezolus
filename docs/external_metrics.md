# Rezolus External Metrics Specification

## Version 1.0

## Overview

This specification defines a binary file format for ingesting external metrics
into Rezolus via memory-mapped files. The format enables high-performance,
lock-free recording of external metrics using the Rezolus recorder. The file
format is designed so that it does not require external dependencies.

## File Structure

The file consists of three main sections:

```
┌─────────────────┐
│     Header      │
├─────────────────┤
│ Metrics Catalog │
├─────────────────┤
│   Data Section  │
└─────────────────┘
```

The file format is designed so the data section is 8-byte aligned to ensure that
read and write operations are naturally atomic on x86_64 and aarch64.

## Header Section

The header is exactly 16 bytes and contains:

| Offset | Size | Field | Description |
|--------|------|-------|-------------|
| 0 | 4 | Magic | Magic number: `0x52455A4C` ("REZL") |
| 4 | 1 | Version Major | Major version number (current: 1) |
| 5 | 1 | Version Minor | Minor version number (current: 0) |
| 6 | 1 | Status | Control byte (bits 0: ready flag) |
| 7 | 1 | Reserved | Must be zero |
| 8 | 4 | Metric Count | Number of metrics in catalog (u32, max 1024) |
| 12 | 4 | Catalog Size | Size of catalog section in bytes (u32) |

### Status Byte Details

- **Bit 0 (Ready)**: Set to 1 only after allocating entire file, populating
catalog, and syncing
- **Bits 1-7**: Reserved, must be zero

## Metrics Catalog Section

The catalog immediately follows the header and contains a sequential list of
metric definitions.

Note: The catalog section must be followed by zero-padding to ensure the data
section is 8-byte aligned.

Each entry has the following structure:

### Metric Entry Format

| Offset | Size | Field | Description |
|--------|------|-------|-------------|
| 0 | 1 | Type | Metric type (1=Counter, 2=Gauge, 3=H2Histogram) |
| 1 | 0-2 | Config | Configuration bytes (only for H2Histogram) |
| 1-3 | 1 | Name Length | Length of metric name (1-255) |
| 2-4 | N | Name | UTF-8 encoded metric name |

### Metric Types

1. **Counter (Type 1)**
   - No configuration bytes
   - Data: 8-byte unsigned integer (u64)

2. **Gauge (Type 2)**
   - No configuration bytes
   - Data: 8-byte signed integer (i64)

3. **H2Histogram (Type 3)**
   - Configuration: 2 bytes
     - Byte 1: Grouping Power (must be < max_value_power)
     - Byte 2: Max Value Power (must be ≤ 64)
   - Data: Variable number of 8-byte buckets

### Metric Name Requirements

- Must be 1-255 bytes in length
- Must be valid UTF-8
- Should follow Prometheus metric naming conventions:
https://prometheus.io/docs/concepts/data_model/#metric-names-and-labels

## Data Section

The data section contains the actual metric values in the exact same order as
defined in the catalog. Metrics MUST appear in the data section in the same
sequence as their corresponding entries in the catalog section. Each metric's
data is stored sequentially with no padding between metrics.

### Data Layout

```
Counter:      [8 bytes: u64 value]
Gauge:        [8 bytes: i64 value]
H2Histogram:  [8*N bytes: N u64 bucket counters stored as contiguous array]
```

**H2Histogram Storage Details:**
- All buckets for one histogram are stored as a contiguous array of u64 values
- For histogram implementation details, see: https://h2histogram.org and the
reference implementation: https://docs.rs/histogram

### Metric Data Offsets

To find a specific metric's data offset:
1. Start at the data section offset (from header)
2. Sum the data sizes of all preceding metrics in catalog order
3. Metrics MUST appear in data section in the same order as catalog entries
4. No padding bytes are needed between metrics

**Example for 3 metrics in catalog order:**
- Metric 0 (Counter): offset = data_section_offset + 0
- Metric 1 (Gauge): offset = data_section_offset + 8
- Metric 2 (H2Histogram, 252 buckets): offset = data_section_offset + 16

## File Lifecycle

## Naming and Permissions
- Files may use any naming convention (filename becomes metric attribute `name`)
- Consumer only requires read access to files (maps with `PROT_READ`)

### Creation
1. Calculates required file size
2. Create file with full size (`fallocate()`)
3. Map the file into memory using `MAP_SHARED`
4. Write header with ready flag set to zero
5. Write catalog section
6. Initialize data section with zeros
7. Set ready flag to one

### Modification
- Only modify values in data section

### Destruction
- Producer is responsible for removing old files
- Producer should clean up partial files if creation fails
