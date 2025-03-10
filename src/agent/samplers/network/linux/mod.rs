mod sysfs;

mod interfaces;
mod traffic;

/// Helper function to filter hidden folders while walking directories.
pub(crate) fn is_hidden(entry: &DirEntry) -> bool {
    entry
        .file_name()
        .to_str()
        .map(|s| s.starts_with('.'))
        .unwrap_or(false)
}

/// Returns a list of network interface names.
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
