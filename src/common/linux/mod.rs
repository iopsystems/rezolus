use walkdir::{DirEntry, WalkDir};

use std::io::Error;

pub fn cpus() -> Result<Vec<usize>, Error> {
    let raw = std::fs::read_to_string("/sys/devices/system/cpu/possible")
        .map(|v| v.trim().to_string())?;

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

pub(crate) fn is_hidden(entry: &DirEntry) -> bool {
    entry
        .file_name()
        .to_str()
        .map(|s| s.starts_with('.'))
        .unwrap_or(false)
}

pub fn network_interfaces() -> Result<Vec<String>, Error> {
    let mut interfaces = Vec::new();

    let walker = WalkDir::new("/sys/class/net/")
        .follow_links(true)
        .max_depth(1)
        .into_iter();

    for entry in walker.filter_entry(|e| !is_hidden(e)) {
        if entry.is_err() {
            continue;
        }
        let entry = entry.unwrap();
        if entry.file_type().is_dir() {
            if let Some(name) = entry.file_name().to_str() {
                let driver: Option<String> =
                    match std::fs::read_link(format!("/sys/class/net/{name}/device/driver/module"))
                    {
                        Ok(driver_link) => Some(
                            driver_link
                                .file_name()
                                .unwrap()
                                .to_string_lossy()
                                .to_string(),
                        ),
                        Err(_) => None,
                    };

                if driver.is_some() {
                    interfaces.push(name.to_string());
                }
            }
        }
    }

    Ok(interfaces)
}
