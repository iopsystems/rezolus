use std::collections::{HashMap, HashSet};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::{Duration, SystemTime};

use axum::body::{Body, Bytes};
use futures::Stream;
use http_body::Frame;
use metriken::{RwLockHistogram, Value};
use rezolus_exposition::*;
use tokio::sync::mpsc::error::TrySendError;
use tokio::sync::mpsc::Sender;
use tokio::sync::Mutex;
use tokio::time::{Instant, MissedTickBehavior};

use crate::common::{CounterGroup, GaugeGroup};

#[derive(Default)]
struct Shared {
    lost: u64,
    info: Vec<MetricInfo>,
}

#[derive(serde::Deserialize)]
pub struct SubscribeQueryParams {
    /// The sampling interval, in milliseconds.
    #[serde(default = "default_u64::<1000>")]
    pub interval: u64,
}

const fn default_u64<const C: u64>() -> u64 {
    C
}

pub(super) async fn subscribe(
    query: axum::extract::Query<SubscribeQueryParams>,
) -> axum::body::Body {
    let duration = Duration::from_millis(query.interval).max(Duration::from_millis(100));
    let (tx, mut rx) = tokio::sync::mpsc::channel(1);
    let shared = Arc::new(Mutex::new(Shared::default()));

    let recorder = AbortJoinHandle(tokio::task::spawn(record(shared.clone(), tx, duration)));
    let stream = async_stream::try_stream! {
        let _task = recorder;

        yield Message::Metadata(Metadata {
            metadata: HashMap::from([
                ("source".to_string(), env!("CARGO_BIN_NAME").to_string()),
                ("version".to_string(), env!("CARGO_PKG_VERSION").to_string())
            ])
        });

        loop {
            let snapshot = match rx.recv().await {
                Some(snapshot) => snapshot,
                None => break,
            };

            let (info, lost) = {
                let mut shared = shared.lock().await;
                (
                    std::mem::take(&mut shared.info),
                    std::mem::take(&mut shared.lost)
                )
            };

            if !info.is_empty() {
                yield Message::Info(info);
            }

            if lost != 0 {
                yield Message::Lost(lost);
            }

            yield Message::Snapshot(snapshot);
        }
    };

    Body::new(ExpositionMessageStreamBody::new(stream))
}

/// Return a start time that is aligned to the last multiple of duration.
///
/// Multiple 0 is considered to be aligned to the unix epoch.
fn align_start_to_duration_edge(duration: Duration) -> Instant {
    let now_sys = SystemTime::now();
    let now_ins = Instant::now();

    let unix = now_sys.duration_since(SystemTime::UNIX_EPOCH).unwrap();
    let offset = unix.as_nanos() % duration.as_nanos();
    let offset = Duration::new((offset / 1_000_000_000) as _, (offset % 1_000_000_000) as _);

    now_ins - offset
}

