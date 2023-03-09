use crate::*;

pub struct Noop { }

impl Noop {
	pub fn new(_config: &Config) -> Self {
		Self { }
	}
}

impl Sampler for Noop {
	fn sample(&mut self) { }
}

impl Display for Noop {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::result::Result<(), std::fmt::Error> {
		write!(f, "noop")
	}
}