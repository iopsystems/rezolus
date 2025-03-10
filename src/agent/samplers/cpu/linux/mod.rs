mod cores;
mod frequency;
mod perf;
mod tlb_flush;
mod usage;

/// Returns a vector of logical CPU IDs for CPUs which are present.
pub fn cpus() -> Result<Vec<usize>, Error> {
    let raw =
        std::fs::read_to_string("/sys/devices/system/cpu/present").map(|v| v.trim().to_string())?;

    let mut ids = Vec::new();

    for range in raw.split(',') {
        let mut parts = range.split('-');

        let first: Option<usize> = parts
            .next()
            .map(|text| text.parse())
            .transpose()
            .map_err(|_| Error::other("could not parse"))?;
        let second: Option<usize> = parts
            .next()
            .map(|text| text.parse())
            .transpose()
            .map_err(|_| Error::other("could not parse"))?;

        if parts.next().is_some() {
            // The line is invalid.
            return Err(Error::other("could not parse"));
        }

        match (first, second) {
            (Some(value), None) => ids.push(value),
            (Some(start), Some(stop)) => ids.extend(start..=stop),
            _ => continue,
        }
    }

    Ok(ids)
}
