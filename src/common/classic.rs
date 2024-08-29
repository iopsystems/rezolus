#![allow(dead_code)]

use tokio::io::AsyncSeekExt;
use tokio::io::AsyncReadExt;
use std::collections::HashMap;
use tokio::fs::File;
use std::io::{Error, ErrorKind};
use std::iter::zip;

/// A type that wraps values associated with a nested set of string keys. It is
/// intended to be constructed by parsing from a file with a specific format and
/// allows the caller to access the values in a more straightforward way and
/// enables reuse of the parsing logic when files have a common format.
pub struct NestedMap {
    inner: HashMap<String, HashMap<String, u64>>,
}

impl NestedMap {
    /// Returns the value for the given pkey and lkey if one exists.
    pub fn get(&self, pkey: &str, lkey: &str) -> Option<u64> {
        self.inner.get(pkey)?.get(lkey).copied()
    }

    /// Tries to create a new NestedMap from a file that would be found in procfs
    /// such as `/proc/net/snmp` with the following format:
    /// ```plain
    /// pkey1 lkey1 ... lkeyN
    /// pkey1 value1 ... valueN
    /// ...
    /// pkeyN lkey1 ... lkeyN
    /// pkeyN value1 ... lkeyN
    /// ```
    pub async fn try_from_procfs(file: &mut File) -> Result<Self, std::io::Error> {
        // seek to start to cause reload of content
        file.rewind().await?;

        let mut data = String::new();
        file.read_to_string(&mut data).await?;

        let mut inner = HashMap::new();

        let mut lines = data.lines();

        loop {
            let k_line = lines.next();
            if k_line.is_none() {
                break;
            }

            let v_line = lines.next();
            if v_line.is_none() {
                break;
            }

            let keys: Vec<&str> = k_line.unwrap().split_whitespace().collect();
            let values: Vec<&str> = v_line.unwrap().split_whitespace().collect();

            if keys.is_empty() || values.is_empty() {
                continue;
            }

            if keys[0] != values[0] {
                println!(
                    "pkey mismatch parsing nested map: {} != {}",
                    keys[0], values[0]
                );
                return Err(Error::new(ErrorKind::InvalidData, "pkey mismatch"));
            }

            let mut map = HashMap::with_capacity(keys.len() - 1);
            for (key, value) in zip(
                keys.iter().skip(1).map(|k| k.to_owned()),
                values.iter().skip(1),
            ) {
                if let Ok(value) = value.parse::<u64>() {
                    map.insert(key.to_owned(), value);
                }
            }

            inner.insert(keys[0].to_owned(), map);
        }

        Ok(Self { inner })
    }
}
