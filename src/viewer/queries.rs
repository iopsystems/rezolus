use crate::viewer::*;

// get a simple metric `sum(metric{labels})`
pub fn get_sum(data: &Tsdb, metric: &str, labels: impl Into<Labels>) -> Vec<Vec<f64>> {
	data.get(metric, &Labels::default()).unwrap().sum().as_data()
}

// get a cpu heatmap series for a metric `sum by(id) (metric{labels})`
pub fn get_cpu_heatmap(data: &Tsdb, metric: &str, labels: impl Into<Labels>) -> Vec<Vec<f64>> {
	let mut heatmap = Vec::new();

    for series in data.get(metric, &labels.into()).unwrap().sum_by_cpu().iter_mut() {
        let d = series.as_data();

        if heatmap.is_empty() {
            heatmap.push(d[0].clone());
        }

        heatmap.push(d[1].clone());
    }

    heatmap
}


pub fn cpu_usage_percent(data: &Tsdb, labels: impl Into<Labels>) -> Vec<Vec<f64>> {
	let cpu_cores = data.get("cpu_cores", &Labels::default()).unwrap().sum();

    let mut cpu_usage = data.get("cpu_usage", &labels.into()).unwrap().sum();
    cpu_usage.divide_scalar(1000000000.0);
    cpu_usage.divide(&cpu_cores);

    cpu_usage.as_data()
}


pub fn cpu_usage_heatmap(data: &Tsdb, labels: impl Into<Labels>) -> Vec<Vec<f64>> {
	let mut heatmap = Vec::new();

    for series in data.get("cpu_usage", &labels.into()).unwrap().sum_by_cpu().iter_mut() {
        series.divide_scalar(1000000000.0);
        let d = series.as_data();

        if heatmap.is_empty() {
            heatmap.push(d[0].clone());
        }

        heatmap.push(d[1].clone());
    }

    heatmap
}

pub fn cpu_ipc(data: &Tsdb) -> Vec<Vec<f64>> {
	let cycles = data.get("cpu_cycles", &Labels::default()).unwrap().sum();

    let mut instructions = data.get("cpu_instructions", &Labels::default()).unwrap().sum();
    instructions.divide(&cycles);

    instructions.as_data()
}

pub fn cpu_ipc_heatmap(data: &Tsdb) -> Vec<Vec<f64>> {
	let mut heatmap = Vec::new();

	let cycles = data.get("cpu_cycles", &Labels::default()).unwrap().sum_by_cpu();
	let mut instructions = data.get("cpu_instructions", &Labels::default()).unwrap().sum_by_cpu();

    for (c, i) in cycles.iter().zip(instructions.iter_mut()) {
        i.divide(c);
        let d = i.as_data();

        if heatmap.is_empty() {
            heatmap.push(d[0].clone());
        }

        heatmap.push(d[1].clone());
    }

    heatmap
}