async fn record(shared: Arc<Mutex<Shared>>, tx: Sender<Snapshot>, duration: Duration) {
    let start = align_start_to_duration_edge(duration);
    let mut interval = tokio::time::interval_at(start, duration);
    interval.set_missed_tick_behavior(MissedTickBehavior::Skip);

    let mut known = HashSet::<String>::new();
    let mut prev = start;
    let mut infos = Vec::new();

    loop {
        // First figure out how many samples have been lost, if any.
        let current = interval.tick().await;
        let step = current - prev;
        let lost = step.div_duration_f32(duration) as u64;
        prev = current;

        let mut snapshot = Snapshot {
            timestamp: SystemTime::now(),
            counters: Vec::new(),
            gauges: Vec::new(),
            histograms: Vec::new(),
        };

        'metric: for (metric_id, metric) in metriken::metrics().iter().enumerate() {
            let value = metric.value();
            if value.is_none() {
                continue;
            }

            let name = metric.name();
            if name.starts_with("log_") {
                continue;
            }

            match value {
                Some(Value::Counter(value)) => {
                    let name = metric_id.to_string();

                    if known.insert(name.clone()) {
                        infos.push(MetricInfo {
                            name: name.clone(),
                            ty: MetricType::Counter,
                            metadata: HashMap::from_iter([("metric".to_owned(), name.to_string())]),
                        });
                    }

                    snapshot.counters.push(Counter { name, value })
                }
                Some(Value::Gauge(value)) => {
                    let name = metric_id.to_string();

                    if known.insert(name.clone()) {
                        infos.push(MetricInfo {
                            name: name.clone(),
                            ty: MetricType::Gauge,
                            metadata: HashMap::from_iter([("metric".to_owned(), name.to_string())]),
                        });
                    }

                    snapshot.gauges.push(Gauge { name, value })
                }
                Some(Value::Other(any)) => {
                    if let Some(histogram) = any.downcast_ref::<RwLockHistogram>() {
                        let value = match histogram.load() {
                            Some(value) => value,
                            None => continue 'metric,
                        };
                        let name = metric_id.to_string();

                        if known.insert(name.clone()) {
                            infos.push(MetricInfo {
                                name: name.clone(),
                                ty: MetricType::Histogram,
                                metadata: HashMap::from_iter([
                                    ("metric".to_owned(), name.to_owned()),
                                    (
                                        "grouping_power".to_owned(),
                                        histogram.config().grouping_power().to_string(),
                                    ),
                                    (
                                        "max_value_power".to_owned(),
                                        histogram.config().max_value_power().to_string(),
                                    ),
                                ]),
                            });
                        }

                        snapshot.histograms.push(Histogram { name, value });
                    } else if let Some(counters) = any.downcast_ref::<CounterGroup>() {
                        let c = match counters.load() {
                            Some(counters) => counters,
                            None => continue 'metric,
                        };

                        for (counter_id, counter) in c.iter().enumerate() {
                            if *counter == 0 {
                                continue;
                            }

                            let name = format!("{metric_id}x{counter_id}");
                            if known.insert(name.clone()) {
                                let mut metadata = HashMap::from_iter([
                                    ("metric".to_owned(), name.to_owned()),
                                    ("id".to_owned(), counter_id.to_string()),
                                ]);
                                metadata.extend(
                                    counters.load_metadata(counter_id).into_iter().flatten(),
                                );

                                infos.push(MetricInfo {
                                    name: name.clone(),
                                    ty: MetricType::Counter,
                                    metadata,
                                })
                            }

                            snapshot.counters.push(Counter {
                                name,
                                value: *counter,
                            });
                        }
                    } else if let Some(gauges) = any.downcast_ref::<GaugeGroup>() {
                        let g = match gauges.load() {
                            Some(g) => g,
                            None => continue 'metric,
                        };

                        for (gauge_id, gauge) in g.iter().enumerate() {
                            if *gauge == i64::MIN {
                                continue;
                            }

                            let name = format!("{metric_id}x{gauge_id}");
                            if known.insert(name.clone()) {
                                let mut metadata = HashMap::from_iter([
                                    ("metric".to_owned(), name.to_owned()),
                                    ("id".to_owned(), gauge_id.to_string()),
                                ]);
                                metadata
                                    .extend(gauges.load_metadata(gauge_id).into_iter().flatten());

                                infos.push(MetricInfo {
                                    name: name.clone(),
                                    ty: MetricType::Counter,
                                    metadata,
                                });
                            }

                            snapshot.gauges.push(Gauge {
                                name,
                                value: *gauge,
                            });
                        }
                    }
                }
                _ => (),
            }
        }

        let mut shared = shared.lock().await;
        shared.lost += lost;
        shared.info.append(&mut infos);

        match tx.try_send(snapshot) {
            Ok(()) => (),
            Err(TrySendError::Full(_)) => shared.lost += 1,
            Err(TrySendError::Closed(_)) => break,
        }
    }
}

struct ExpositionMessageStreamBody<S> {
    stream: S,
}

impl<S> ExpositionMessageStreamBody<S>
where
    S: Stream<Item = Result<Message, axum::BoxError>>,
{
    pub fn new(stream: S) -> Self {
        Self { stream }
    }
}

impl<S> ExpositionMessageStreamBody<S> {
    fn project(self: Pin<&mut Self>) -> Pin<&mut S> {
        // SAFETY: We do not move the inner field.
        unsafe { self.map_unchecked_mut(|this| &mut this.stream) }
    }
}

impl<S> axum::body::HttpBody for ExpositionMessageStreamBody<S>
where
    S: Stream<Item = Result<Message, axum::BoxError>>,
{
    type Data = Bytes;
    type Error = axum::BoxError;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let stream = self.project();

        let message = match std::task::ready!(stream.poll_next(cx)) {
            Some(Ok(message)) => message,
            Some(Err(e)) => return Poll::Ready(Some(Err(e))),
            None => return Poll::Ready(None),
        };

        let mut bytes = vec![0u8; 8];
        if let Err(e) = rmp_serde::encode::write(&mut bytes, &message) {
            return Poll::Ready(Some(Err(e.into())));
        };

        let len = bytes.len();
        bytes[..8].copy_from_slice(&u64::to_le_bytes((len - 8) as u64));

        Poll::Ready(Some(Ok(Frame::data(bytes.into()))))
    }
}

struct AbortJoinHandle<T>(tokio::task::JoinHandle<T>);

impl<T> Drop for AbortJoinHandle<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}
