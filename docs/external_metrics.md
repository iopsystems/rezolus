# External Metrics Ingestion

Rezolus supports ingesting metrics from external processes via a Unix Domain
Socket (UDS). This enables sidecar applications, custom instrumentation, and
third-party tools to contribute metrics that are then exposed alongside
Rezolus's native telemetry.

## Overview

External processes connect to a Unix domain socket and push metrics using either
a binary protocol (optimized for efficiency) or a line protocol (optimized for
simplicity). Metrics are stored with a configurable TTL and automatically expire
if not refreshed. External metrics are exposed with an `ext_` prefix to
distinguish them from native Rezolus metrics.

## Configuration

Enable external metrics ingestion in `agent.toml`:

```toml
[external_metrics]
enabled = true
socket_path = "/var/run/rezolus/external.sock"
protocol = "auto"
metric_ttl = "60s"
max_connections = 1000
max_metrics = 100000
max_metrics_per_connection = 10000
```

| Option                       | Default                          | Description                            |
|------------------------------|----------------------------------|----------------------------------------|
| `enabled`                    | `false`                          | Enable external metrics ingestion      |
| `socket_path`                | `/var/run/rezolus/external.sock` | Path for the Unix domain socket        |
| `protocol`                   | `auto`                           | Protocol mode: `binary`, `line`, `auto`|
| `metric_ttl`                 | `60s`                            | Time-to-live before metrics expire     |
| `max_connections`            | `1000`                           | Maximum concurrent connections         |
| `max_metrics`                | `100000`                         | Global metric capacity limit           |
| `max_metrics_per_connection` | `10000`                          | Per-connection metric limit            |

When `protocol` is set to `auto`, the server auto-detects the protocol by
inspecting the first bytes of each connection.

## Metric Types

Three metric types are supported:

| Type          | Description                                                    |
|---------------|----------------------------------------------------------------|
| **Counter**   | Monotonically increasing unsigned 64-bit integer               |
| **Gauge**     | Point-in-time signed 64-bit integer (can increase or decrease) |
| **Histogram** | Distribution using HDR histogram with configurable precision   |

### Histogram Configuration

Histograms require two configuration parameters:

- **grouping_power** (0-7): Controls bucket precision. Higher values give finer
  granularity at the cost of more buckets.
- **max_value_power** (1-64): The maximum value is `2^max_value_power - 1`.

The number of buckets is determined by: `(max_value_power - grouping_power) * 2^grouping_power`

Example: `grouping_power=3, max_value_power=20` creates a histogram that can
store values up to ~1 million with 136 buckets.

## Session Labels

Both protocols support **session labels** - connection-level labels that are
automatically applied to all metrics sent on that connection. This avoids
repeating common labels (like `host`, `service`, or `instance`) with every
metric.

Session labels can be set once at connection start and apply to all subsequent
metrics. Metric-specific labels override session labels if there's a conflict.

## Line Protocol

A human-readable text protocol where each line represents one metric or
directive.

### Format

```
metric_name{label="value",label2="value2"} type:value
```

Components:
- **metric_name**: The metric name (required)
- **labels**: Optional key-value pairs in curly braces
- **type**: One of `counter`, `gauge`, or `histogram`
- **value**: The metric value

### Examples

```
# Counter
http_requests{method="GET",path="/api"} counter:12345

# Gauge (supports negative values)
temperature{location="cpu"} gauge:-5

# Gauge without labels
active_connections gauge:42

# Histogram (grouping_power,max_value_power:bucket_values)
request_latency_ns{service="api"} histogram:3,20:0 0 100 250 50 0 0 0
```

### Session Labels

Set session labels with the `# SESSION` directive:

```
# SESSION host="server01",service="myapp"
```

After this directive, all subsequent metrics on this connection will
automatically include `host="server01"` and `service="myapp"` labels.

### Parsing Rules

