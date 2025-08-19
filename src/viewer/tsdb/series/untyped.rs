use super::*;

#[derive(Default, Clone)]
pub struct UntypedSeries {
    pub inner: BTreeMap<u64, f64>,
}

impl UntypedSeries {
    fn divide_scalar(mut self, divisor: f64) -> Self {
        for value in self.inner.values_mut() {
            *value /= divisor;
        }

        self
    }

    fn divide(mut self, other: &UntypedSeries) -> Self {
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

    fn multiply(mut self, other: &UntypedSeries) -> Self {
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

impl Add<UntypedSeries> for UntypedSeries {
    type Output = UntypedSeries;

    fn add(self, other: UntypedSeries) -> Self::Output {
        self.add(&other)
    }
}

impl Add<&UntypedSeries> for UntypedSeries {
    type Output = UntypedSeries;

    fn add(mut self, other: &UntypedSeries) -> Self::Output {
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

impl Div<UntypedSeries> for UntypedSeries {
    type Output = UntypedSeries;
    fn div(self, other: UntypedSeries) -> <Self as Div<UntypedSeries>>::Output {
        self.divide(&other)
    }
}

impl Div<&UntypedSeries> for UntypedSeries {
    type Output = UntypedSeries;
    fn div(self, other: &UntypedSeries) -> <Self as Div<UntypedSeries>>::Output {
        self.divide(other)
    }
}

impl Div<f64> for UntypedSeries {
    type Output = UntypedSeries;
    fn div(self, other: f64) -> <Self as Div<UntypedSeries>>::Output {
        self.divide_scalar(other)
    }
}

impl Mul<UntypedSeries> for UntypedSeries {
    type Output = UntypedSeries;
    fn mul(self, other: UntypedSeries) -> <Self as Mul<UntypedSeries>>::Output {
        self.multiply(&other)
    }
}

impl Mul<&UntypedSeries> for UntypedSeries {
    type Output = UntypedSeries;
    fn mul(self, other: &UntypedSeries) -> <Self as Mul<UntypedSeries>>::Output {
        self.multiply(other)
    }
}

impl Mul<f64> for UntypedSeries {
    type Output = UntypedSeries;
    fn mul(self, other: f64) -> <Self as Mul<UntypedSeries>>::Output {
        self.multiply_scalar(other)
    }
}
