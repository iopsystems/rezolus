//! Help text is an interface, so hold it to an interface's standard.
//!
//! Rezolus is increasingly driven by agents that read `--help` and act on it.
//! For them a stale help string is not cosmetic: it is a wrong tool call, or a
//! refusal to attempt something that works. This repo has now shipped that bug
//! three separate times — `record --rez-version` (removed, still advertised),
//! `record --node`/`--instance` (advertised, never implemented), and three
//! `recording` subcommands claiming v3 `.rez` "errors clearly" long after v3
//! worked.
//!
//! The two checks here are the mechanical half of that problem — the half a
//! machine can settle. Both would have caught the flag cases above the day they
//! landed:
//!
//! 1. every flag a help text *mentions* is a flag that command actually defines
//! 2. every example invocation *in* a help text uses only real flags
//!
//! What they cannot check is whether a true-looking sentence is true — that
//! `.rez` output refuses an existing path, that a wrapped command's exit status
//! passes through. Those need someone to read the code, and the
//! `document-feature` skill exists for that. These tests just make sure the
//! cheap, decidable failures never reach a user.

use std::collections::BTreeSet;
use std::process::Command;

/// Every subcommand path in the tree, discovered rather than hardcoded so a
/// new one is covered the day it is added.
fn command_paths() -> Vec<Vec<String>> {
    let mut paths = vec![vec![]];
    for top in subcommands_of(&[]) {
        let path = vec![top];
        for sub in subcommands_of(&path) {
            let mut p = path.clone();
            p.push(sub);
            paths.push(p);
        }
        paths.push(path);
    }
    paths
}

fn bin() -> String {
    env!("CARGO_BIN_EXE_rezolus").to_string()
}

