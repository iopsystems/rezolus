use crate::PERCENTILES;
use warp::Filter;
use metriken::{Counter, Gauge, Heatmap};

pub async fn http() {
    let root = warp::path::end().map(|| "rezolus");

    let vars = warp::path("vars").map(human_stats);

    let metrics = warp::path("metrics").map(prometheus_stats);

    let routes = warp::get().and(
        root
            .or(vars)
            .or(metrics),
    );

    warp::serve(routes).run(([0, 0, 0, 0], 4242)).await;

}

fn prometheus_stats() -> String {
    let mut data = Vec::new();

    for metric in &metriken::metrics() {
        let any = match metric.as_any() {
            Some(any) => any,
            None => {
                continue;
            }
        };

        if metric.name().starts_with("log_") {
            continue;
        }

        if let Some(counter) = any.downcast_ref::<Counter>() {
            data.push(format!("# TYPE {} counter\n{} {}", metric.name(), metric.name(), counter.value()));
        } else if let Some(gauge) = any.downcast_ref::<Gauge>() {
            data.push(format!("# TYPE {} gauge\n{} {}", metric.name(), metric.name(), gauge.value()));
        } else if let Some(heatmap) = any.downcast_ref::<Heatmap>() {
            for (_label, percentile) in PERCENTILES {
                let value = heatmap.percentile(*percentile).map(|b| b.high()).unwrap_or(0);
                data.push(format!("# TYPE {} gauge\n{}{{percentile=\"{:02}\"}} {}", metric.name(), metric.name(), percentile, value));
            }
        }
    }

    data.sort();
    let mut content = data.join("\n");
    content += "\n";
    let parts: Vec<&str> = content.split('/').collect();
    parts.join("_")
}

fn human_stats() -> String {
    let mut data = Vec::new();

    for metric in &metriken::metrics() {
        let any = match metric.as_any() {
            Some(any) => any,
            None => {
                continue;
            }
        };

        if metric.name().starts_with("log_") {
            continue;
        }

        if let Some(counter) = any.downcast_ref::<Counter>() {
            data.push(format!("{}: {}", metric.name(), counter.value()));
        } else if let Some(gauge) = any.downcast_ref::<Gauge>() {
            data.push(format!("{}: {}", metric.name(), gauge.value()));
        } else if let Some(heatmap) = any.downcast_ref::<Heatmap>() {
            for (label, p) in PERCENTILES {
                let percentile = heatmap.percentile(*p).map(|b| b.high()).unwrap_or(0);
                data.push(format!("{}/{}: {}", metric.name(), label, percentile));
            }
        }
    }

    data.sort();
    let mut content = data.join("\n");
    content += "\n";
    content
}