- Lines starting with `#` (except `# SESSION`) are comments and ignored
- Empty lines are ignored
- Label values must be quoted with double quotes
- Escaped characters in values: `\"`, `\\`

## Binary Protocol

An efficient binary protocol for high-throughput metric ingestion.

### Message Structure

```
Header (12 bytes):
  Bytes 0-3:   Magic bytes "REZL" (0x52 0x45 0x5A 0x4C)
  Byte 4:      Version major (1)
  Byte 5:      Version minor (0)
  Bytes 6-7:   Metric count (u16, little-endian)
  Bytes 8-11:  Payload size (u32, little-endian)

Payload:
  [Metric messages...]
```

Maximum message size: 65,536 bytes

### Message Types

| Type      | Value | Description                         |
|-----------|-------|-------------------------------------|
| SESSION   | 0     | Set session labels (no metric data) |
| COUNTER   | 1     | Counter metric                      |
| GAUGE     | 2     | Gauge metric                        |
| HISTOGRAM | 3     | Histogram metric                    |

### Metric Message Format

```
Type byte (1 byte)
Value (variable):
  - Counter: 8 bytes (u64, little-endian)
  - Gauge: 8 bytes (i64, little-endian)
  - Histogram:
      - grouping_power (1 byte)
      - max_value_power (1 byte)
      - bucket_count (2 bytes, u16 little-endian)
      - buckets (bucket_count * 8 bytes, u64 little-endian each)
Name length (2 bytes, u16 little-endian)
Name (UTF-8 string)
Label count (2 bytes, u16 little-endian)
Labels:
  For each label:
    - Key length (1 byte)
    - Key (UTF-8 string)
    - Value length (1 byte)
    - Value (UTF-8 string)
```

### Session Message Format

```
Type byte: 0 (SESSION)
Label count (2 bytes, u16 little-endian)
Labels:
  For each label:
    - Key length (1 byte)
    - Key (UTF-8 string)
    - Value length (1 byte)
    - Value (UTF-8 string)
```

## Metric Exposition

External metrics are exposed via the standard Rezolus HTTP endpoints:

- `/metrics/binary` - Msgpack format
- `/metrics/json` - JSON format

External metrics include the following metadata:

| Key               | Value                                       |
|-------------------|---------------------------------------------|
| `metric`          | Original metric name                        |
| `source`          | `external`                                  |
| Labels            | All metric labels as metadata               |
| `grouping_power`  | (Histograms only) Histogram grouping power  |
| `max_value_power` | (Histograms only) Histogram max value power |


## Safety Features

### Collision Prevention

External metrics that collide with internal Rezolus metric names are rejected.
The collision is logged and counted in the `collisions_blocked` diagnostic
counter.

### Capacity Limits

- **Global limit**: Total number of unique metrics across all connections
- **Per-connection limit**: Maximum metrics from a single connection

When limits are reached, new metrics are rejected until existing metrics expire.

### TTL Expiration

Metrics that are not updated within the configured TTL are automatically
removed. This prevents stale metrics from accumulating when external processes
disconnect or stop sending updates.

### Connection Limits

The maximum number of concurrent connections is configurable. New connections
are rejected when the limit is reached.

## Diagnostic Metrics

The external metrics store tracks operational statistics:

| Counter              | Description                            |
|----------------------|----------------------------------------|
| `received`           | Total metrics received                 |
| `parse_errors`       | Metrics that failed to parse           |
| `expired`            | Metrics removed due to TTL expiration  |
| `collisions_blocked` | Metrics rejected due to name collision |

## Example: Sending Metrics with netcat

Using the line protocol with netcat:

```bash
# Send a counter
echo 'http_requests{method="GET"} counter:100' | nc -U /var/run/rezolus/external.sock

# Send multiple metrics with session labels
cat <<EOF | nc -U /var/run/rezolus/external.sock
# SESSION service="myapp",host="server01"
requests_total counter:1000
active_users gauge:42
response_time_ns histogram:3,20:0 0 100 250 50
EOF
```

