# Rezolus External Metrics Specification

## Version 1.0

## Overview

This specification defines a custom binary file format for ingesting external
metrics into Rezolus via memory-mapped files. The format is designed for
high-performance, lock-free communication between a producer process (writing
metrics) and a consumer process (Rezolus agent) using `mmap()` on Linux systems.

## Design Goals

- **Zero-copy access**: Direct memory mapping for maximum performance
- **Lock-free reads**: Consumer can read without blocking producer
- **Simple serialization**: No external dependencies required
- **Concurrent safety**: Safe for one writer, multiple readers
- **Static metric set**: Metrics defined at file creation, no runtime additions

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

## Header Section

The header is exactly 64 bytes and contains:

| Offset | Size | Field | Description |
|--------|------|-------|-------------|
| 0 | 4 | Magic | Magic number: `0x52455A4C` ("REZL") |
| 4 | 1 | Version Major | Major version number (current: 1) |
| 5 | 1 | Version Minor | Minor version number (current: 0) |
| 6 | 1 | Status | Control byte (bits 0-1: ready flags) |
| 7 | 1 | Reserved | Must be zero |
| 8 | 4 | Metric Count | Number of metrics in catalog (u32, max 1024) |
| 12 | 4 | Catalog Size | Size of catalog section in bytes (u32) |
| 16 | 1 | Checksum Type | Checksum algorithm (0=None, 1=CRC32-IEEE) |
| 17 | 3 | Reserved | Must be zero |
| 20 | 4 | Catalog Checksum | CRC32 of catalog section (0 if type=0) |
| 24 | 8 | Data Offset | Offset to data section (u64) |
| 32 | 8 | Data Size | Size of data section in bytes (u64) |
| 40 | 8 | Timestamp | Unix timestamp of file creation (u64) |
| 48 | 16 | Reserved | Must be zero |

### Status Byte Details

- **Bit 0 (Catalog Ready)**: Set to 1 when catalog is complete and checksum
valid
- **Bit 1 (Data Ready)**: Set to 1 when data section is initialized and ready
for consumption
- **Bits 2-7**: Reserved, must be zero

Both bits must be set for the file to be considered ready for consumption.

**Status Byte Examples:**
```c
// Check catalog ready (bit 0)
bool catalog_ready = (status_byte & 0x01) != 0;

// Check data ready (bit 1)  
bool data_ready = (status_byte & 0x02) != 0;

// Check both ready
bool file_ready = (status_byte & 0x03) == 0x03;

// Status byte values during initialization:
// 0x00 - neither ready (initial state)
// 0x01 - catalog ready, data not ready
// 0x03 - both ready (final state)
```

## Metrics Catalog Section

The catalog immediately follows the header and contains a sequential list of
metric definitions. Each entry has the following structure:

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
   - Bucket count: `(max_value_power - grouping_power + 1) * 2^grouping_power`

### Metric Name Requirements

- Must be 1-255 bytes in length
- Must be valid UTF-8
- Should follow Prometheus metric naming conventions:
https://prometheus.io/docs/concepts/data_model/#metric-names-and-labels

## Data Section

The data section contains the actual metric values in the exact same order as
defined in the catalog. Metrics MUST appear in the data section in the same
sequence as their corresponding entries in the catalog section. Each metric's
data is stored sequentially with no padding between metrics, since all metric
types are naturally 8-byte aligned.

### Data Layout

```
Counter:      [8 bytes: u64 value]
Gauge:        [8 bytes: i64 value]
H2Histogram:  [8*N bytes: N u64 bucket counters stored as contiguous array]
```

**H2Histogram Storage Details:**
- All buckets for one histogram are stored as a contiguous array of u64 values
- Bucket count N is calculated as:
`(max_value_power - grouping_power + 1) * 2^grouping_power`
- Bucket ordering and value-to-bucket mapping is defined by the h2histogram
algorithm
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

### Memory Alignment

