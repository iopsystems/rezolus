use super::*;

#[derive(Default, Eq, PartialEq, Hash, Clone, Debug)]
pub struct Labels {
    pub inner: BTreeMap<String, String>,
}

impl Labels {
    pub fn matches(&self, other: &Labels) -> bool {
        for (label, value) in other.inner.iter() {
            // Check if it's a negative match pattern
            if let Some(pattern) = value.strip_prefix('!') {
                // Remove the '!' prefix

                // For negative patterns, check if label exists and DOESN'T match
                if let Some(v) = self.inner.get(label) {
                    // Check if the value matches the negative pattern
                    if pattern.contains('|') {
                        // Simple alternation pattern - check if v matches any of the options
                        let inner_pattern = if pattern.starts_with('(') && pattern.ends_with(')') {
                            &pattern[1..pattern.len() - 1]
                        } else {
                            pattern
                        };

                        // Split on | and check if v matches any option
                        let matches_any = inner_pattern.split('|').any(|option| {
                            // Handle escaped dots in the pattern
                            if option.contains("\\.") {
                                // Replace \. with . for literal matching
                                let unescaped = option.replace("\\.", ".");
                                v == &unescaped
                            } else {
                                v == option
                            }
                        });
                        if matches_any {
                            // If it matches the negative pattern, exclude this series
                            return false;
                        }
                    } else {
                        // Simple negative match - might have escaped dots
                        let matches = if pattern.contains("\\.") {
                            // Replace \. with . for literal matching
                            let unescaped = pattern.replace("\\.", ".");
                            v == &unescaped
                        } else {
                            v == pattern
                        };
                        if matches {
                            return false;
                        }
                    }
                }
                // If label doesn't exist, it passes the negative filter
            } else if let Some(v) = self.inner.get(label) {
                // Regular positive match
                // Check if the value looks like a simple regex alternation pattern
                // Format: (option1|option2|option3) or option1|option2|option3
                if value.contains('|') {
                    // Simple alternation pattern - check if v matches any of the options
                    let pattern = if value.starts_with('(') && value.ends_with(')') {
                        &value[1..value.len() - 1]
                    } else {
                        value
                    };

                    // Split on | and check if v matches any option
                    let matches_any = pattern.split('|').any(|option| {
                        // Handle escaped dots in the pattern
                        if option.contains("\\.") {
                            // Replace \. with . for literal matching
                            let unescaped = option.replace("\\.", ".");
                            v == &unescaped
                        } else {
                            v == option
                        }
                    });
                    if !matches_any {
                        return false;
                    }
                } else {
                    // Single value match - might have escaped dots
                    let matches = if value.contains("\\.") {
                        // Replace \. with . for literal matching
                        let unescaped = value.replace("\\.", ".");
                        v == &unescaped
                    } else {
                        v == value
                    };
                    if !matches {
                        return false;
                    }
                }
            } else {
                // Label doesn't exist but was required (positive match)
                return false;
            }
        }

        true
    }
}

impl From<&[(&str, &str)]> for Labels {
    fn from(other: &[(&str, &str)]) -> Self {
        Labels {
            inner: other
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }
    }
}

impl From<()> for Labels {
    fn from(_other: ()) -> Self {
        Labels::default()
    }
}

impl<const N: usize> From<[(&str, &str); N]> for Labels {
    fn from(other: [(&str, &str); N]) -> Self {
        Labels {
            inner: other
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }
    }
}

impl<const N: usize> From<[(String, String); N]> for Labels {
    fn from(other: [(String, String); N]) -> Self {
        Labels {
            inner: other.iter().cloned().collect(),
        }
    }
}

impl<const N: usize> From<[(&str, String); N]> for Labels {
    fn from(other: [(&str, String); N]) -> Self {
        Labels {
            inner: other
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone()))
                .collect(),
        }
    }
}

impl From<&mut dyn Iterator<Item = (&str, &str)>> for Labels {
    fn from(other: &mut dyn Iterator<Item = (&str, &str)>) -> Self {
        Self {
            inner: other.map(|(k, v)| (k.to_string(), v.to_string())).collect(),
        }
    }
}
