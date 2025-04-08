use crate::viewer::*;

// get a cpu heatmap series for a metric `sum by(id) (metric{labels})`
pub fn get_cpu_heatmap(data: &Tsdb, metric: &str, labels: impl Into<Labels>) -> Vec<Vec<f64>> {
    let mut heatmap = Vec::new();

    if let Some(collection) = data.get(metric, &labels.into()) {
        for series in collection.sum_by_cpu().iter_mut() {
            let d = series.as_data();

            if heatmap.is_empty() {
                heatmap.push(d[0].clone());
            }

            heatmap.push(d[1].clone());
        }
    }

    heatmap
}

pub fn cpu_usage_heatmap(data: &Tsdb, labels: impl Into<Labels>) -> Vec<Vec<f64>> {
    let mut heatmap = Vec::new();

    for series in data
        .get("cpu_usage", &labels.into())
        .unwrap()
        .sum_by_cpu()
        .drain(..)
    {
        let series = series.divide_scalar(1000000000.0).as_data();

        if heatmap.is_empty() {
            heatmap.push(series[0].clone());
        }

        heatmap.push(series[1].clone());
    }

    heatmap
}

pub fn cpu_ipc_heatmap(data: &Tsdb) -> Vec<Vec<f64>> {
    let mut heatmap = Vec::new();

    let mut cycles = data
        .get("cpu_cycles", &Labels::default())
        .unwrap()
        .sum_by_cpu();
    let mut instructions = data
        .get("cpu_instructions", &Labels::default())
        .unwrap()
        .sum_by_cpu();

    for (c, i) in cycles.drain(..).zip(instructions.drain(..)) {
        let series = i.divide(&c).as_data();

        if heatmap.is_empty() {
            heatmap.push(series[0].clone());
        }

        heatmap.push(series[1].clone());
    }

    heatmap
}

pub fn cpu_frequency_heatmap(data: &Tsdb) -> Vec<Vec<f64>> {
    let mut heatmap = Vec::new();

    let mut aperf = data
        .get("cpu_aperf", &Labels::default())
        .unwrap()
        .sum_by_cpu();
    let mut mperf = data
        .get("cpu_mperf", &Labels::default())
        .unwrap()
        .sum_by_cpu();
    let mut tsc = data
        .get("cpu_tsc", &Labels::default())
        .unwrap()
        .sum_by_cpu();

    for ((a, m), t) in aperf.drain(..).zip(mperf.drain(..)).zip(tsc.drain(..)) {
        let series = t.multiply(&a.divide(&m)).as_data();

        if heatmap.is_empty() {
            heatmap.push(series[0].clone());
        }

        heatmap.push(series[1].clone());
    }

    heatmap
}

pub fn cpu_ipns_heatmap(data: &Tsdb) -> Vec<Vec<f64>> {
    let mut heatmap = Vec::new();

    let mut aperf = data
        .get("cpu_aperf", &Labels::default())
        .unwrap()
        .sum_by_cpu();
    let mut mperf = data
        .get("cpu_mperf", &Labels::default())
        .unwrap()
        .sum_by_cpu();
    let mut tsc = data
        .get("cpu_tsc", &Labels::default())
        .unwrap()
        .sum_by_cpu();
    let mut cycles = data
        .get("cpu_cycles", &Labels::default())
        .unwrap()
        .sum_by_cpu();
    let mut instructions = data
        .get("cpu_instructions", &Labels::default())
        .unwrap()
        .sum_by_cpu();

    for ((((a, m), t), c), i) in aperf
        .drain(..)
        .zip(mperf.drain(..))
        .zip(tsc.drain(..))
        .zip(cycles.drain(..))
        .zip(instructions.drain(..))
    {
        let series = i
            .divide(&c)
            .multiply(&t.multiply(&a.divide(&m)))
            .divide_scalar(1000000000.0)
            .as_data();

        if heatmap.is_empty() {
            heatmap.push(series[0].clone());
        }

        heatmap.push(series[1].clone());
    }

    heatmap
}