- **8-byte aligned** means the file offset is evenly divisible by 8 (i.e.,
`offset % 8 == 0`)
- The header is exactly 64 bytes (naturally 8-byte aligned)
- The catalog section starts immediately after the header at offset 64
- The data section MUST start at an 8-byte aligned file offset
- Padding bytes (set to zero) are inserted after the catalog as needed to
achieve alignment
- All metric data within the data section starts at 8-byte aligned file offsets
- After mmap(), memory addresses inherit the alignment from file offsets

### Data Section Offset Calculation

The data section offset is calculated as:
```
data_section_offset = 64 + ((catalog_size + 7) / 8) * 8
```

This ensures the data section starts at an 8-byte aligned address.

**Example:**
- Header: 64 bytes (ends at offset 64)
- Catalog: 250 bytes (ends at offset 314)
- Padding needed: 8 - (250 % 8) = 6 bytes
- Data section starts at: 64 + 250 + 6 = 320 (divisible by 8)
- Padding bytes (314-319) are set to zero

## Concurrency Model

### Producer Responsibilities

1. **Initialization**:
   - Create file with appropriate size
   - Write header with ready flags = 0
   - Write complete catalog section
   - Calculate checksum if checksum type != 0 (covers entire catalog region
   including padding)
   - Write checksum to header
   - Issue memory barrier (`msync(MS_SYNC)` or `fdatasync()` - see note below)
   - Set catalog ready flag = 1
   - Zero-initialize data section
   - Set data ready flag = 1

**Memory Barrier Note**: The memory barrier ensures that catalog data is visible
to consumers before the catalog ready flag is set. This is primarily needed when
the catalog spans multiple memory pages. For optimization, producers may skip
the barrier if the entire catalog fits within the first 4KB page:
```c
catalog_end_offset = 64 + catalog_size;
if (catalog_end_offset > 4096) {
    msync(mapped_addr, catalog_end_offset, MS_SYNC);
}
```

**Checksum Scope**: When checksum type is 1 (CRC32-IEEE), the checksum covers
the entire catalog region from offset 64 to the start of the data section,
including any padding bytes. Use the standard IEEE 802.3 CRC32 polynomial
(0x04C11DB7). This ensures complete integrity of the catalog region:
```c
checksum_start = 64;
checksum_length = data_section_offset - 64;
crc32_value = crc32_ieee(mapped_addr + checksum_start, checksum_length);
```

2. **Runtime Updates**:
   - Only modify values in data section
   - All 64-bit writes are naturally atomic due to 8-byte alignment
   - Never modify header or catalog after both ready flags are set

### Consumer Responsibilities

1. **Discovery**: Use inotify to detect new files
2. **Validation**: Check magic number, major version compatibility, and both
ready flags
3. **Catalog verification**: If checksum type != 0, validate catalog checksum
4. **Mapping**: mmap() the entire file as read-only
5. **Reading**: Poll data section periodically, ignore incomplete writes

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

### Creation Process

1. Producer calculates required file size
2. Creates file with full size (using `fallocate()` or similar)
3. Maps file into memory
4. Writes header (ready flag = 0)
5. Writes catalog section
6. Initializes data section to zeros
7. Sets ready flag = 1

### Discovery Process

1. Consumer monitors directory with inotify
2. On new file detection, attempts to mmap()
3. Validates magic number and major version compatibility
4. Waits for both ready flags = 1 (with timeout)
5. Verifies catalog checksum (if checksum type != 0)
6. Parses catalog and begins periodic reading

### File Management

- Producer is responsible for removing old files
- Producer should clean up partial files on creation failure
- Consumer should handle graceful unmapping when files disappear
- Files may use any naming convention (filename becomes metric attribute)
- Consumer only requires read access to files (maps with `PROT_READ`)
- Recommended file permissions: `0644` (owner read/write, group/other read)
- Permissions may be more restrictive based on security requirements
- Directory must be readable and executable by consumer process

### Error Recovery

**Producer responsibilities:**
- Must clean up partial files on creation failure (or handle cleanup externally)
- Retry behavior is implementation-specific
- Failed file creation may result in missing observability data

**Consumer behavior:**
- Ignores files where both ready flags are not set
- Should implement reasonable timeout for ready flag detection
- Skips malformed or incomplete files without blocking operation