## Example: Python Client

```python
import socket
import struct

def send_counter(sock, name, value, labels=None):
    """Send a counter metric using the binary protocol."""
    labels = labels or {}

    # Build metric payload
    payload = bytearray()
    payload.append(1)  # COUNTER type
    payload.extend(struct.pack('<Q', value))  # u64 little-endian

    name_bytes = name.encode('utf-8')
    payload.extend(struct.pack('<H', len(name_bytes)))
    payload.extend(name_bytes)

    payload.extend(struct.pack('<H', len(labels)))
    for key, val in labels.items():
        key_bytes = key.encode('utf-8')
        val_bytes = val.encode('utf-8')
        payload.append(len(key_bytes))
        payload.extend(key_bytes)
        payload.append(len(val_bytes))
        payload.extend(val_bytes)

    # Build header
    header = bytearray()
    header.extend(b'REZL')  # Magic
    header.append(1)  # Version major
    header.append(0)  # Version minor
    header.extend(struct.pack('<H', 1))  # Metric count
    header.extend(struct.pack('<I', len(payload)))  # Payload size

    sock.sendall(header + payload)

# Usage
sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
sock.connect('/var/run/rezolus/external.sock')
send_counter(sock, 'my_counter', 42, {'service': 'myapp'})
sock.close()
```

## Example: C Client

