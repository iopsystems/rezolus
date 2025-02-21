//! Type definitions for the wire protocol making up the metrics subscription
//! endpoint for rezolus.
//!
//! # Wire Protocol
//! The wire protocol is a series a message frames made up as follows:
//! - An 8-byte little-endian header describing the length of the frame.
//!   This length does not include the 8 bytes for the length.
//! - A msgpack-serialized [`Message`] instance with the relevant message data.
//!
//! Rezolus will push messages down the connection. If the client does not
//! manage to keep up with the connection then rezolus will drop snapshot
//! messages until the client catches back up. No other message type will be
//! dropped.
//!
//! # Message Protocol
//! The message protocol always starts with a [`Message::Metadata`] message
//! that provides protocol-level metadata.
//!
//! After that the connection will periodically send the following messages:
//! - [`Message::Info`] is sent in order to provide information on new metrics
//!   being emitted via the subscription. An info frame for a metric will
//!   always be emitted before that metric is emitted as part of a snapshot.
//! - [`Message::Snapshot`] is sent periodically with a snapshot of all metrics
//!   that are emitted by rezolus.
//! - [`Message::Lost`] is sent after snapshots are lost due to the client not
//!   consuming them quickly enough or due to rezolus not being able to emit
//!   them fast enough.

use std::collections::HashMap;
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

#[non_exhaustive]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Message {
    /// Connection-level metadata information.
    ///
    /// This will always be the first message emitted on a new connection.
    Metadata(Metadata),

    /// Metadata information about future metrics that will be emitted.
    ///
    /// Rezolus will always emit a [`MetricInfo`] entry for a given metric ID
    /// _before_ that metric is emitted in a snapshot.
    Info(Vec<MetricInfo>),

    /// A snapshot of metrics exported by rezolus at a given point in time.
    Snapshot(Snapshot),

    /// An indication that some number of snapshots have been lost because the
    /// reader was not consuming messages quickly enough.
    Lost(u64),

    #[doc(hidden)]
    #[serde(other)]
    __Other,
}

#[non_exhaustive]
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum MetricType {
    Counter = 0,
    Gauge = 1,
    Histogram = 2,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MetricInfo {
    /// The name of this metric.
    ///
    /// This will be used to identify the metric in future messages.
    pub name: String,

    /// The type of this metric.
    #[serde(rename = "type")]
    pub ty: MetricType,

    /// Metadata for this metric.
    pub metadata: HashMap<String, String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Metadata {
    pub metadata: HashMap<String, String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Snapshot {
    /// The timestamp that this snapshot was recorded at.
    pub timestamp: SystemTime,

    pub counters: Vec<Counter>,
    pub gauges: Vec<Gauge>,
    pub histograms: Vec<Histogram>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Counter {
    /// A unique name for this counter.
    ///
    /// This will correspond to a previously emitted [`MetricInfo`] in an
    /// `Info` message.
    pub name: String,
    pub value: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Gauge {
    /// A unique name for this counter.
    ///
    /// This will correspond to a previously emitted [`MetricInfo`] in an
    /// `Info` message.
    pub name: String,
    pub value: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Histogram {
    /// A unique name for this counter.
    ///
    /// This will correspond to a previously emitted [`MetricInfo`] in an
    /// `Info` message.
    pub name: String,
    pub value: histogram::Histogram,
}