**Partial file detection:**
Files are considered incomplete if:
- Ready flags remain 0 after reasonable timeout
- File size doesn't match header specifications  
- Validation errors during catalog parsing

## Error Handling

### Invalid Files

Consumers should reject files with:
- Incorrect magic number (including endianness mismatches)
- Unsupported major version
- Invalid metric types
- Catalog/data size mismatches
- Names longer than 255 bytes
- Invalid UTF-8 in metric names
- More than 1024 metrics in catalog
- Catalog checksum mismatch (if checksum type != 0)
- Unsupported checksum type
- File size doesn't match expected size from header calculations

### Runtime Errors

- File disappearance during reading (handle SIGBUS)
- Incomplete writes (ignore transient values)
- Catalog parsing errors (skip malformed entries)
- Histogram inconsistency during updates (acceptable - each bucket is
individually consistent)

## Performance Considerations

### File Size Calculation

```
Header Size = 64 bytes
Catalog Size = Σ(metric_entry_size) for all metrics
Catalog Padding = (8 - (catalog_size % 8)) % 8
Data Section Offset = 64 + catalog_size + catalog_padding
Data Size = Σ(metric_data_size) for all metrics
Total File Size = data_section_offset + data_size

Where metric_entry_size includes:
- 1 byte (type) + 0-2 bytes (config) + 1 byte (name_length) + name_length
```

### Optimal Access Patterns

- Consumers should read entire data section in sequential order
- Avoid random access patterns within large histogram data
- Consider using `madvise(MADV_SEQUENTIAL)` for large files

## Versioning

### Version 1.0 (Current)

- Initial implementation as specified above
- Magic number: `0x52455A4C`
- Major version: 1, Minor version: 0

### Version Compatibility Rules

- **Major version changes**: Breaking changes that require consumer updates
  - Changes to header structure
  - Changes to catalog format
  - Changes to data layout
  - Removal of required fields

- **Minor version changes**: Backward-compatible additions
  - New optional fields in reserved space
  - New metric types (with graceful degradation)
  - New checksum algorithms
  - Additional status flags

### Consumer Version Handling

- Consumers MUST support the exact major version they were built for
- Consumers SHOULD accept any minor version >= their supported version
- Consumers MUST reject files with unsupported major versions
- Consumers MAY warn about unknown minor version features but continue
processing

## Example Usage

### Typical File Sizes

- 10 counters: ~200 bytes
- 10 gauges: ~200 bytes
- 1 histogram (976 buckets): ~8KB
- Mixed workload (50 metrics): ~10-50KB

### Implementation Notes

- Use `MAP_SHARED` for producer, `MAP_PRIVATE` or `MAP_SHARED` for consumer
- Consider `MAP_POPULATE` to avoid page faults during time-critical reads
- Producer should use `msync()` or `fdatasync()` if durability is required

## Security Considerations

- No authentication or encryption required per specification
- File permissions should restrict access to appropriate users/groups
- Consider using temporary directories with restricted access
- Validate all input data to prevent buffer overflows

## Compliance and Standards

- All multi-byte integers use native endianness of the target platform
- UTF-8 encoding for all text fields
- Unix timestamps in seconds since epoch
- Metric names should follow Prometheus conventions where applicable
- Files may use any naming convention - filename becomes a metric attribute

### Byte Order Examples

**Little-endian (x86_64) representation of u64 value 0x0123456789ABCDEF:**
```
Offset: 0x00 0x01 0x02 0x03 0x04 0x05 0x06 0x07
Bytes:  0xEF 0xCD 0xAB 0x89 0x67 0x45 0x23 0x01
```

**Big-endian (some aarch64) representation of the same value:**
```
Offset: 0x00 0x01 0x02 0x03 0x04 0x05 0x06 0x07
Bytes:  0x01 0x23 0x45 0x67 0x89 0xAB 0xCD 0xEF
```

Producers and consumers must use the same native byte order. Cross-platform
compatibility requires matching endianness - mismatched endianness will cause
the magic number to appear incorrect and the file will be rejected.