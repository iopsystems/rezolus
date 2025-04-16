use super::*;

#[derive(Default, Clone)]
pub struct Timeseries {
    pub inner: BTreeMap<u64, f64>,
}

impl Timeseries {
    pub fn average(&self) -> f64 {
        if self.inner.is_empty() {
            return 0.0;
        }

        let mut sum = 0.0;
        let mut count = 0;

        for value in self.inner.values() {
            sum += *value;
            count += 1;
        }

        if count > 0 {
            sum / count as f64
        } else {
            0.0
        }
    }

    fn stddev(&self) -> f64 {
        if self.inner.is_empty() {
            return 0.0;
        }

        let values: Vec<f64> = self.inner.values().cloned().collect();

        if values.is_empty() {
            return 0.0;
        }

        let mean = values.iter().sum::<f64>() / values.len() as f64;
        let variance =
            values.iter().map(|x| (*x - mean).powi(2)).sum::<f64>() / values.len() as f64;

        variance.sqrt()
    }

    fn divide_scalar(mut self, divisor: f64) -> Self {
        for value in self.inner.values_mut() {
            *value /= divisor;
        }

        self
    }

    fn divide(mut self, other: &Timeseries) -> Self {
        // remove any times in this series that aren't in other
        let times: Vec<u64> = self.inner.keys().copied().collect();
        for time in times {
            if !other.inner.contains_key(&time) {
                let _ = self.inner.remove(&time);
            }
        }

        // divide all values with matching timestamps, leave nulls
        for (time, divisor) in other.inner.iter() {
            if let Some(v) = self.inner.get_mut(time) {
                *v /= divisor;
            }
        }

        self
    }

    fn multiply_scalar(mut self, multiplier: f64) -> Self {
        for value in self.inner.values_mut() {
            *value *= multiplier;
        }

        self
    }

    fn multiply(mut self, other: &Timeseries) -> Self {
        // remove any times in this series that aren't in other
        let times: Vec<u64> = self.inner.keys().copied().collect();
        for time in times {
            if !other.inner.contains_key(&time) {
                let _ = self.inner.remove(&time);
            }
        }

        // multiply all values with matching timestamps, leave nulls
        for (time, multiplier) in other.inner.iter() {
            if let Some(v) = self.inner.get_mut(time) {
                *v *= multiplier;
            }
        }

        self
    }

    pub fn as_data(&self) -> Vec<Vec<f64>> {
        let mut times = Vec::new();
        let mut values = Vec::new();

        for (time, value) in self.inner.iter() {
            // convert time to unix epoch float seconds
            times.push(*time as f64 / 1000000000.0);
            values.push(*value);
        }

        vec![times, values]
    }
}

impl Add<Timeseries> for Timeseries {
    type Output = Timeseries;

    fn add(self, other: Timeseries) -> Self::Output {
        self.add(&other)
    }
}

impl Add<&Timeseries> for Timeseries {
    type Output = Timeseries;

    fn add(mut self, other: &Timeseries) -> Self::Output {
        // Add values from other TimeSeries where timestamps match
        for (time, value) in other.inner.iter() {
            if let Some(existing) = self.inner.get_mut(time) {
                *existing += value;
            } else {
                // If timestamp doesn't exist in self, add it
                self.inner.insert(*time, *value);
            }
        }

        self
    }
}

impl Div<Timeseries> for Timeseries {
    type Output = Timeseries;
    fn div(self, other: Timeseries) -> <Self as Div<Timeseries>>::Output {
        self.divide(&other)
    }
}

impl Div<&Timeseries> for Timeseries {
    type Output = Timeseries;
    fn div(self, other: &Timeseries) -> <Self as Div<Timeseries>>::Output {
        self.divide(other)
    }
}

impl Div<f64> for Timeseries {
    type Output = Timeseries;
    fn div(self, other: f64) -> <Self as Div<Timeseries>>::Output {
        self.divide_scalar(other)
    }
}

impl Mul<Timeseries> for Timeseries {
    type Output = Timeseries;
    fn mul(self, other: Timeseries) -> <Self as Mul<Timeseries>>::Output {
        self.multiply(&other)
    }
}

impl Mul<&Timeseries> for Timeseries {
    type Output = Timeseries;
    fn mul(self, other: &Timeseries) -> <Self as Mul<Timeseries>>::Output {
        self.multiply(other)
    }
}

impl Mul<f64> for Timeseries {
    type Output = Timeseries;
    fn mul(self, other: f64) -> <Self as Mul<Timeseries>>::Output {
        self.multiply_scalar(other)
    }
}