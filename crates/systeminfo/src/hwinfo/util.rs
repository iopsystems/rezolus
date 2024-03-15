use std::ops::{Deref, DerefMut};
use std::path::Path;
use std::str::FromStr;

use walkdir::{DirEntry, WalkDir};

use crate::{Error, Result};

pub(crate) fn read_usize(path: impl AsRef<Path>) -> Result<usize> {
    let path = path.as_ref();

    let raw = std::fs::read_to_string(path).map_err(|e| Error::unreadable(e, path))?;
    let raw = raw.trim();

    raw.parse().map_err(|e| Error::unparseable(e, path))
}

pub(crate) fn read_string(path: impl AsRef<Path>) -> Result<String> {
    let path = path.as_ref();

    let raw = std::fs::read_to_string(path).map_err(|e| Error::unreadable(e, path))?;
    let raw = raw.trim();

    Ok(raw.to_string())
}

pub(crate) fn read_list(path: impl AsRef<Path>) -> Result<Vec<usize>> {
    let path = path.as_ref();
    let raw = std::fs::read_to_string(path).map_err(|e| Error::unreadable(e, path))?;
    parse_list(&raw, path)
}

fn parse_list(raw: &str, path: &Path) -> Result<Vec<usize>> {
    let raw = raw.trim();
    let mut ret = Vec::new();

    for range in raw.split(',') {
        let mut parts = range.split('-');

        let first: Option<usize> = parts
            .next()
            .map(|text| text.parse())
            .transpose()
            .map_err(|e| Error::unparseable(e, path))?;
        let second: Option<usize> = parts
            .next()
            .map(|text| text.parse())
            .transpose()
            .map_err(|e| Error::unparseable(e, path))?;

        if parts.next().is_some() {
            // The line is invalid, skip it.
            continue;
        }

        match (first, second) {
            (Some(value), None) => ret.push(value),
            (Some(start), Some(stop)) => ret.extend(start..=stop),
            _ => continue,
        }
    }

    Ok(ret)
}

pub(crate) fn read_hexbitmap(path: impl AsRef<Path>) -> Vec<usize> {
    let mut ret: Vec<usize> = Vec::new();
    if let Ok(hex_string) = read_string(path) {
        hex_string.chars().rfold(0, |acc, c: char| {
            let val = c.to_digit(16).unwrap_or(0);
            for i in 0..4 {
                if (val & (0x1 << i)) > 0 {
                    ret.push(acc + i);
                }
            }
            acc + 4
        });
    }
    ret
}

// Return a list of IRQs
pub(crate) fn read_irqs(path: impl AsRef<Path>) -> Vec<usize> {
    let walker = WalkDir::new(path).max_depth(1);
    return walker
        .into_iter()
        .filter_map(|entry| {
            if let Ok(irq) = entry {
                irq.file_name().to_str().unwrap().parse::<usize>().ok()
            } else {
                None
            }
        })
        .collect();
}

pub(crate) fn read_space_list<T: FromStr>(path: impl AsRef<Path>) -> Result<Vec<T>> {
    Ok(read_string(path)?
        .split_whitespace()
        .filter_map(|x| x.parse::<T>().ok())
        .collect())
}

pub(crate) fn is_hidden(entry: &DirEntry) -> bool {
    entry
        .file_name()
        .to_str()
        .map(|s| s.starts_with('.'))
        .unwrap_or(false)
}

/// Guard which clears the contained string upon drop.
pub(crate) struct ClearGuard<'a>(&'a mut String);

impl<'a> ClearGuard<'a> {
    pub fn new(value: &'a mut String) -> Self {
        Self(value)
    }
}

impl<'a> Drop for ClearGuard<'a> {
    fn drop(&mut self) {
        self.0.clear();
    }
}

impl<'a> Deref for ClearGuard<'a> {
    type Target = String;

    fn deref(&self) -> &Self::Target {
        self.0
    }
}

impl<'a> DerefMut for ClearGuard<'a> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut *self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_parsing() {
        let list = "0-1\r\n";
        assert_eq!(parse_list(list, "/test/case".as_ref()).unwrap(), vec![0, 1]);
    }
}
