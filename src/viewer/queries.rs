use crate::viewer::*;


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