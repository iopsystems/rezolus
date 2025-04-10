use metriken::*;

use crate::agent::*;

#[metric(
    name = "softnet_time_squeezed",
    description = "Number of times net_rx_action() exits due to time constraints (time squeeze)",
    metadata = { unit = "events" }
)]
pub static SOFTNET_TIME_SQUEEZED: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "softnet_budget_exhausted",
    description = "Number of times net_rx_action() exits due to budget exhaustion",
    metadata = { unit = "events" }
)]
pub static SOFTNET_BUDGET_EXHAUSTED: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "softnet_processed",
    description = "Number of packets processed during net_rx_action()",
    metadata = { unit = "packets" }
)]
pub static SOFTNET_PROCESSED: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "softnet_poll",
    description = "Number of NAPI poll function calls made during packet processing",
    metadata = { unit = "calls" }
)]
pub static SOFTNET_POLL: CounterGroup = CounterGroup::new(MAX_CPUS);
