use metriken::Heatmap;
use metriken::Lazy;
use metriken::Counter;

type Duration = clocksource::Duration<clocksource::Nanoseconds<u64>>;

#[metric(name = "tcp/receive/segments", description = "number of TCP segments received")]
pub static TCP_RX_SEGS: Lazy<Counter> = Lazy::new(|| {
    Counter::new()
});

#[metric(name = "tcp/receive/segments", description = "number of TCP segments received")]
pub static TCP_RX_SEGS_HIST: Lazy<Heatmap> = Lazy::new(|| {
    Heatmap::new(0, 4, 64, Duration::from_secs(60), Duration::from_secs(1)).unwrap()
});