fn help_for(path: &[String]) -> String {
    let out = Command::new(bin())
        .args(path)
        .arg("--help")
        .output()
        .unwrap_or_else(|e| panic!("running `rezolus {} --help`: {e}", path.join(" ")));
    // clap prints long help to stdout; a parse failure would land on stderr.
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// The `Commands:` block lists subcommands one per line, name first.
fn subcommands_of(path: &[String]) -> Vec<String> {
    let help = help_for(path);
    let Some(rest) = help.split_once("\nCommands:\n") else {
        return Vec::new();
    };
    rest.1
        .lines()
        .take_while(|l| !l.trim().is_empty())
        .filter_map(|l| l.split_whitespace().next())
        .filter(|n| *n != "help")
        .map(|n| n.to_string())
        .collect()
}

/// Flags clap registered, read back off its own `Options:`/`Arguments:` layout:
/// a definition starts a line, indented, as `--flag` or `-f, --flag`.
fn defined_flags(help: &str) -> BTreeSet<String> {
    let mut flags = BTreeSet::new();
    for line in help.lines() {
        let indent = line.len() - line.trim_start().len();
        if !(2..=6).contains(&indent) {
            continue;
        }
        let t = line.trim_start();
        // `-f, --flag`
        if let Some(rest) = t
            .strip_prefix('-')
            .filter(|r| r.len() > 2 && r.as_bytes()[1] == b',')
        {
            flags.insert(format!("-{}", &rest[..1]));
            if let Some(long) = rest[2..].split_whitespace().next() {
                // clap suffixes a repeatable flag with `...`
                flags.insert(long.trim_end_matches('.').to_string());
            }
            continue;
        }
        if t.starts_with("--") {
            if let Some(long) = t.split_whitespace().next() {
                flags.insert(long.trim_end_matches([',', '.']).to_string());
            }
        }
    }
    // Present on every clap command whether or not it lists them here.
    for universal in ["-h", "--help", "-V", "--version"] {
        flags.insert(universal.to_string());
    }
    flags
}

/// Long flags a help text refers to in prose or examples.
fn mentioned_flags(help: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let bytes = help.as_bytes();
    let mut i = 0;
    while i + 2 < bytes.len() {
        if bytes[i] == b'-' && bytes[i + 1] == b'-' && bytes[i + 2].is_ascii_alphanumeric() {
            // not mid-word (e.g. `foo--bar`) and not the bare `--` separator
            let prev_ok = i == 0 || !(bytes[i - 1] as char).is_ascii_alphanumeric();
            if prev_ok {
                let start = i;
                i += 2;
                while i < bytes.len()
                    && ((bytes[i] as char).is_ascii_alphanumeric() || bytes[i] == b'-')
                {
                    i += 1;
                }
                let flag = help[start..i].trim_end_matches('-');
                if flag.len() > 2 {
                    out.insert(flag.to_string());
                }
                continue;
            }
        }
        i += 1;
    }
    out
}

/// Flags a help text may name without defining them.
///
/// Two legitimate reasons exist, and both are load-bearing rather than
/// loopholes: a page points at a *sibling* command's flag ("set with
/// `annotate --source`"), or it names a flag precisely to say it does NOT
/// exist ("there is no --force"). Anything else is the bug this test is for.
fn is_cross_reference(help: &str, flag: &str, own: &BTreeSet<String>) -> bool {
    if own.contains(flag) {
        return true;
    }
    // A family named collectively: `--proxy-*` covers --proxy-allow and
    // --proxy-allow-any. The trailing `-` survives the wildcard being stripped.
    if help.contains(&format!("{flag}-*")) && own.iter().any(|f| f.starts_with(flag)) {
        return true;
    }
    // Flags belonging to some other command in the tree, or to a user's own
    // wrapped command (`-- ./bench.sh --iters 100`).
    for line in help.lines() {
        if !line.contains(flag) {
            continue;
        }
        let l = line.trim();
        let names_another_command = l.contains("rezolus ") || l.contains('`');
        let denies_it = l.contains("no ") || l.contains("removed") || l.contains("were never");
        if !(names_another_command || denies_it) {
            return false;
        }
    }
    true
}

#[test]
fn every_flag_a_help_text_mentions_is_a_flag_that_command_defines() {
    let mut problems = Vec::new();
    for path in command_paths() {
        let help = help_for(&path);
        let defined = defined_flags(&help);
        for flag in mentioned_flags(&help) {
            if !is_cross_reference(&help, &flag, &defined) {
                problems.push(format!(
                    "`rezolus {} --help` refers to {flag}, which it does not define",
                    path.join(" ")
                ));
            }
        }
    }
    assert!(problems.is_empty(), "\n{}", problems.join("\n"));
}

#[test]
fn every_example_invocation_uses_only_flags_its_command_defines() {
    let tops: BTreeSet<String> = subcommands_of(&[]).into_iter().collect();
    let mut problems = Vec::new();
    let mut examples = 0usize;

    for path in command_paths() {
        let help = help_for(&path);
        for line in help.lines() {
            let line = line.trim();
            let Some(rest) = line.strip_prefix("rezolus ") else {
                continue;
            };
            // Stop at a shell operator: what follows is a different command.
            let rest = rest
                .split(" && ")
                .next()
                .unwrap()
                .split(" | ")
                .next()
                .unwrap()
                .split(" > ")
                .next()
                .unwrap();
            let toks: Vec<&str> = rest.split_whitespace().collect();
            if toks.is_empty() || !tops.contains(toks[0]) {
                continue; // `rezolus config/agent.toml`, the agent form
            }
            let mut cmd = vec![toks[0].to_string()];
            let mut i = 1;
            if let Some(second) = toks.get(1) {
                if !second.starts_with('-') && subcommands_of(&cmd).iter().any(|s| s == second) {
                    cmd.push(second.to_string());
                    i = 2;
                }
            }
            examples += 1;
            let defined = defined_flags(&help_for(&cmd));
            for tok in &toks[i..] {
                if *tok == "--" {
                    break; // everything after is the user's wrapped command
                }
                if !tok.starts_with('-') || *tok == "-" {
                    continue;
                }
                let name = tok.split('=').next().unwrap();
                if !defined.contains(name) {
                    problems.push(format!(
                        "example in `rezolus {} --help` passes {name} to `rezolus {}`, \
                         which does not define it\n    {line}",
                        path.join(" "),
                        cmd.join(" ")
                    ));
                }
            }
        }
    }

    assert!(
        examples > 20,
        "only found {examples} examples — parser broke"
    );
    assert!(problems.is_empty(), "\n{}", problems.join("\n"));
}