```c
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/un.h>
#include <unistd.h>

#define SOCKET_PATH "/var/run/rezolus/external.sock"

// Message types
#define MSG_TYPE_SESSION   0
#define MSG_TYPE_COUNTER   1
#define MSG_TYPE_GAUGE     2
#define MSG_TYPE_HISTOGRAM 3

// Batch builder for sending multiple metrics in one message
typedef struct {
    uint8_t buf[65536];
    size_t offset;
    uint16_t count;
} metric_batch_t;

static void write_u16(uint8_t *buf, uint16_t val) {
    buf[0] = val & 0xFF;
    buf[1] = (val >> 8) & 0xFF;
}

static void write_u32(uint8_t *buf, uint32_t val) {
    for (int i = 0; i < 4; i++) buf[i] = (val >> (i * 8)) & 0xFF;
}

static void write_u64(uint8_t *buf, uint64_t val) {
    for (int i = 0; i < 8; i++) buf[i] = (val >> (i * 8)) & 0xFF;
}

void batch_init(metric_batch_t *b) {
    b->offset = 12;  // Reserve space for header
    b->count = 0;
}

void batch_set_session(metric_batch_t *b, const char *key, const char *value) {
    // Session message sets connection-level labels
    b->buf[b->offset++] = MSG_TYPE_SESSION;

    // Single label
    write_u16(b->buf + b->offset, 1);
    b->offset += 2;

    size_t key_len = strlen(key);
    b->buf[b->offset++] = (uint8_t)key_len;
    memcpy(b->buf + b->offset, key, key_len);
    b->offset += key_len;

    size_t val_len = strlen(value);
    b->buf[b->offset++] = (uint8_t)val_len;
    memcpy(b->buf + b->offset, value, val_len);
    b->offset += val_len;

    b->count++;
}

void batch_add_counter(metric_batch_t *b, const char *name, uint64_t value) {
    b->buf[b->offset++] = MSG_TYPE_COUNTER;
    write_u64(b->buf + b->offset, value);
    b->offset += 8;

    size_t name_len = strlen(name);
    write_u16(b->buf + b->offset, (uint16_t)name_len);
    b->offset += 2;
    memcpy(b->buf + b->offset, name, name_len);
    b->offset += name_len;

    write_u16(b->buf + b->offset, 0);  // No labels
    b->offset += 2;
    b->count++;
}

void batch_add_gauge(metric_batch_t *b, const char *name, int64_t value) {
    b->buf[b->offset++] = MSG_TYPE_GAUGE;
    write_u64(b->buf + b->offset, (uint64_t)value);
    b->offset += 8;

    size_t name_len = strlen(name);
    write_u16(b->buf + b->offset, (uint16_t)name_len);
    b->offset += 2;
    memcpy(b->buf + b->offset, name, name_len);
    b->offset += name_len;

    write_u16(b->buf + b->offset, 0);  // No labels
    b->offset += 2;
    b->count++;
}

void batch_add_histogram(metric_batch_t *b, const char *name,
                         uint8_t grouping_power, uint8_t max_value_power,
                         const uint64_t *buckets, uint16_t bucket_count) {
    b->buf[b->offset++] = MSG_TYPE_HISTOGRAM;

    // Histogram config
    b->buf[b->offset++] = grouping_power;
    b->buf[b->offset++] = max_value_power;
    write_u16(b->buf + b->offset, bucket_count);
    b->offset += 2;

    // Bucket values
    for (uint16_t i = 0; i < bucket_count; i++) {
        write_u64(b->buf + b->offset, buckets[i]);
        b->offset += 8;
    }

    size_t name_len = strlen(name);
    write_u16(b->buf + b->offset, (uint16_t)name_len);
    b->offset += 2;
    memcpy(b->buf + b->offset, name, name_len);
    b->offset += name_len;

    write_u16(b->buf + b->offset, 0);  // No labels
    b->offset += 2;
    b->count++;
}

int batch_send(metric_batch_t *b, int sock) {
    // Write header
    memcpy(b->buf, "REZL", 4);
    b->buf[4] = 1;  // Version major
    b->buf[5] = 0;  // Version minor
    write_u16(b->buf + 6, b->count);
    write_u32(b->buf + 8, (uint32_t)(b->offset - 12));

    return send(sock, b->buf, b->offset, 0) == (ssize_t)b->offset ? 0 : -1;
}

int main(void) {
    int sock = socket(AF_UNIX, SOCK_STREAM, 0);
    if (sock < 0) {
        perror("socket");
        return 1;
    }

    struct sockaddr_un addr = {0};
    addr.sun_family = AF_UNIX;
    strncpy(addr.sun_path, SOCKET_PATH, sizeof(addr.sun_path) - 1);

    if (connect(sock, (struct sockaddr *)&addr, sizeof(addr)) < 0) {
        perror("connect");
        close(sock);
        return 1;
    }

    // Batch multiple metrics into a single message
    metric_batch_t batch;
    batch_init(&batch);

    // Set session labels (applied to all metrics on this connection)
    batch_set_session(&batch, "service", "myapp");

    batch_add_counter(&batch, "http_requests", 100);
    batch_add_counter(&batch, "cache_hits", 50);
    batch_add_gauge(&batch, "active_connections", 42);
    batch_add_gauge(&batch, "queue_depth", 7);

    // Histogram with grouping_power=1, max_value_power=4 (8 buckets, max value 15)
    uint64_t latency_buckets[8] = {0, 5, 12, 8, 3, 2, 1, 0};
    batch_add_histogram(&batch, "request_latency_us", 1, 4,
                        latency_buckets, 8);

    if (batch_send(&batch, sock) < 0) {
        perror("send");
        close(sock);
        return 1;
    }

    printf("Sent %d metrics in one message\n", batch.count);
    close(sock);
    return 0;
}
```

## Example: Rust Client

