pub use super::*;

#[derive(Serialize)]
pub struct Memory {
    total_bytes: u64,
}

impl Memory {
    pub fn new() -> Result<Self> {
        let file = File::open("/proc/meminfo")?;
        let reader = BufReader::new(file);

        let mut ret = Self { total_bytes: 0 };

        for line in reader.lines() {
            let line = line.unwrap();
            if line.starts_with("MemTotal:") {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() == 3 {
                    ret.total_bytes = parts[1].parse::<u64>().map(|v| v * 1024).map_err(|_| {
                        std::io::Error::new(std::io::ErrorKind::InvalidData, "bad value")
                    })?;
                }
            }
        }

        Ok(ret)
    }

    pub fn node(id: usize) -> Result<Self> {
        let file = File::open(format!("/sys/devices/system/node/node{id}/meminfo"))?;
        let reader = BufReader::new(file);

        let mut ret = Self { total_bytes: 0 };

        for line in reader.lines() {
            let line = line.unwrap();
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 4 && parts[2] == "MemTotal:" {
                ret.total_bytes = parts[3].parse::<u64>().map(|v| v * 1024).map_err(|_| {
                    std::io::Error::new(std::io::ErrorKind::InvalidData, "bad value")
                })?;
            }
        }

        Ok(ret)
    }
}