```rust
use histogram::Histogram;
use std::io::{self, Write};
use std::os::unix::net::UnixStream;

const SOCKET_PATH: &str = "/var/run/rezolus/external.sock";

const MSG_TYPE_SESSION: u8 = 0;
const MSG_TYPE_COUNTER: u8 = 1;
const MSG_TYPE_GAUGE: u8 = 2;
const MSG_TYPE_HISTOGRAM: u8 = 3;

/// Batch builder for sending multiple metrics in one message
struct MetricBatch {
    payload: Vec<u8>,
    count: u16,
}

impl MetricBatch {
    fn new() -> Self {
        Self {
            payload: Vec::new(),
            count: 0,
        }
    }

    /// Set session labels (applied to all metrics on this connection)
    fn set_session(&mut self, labels: &[(&str, &str)]) {
        self.payload.push(MSG_TYPE_SESSION);
        self.payload.extend_from_slice(&(labels.len() as u16).to_le_bytes());
        for (key, val) in labels {
            self.payload.push(key.len() as u8);
            self.payload.extend_from_slice(key.as_bytes());
            self.payload.push(val.len() as u8);
            self.payload.extend_from_slice(val.as_bytes());
        }
        self.count += 1;
    }

    fn add_counter(&mut self, name: &str, value: u64, labels: &[(&str, &str)]) {
        self.payload.push(MSG_TYPE_COUNTER);
        self.payload.extend_from_slice(&value.to_le_bytes());
        self.write_name_and_labels(name, labels);
        self.count += 1;
    }

    fn add_gauge(&mut self, name: &str, value: i64, labels: &[(&str, &str)]) {
        self.payload.push(MSG_TYPE_GAUGE);
        self.payload.extend_from_slice(&value.to_le_bytes());
        self.write_name_and_labels(name, labels);
        self.count += 1;
    }

    fn add_histogram(&mut self, name: &str, histogram: &Histogram, labels: &[(&str, &str)]) {
        let config = histogram.config();
        let buckets = histogram.as_slice();

        self.payload.push(MSG_TYPE_HISTOGRAM);
        self.payload.push(config.grouping_power());
        self.payload.push(config.max_value_power());
        self.payload.extend_from_slice(&(buckets.len() as u16).to_le_bytes());
        for &bucket in buckets {
            self.payload.extend_from_slice(&bucket.to_le_bytes());
        }
        self.write_name_and_labels(name, labels);
        self.count += 1;
    }

    fn write_name_and_labels(&mut self, name: &str, labels: &[(&str, &str)]) {
        let name_bytes = name.as_bytes();
        self.payload.extend_from_slice(&(name_bytes.len() as u16).to_le_bytes());
        self.payload.extend_from_slice(name_bytes);

        self.payload.extend_from_slice(&(labels.len() as u16).to_le_bytes());
        for (key, val) in labels {
            self.payload.push(key.len() as u8);
            self.payload.extend_from_slice(key.as_bytes());
            self.payload.push(val.len() as u8);
            self.payload.extend_from_slice(val.as_bytes());
        }
    }

    fn send(self, stream: &mut UnixStream) -> io::Result<()> {
        let mut message = Vec::with_capacity(12 + self.payload.len());
        message.extend_from_slice(b"REZL");
        message.push(1);  // Version major
        message.push(0);  // Version minor
        message.extend_from_slice(&self.count.to_le_bytes());
        message.extend_from_slice(&(self.payload.len() as u32).to_le_bytes());
        message.extend_from_slice(&self.payload);
        stream.write_all(&message)
    }
}

fn main() -> io::Result<()> {
    let mut stream = UnixStream::connect(SOCKET_PATH)?;

    let mut batch = MetricBatch::new();

    // Set session labels (applied to all metrics on this connection)
    batch.set_session(&[("service", "myapp"), ("host", "server01")]);

    // Add counters and gauges
    batch.add_counter("http_requests", 100, &[("method", "GET")]);
    batch.add_counter("cache_hits", 50, &[]);
    batch.add_gauge("active_connections", 42, &[]);
    batch.add_gauge("queue_depth", 7, &[]);

    // Create and populate a histogram using the histogram crate
    let mut latency = Histogram::new(3, 20).expect("valid config");
    for &value in &[50, 120, 85, 200, 150, 90, 110, 95, 180, 75] {
        latency.increment(value).expect("value in range");
    }
    batch.add_histogram("request_latency_us", &latency, &[]);

    batch.send(&mut stream)?;
    println!("Sent metrics batch");
    Ok(())
}
```
