//! `rezolus mcp install`: register the stdio server with a client and
//! install the skill that tells the client how to use it.
//!
//! The one client known today is Claude Code. Registration goes through its
//! own CLI (`claude mcp add`) when that is on `PATH`, since its user-scope
//! configuration file is the client's own state and not a file this binary
//! should edit. Without the CLI, project scope is still possible because the
//! project file (`.mcp.json` in the working directory) has a documented,
//! small shape that is merged into rather than replaced; user scope then
//! prints the command to run.
//!
//! The skill is `SKILL.md`, embedded at build time from `src/mcp/skill/`,
//! written under `~/.claude/skills/rezolus-mcp/` (user scope; under
//! `$CLAUDE_CONFIG_DIR/skills/` when that is set, since that is where the
//! client looks) or `.claude/skills/rezolus-mcp/` (project scope). A file already there is
//! replaced only when it is ours (its frontmatter names the skill); anything
//! else is refused rather than overwritten.

use std::path::{Path, PathBuf};
use std::process::Command;

pub(crate) const SERVER_NAME: &str = "rezolus";
pub(crate) const SKILL_NAME: &str = "rezolus-mcp";
pub(crate) const SKILL_MD: &str = include_str!("skill/SKILL.md");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Scope {
    User,
    Project,
}

impl Scope {
    pub(crate) fn parse(s: &str) -> Result<Self, String> {
        match s {
            "user" => Ok(Scope::User),
            "project" => Ok(Scope::Project),
            other => Err(format!(
                "scope must be \"user\" or \"project\", not {other:?}"
            )),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Scope::User => "user",
            Scope::Project => "project",
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct InstallOptions {
    pub scope: Scope,
    /// Server flags to register the server with, so the registered command
    /// starts a server with the operator's tiers and directories.
    pub server: super::server::ServerOptions,
    /// Install the skill (default) or only register the server.
    pub skill: bool,
    /// Report what would be done and touch nothing.
    pub dry_run: bool,
    /// The binary to register; `current_exe` unless overridden.
    pub binary: Option<PathBuf>,
    /// Where project-scope files go; the working directory unless given.
    pub project_dir: Option<PathBuf>,
    /// The user's home, for the user-scope skill; `$HOME` unless given.
    pub home: Option<PathBuf>,
    /// Where the `claude` CLI is: `None` looks on `PATH`, `Some(None)` says
    /// there is none (tests), `Some(Some(p))` names it.
    pub cli: Option<Option<PathBuf>>,
}

/// The `args` the registered command runs with: `mcp` plus the server
/// flags in a fixed order.
pub(crate) fn server_args(server: &super::server::ServerOptions) -> Vec<String> {
    let mut args = vec!["mcp".to_string()];
    if server.allow_mutating {
        args.push("--allow-mutating".to_string());
    }
    if let Some(dir) = &server.export_dir {
        args.push("--export-dir".to_string());
        args.push(dir.display().to_string());
    }
    if let Some(url) = &server.viewer_url {
        args.push("--viewer-url".to_string());
        args.push(url.clone());
    }
    args
}

/// Merge the server entry into a project `.mcp.json` payload: other servers
/// stay, an existing `rezolus` entry is replaced, and a file that is not a
/// JSON object is refused rather than overwritten.
pub(crate) fn merge_project_config(
    existing: Option<&str>,
    binary: &Path,
    args: &[String],
) -> Result<String, String> {
    let mut root: serde_json::Value = match existing {
        None => serde_json::json!({}),
        Some(text) if text.trim().is_empty() => serde_json::json!({}),
        Some(text) => serde_json::from_str(text)
            .map_err(|e| format!(".mcp.json is not valid JSON, refusing to overwrite it: {e}"))?,
    };
    let obj = root
        .as_object_mut()
        .ok_or(".mcp.json is not a JSON object, refusing to overwrite it")?;
    let servers = obj
        .entry("mcpServers")
        .or_insert_with(|| serde_json::json!({}));
    let servers = servers
        .as_object_mut()
        .ok_or(".mcp.json has a non-object \"mcpServers\", refusing to overwrite it")?;
    servers.insert(
        SERVER_NAME.to_string(),
        serde_json::json!({
            "command": binary.display().to_string(),
            "args": args,
        }),
    );
    serde_json::to_string_pretty(&root)
        .map(|s| s + "\n")
        .map_err(|e| e.to_string())
}

/// Whether a SKILL.md at the target is ours to replace.
pub(crate) fn is_our_skill(text: &str) -> bool {
    // Frontmatter only: `---`, then `name: rezolus-mcp` before the closing
    // `---`. A mention of the name in a body is not a claim of ownership.
    let mut lines = text.lines();
    if lines.next().map(str::trim) != Some("---") {
        return false;
    }
    lines
        .take_while(|l| l.trim() != "---")
        .any(|l| l.trim() == format!("name: {SKILL_NAME}"))
}

fn skill_dir(opts: &InstallOptions) -> Result<PathBuf, String> {
    match opts.scope {
        Scope::User => {
            // The client's config directory: `CLAUDE_CONFIG_DIR` when set
            // (the CLI resolves its own files from it, and a `HOME` override
            // does not move them), else `~/.claude`.
            let config_dir = match &opts.home {
                Some(h) => h.join(".claude"),
                None => match std::env::var_os("CLAUDE_CONFIG_DIR") {
                    Some(d) if !d.is_empty() => PathBuf::from(d),
                    _ => std::env::var_os("HOME")
                        .map(|h| PathBuf::from(h).join(".claude"))
                        .ok_or("HOME is not set; pass a scope of \"project\" or set HOME")?,
                },
            };
            Ok(config_dir.join("skills").join(SKILL_NAME))
        }
        Scope::Project => Ok(project_dir(opts)?
            .join(".claude")
            .join("skills")
            .join(SKILL_NAME)),
    }
}

fn project_dir(opts: &InstallOptions) -> Result<PathBuf, String> {
    match &opts.project_dir {
        Some(d) => Ok(d.clone()),
        None => std::env::current_dir().map_err(|e| format!("current directory: {e}")),
    }
}

/// Write (or, in a dry run, describe) the skill. Returns the report line.
pub(crate) fn install_skill(opts: &InstallOptions) -> Result<String, String> {
    let dir = skill_dir(opts)?;
    let path = dir.join("SKILL.md");
    // A symlink is refused, dangling or not: every read and write below
    // would follow it, and the file it names (a checkout's copy of this
    // skill, say) is not ours to replace.
    if let Ok(meta) = std::fs::symlink_metadata(&path) {
        if meta.file_type().is_symlink() {
            let target = std::fs::read_link(&path)
                .map(|t| t.display().to_string())
                .unwrap_or_else(|_| "?".into());
            return Err(format!(
                "{} is a symlink (to {target}); refusing to write through it. \
                 Remove the link or update its target yourself",
                path.display()
            ));
        }
    }
    if path.exists() {
        let existing = std::fs::read_to_string(&path)
            .map_err(|e| format!("reading {}: {e}", path.display()))?;
        if !is_our_skill(&existing) {
            return Err(format!(
                "{} exists and is not the rezolus-mcp skill (its frontmatter does not say \
                 `name: {SKILL_NAME}`); move it aside first",
                path.display()
            ));
        }
        if existing == SKILL_MD {
            return Ok(format!("skill up to date at {}", path.display()));
        }
    }
    if opts.dry_run {
        return Ok(format!("would write the skill to {}", path.display()));
    }
    std::fs::create_dir_all(&dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
    std::fs::write(&path, SKILL_MD).map_err(|e| format!("writing {}: {e}", path.display()))?;
    Ok(format!("wrote the skill to {}", path.display()))
}

fn claude_cli(opts: &InstallOptions) -> Option<PathBuf> {
    if let Some(given) = &opts.cli {
        return given.clone();
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join("claude"))
        .find(|p| p.is_file())
}

fn binary(opts: &InstallOptions) -> Result<PathBuf, String> {
    match &opts.binary {
        Some(b) => Ok(b.clone()),
        None => std::env::current_exe().map_err(|e| format!("locating this binary: {e}")),
    }
}

/// Register the server. Returns the report lines.
pub(crate) fn register(opts: &InstallOptions) -> Result<Vec<String>, String> {
    let bin = binary(opts)?;
    let args = server_args(&opts.server);
    let mut out = Vec::new();

    if let Some(cli) = claude_cli(opts) {
        let mut add = vec![
            "mcp".to_string(),
            "add".to_string(),
            "--scope".to_string(),
            opts.scope.as_str().to_string(),
            SERVER_NAME.to_string(),
            "--".to_string(),
            bin.display().to_string(),
        ];
        add.extend(args.iter().cloned());
        let shown = std::iter::once(cli.display().to_string())
            .chain(add.iter().cloned())
            .map(|a| shell_quote(&a))
            .collect::<Vec<_>>()
            .join(" ");
        if opts.dry_run {
            out.push(format!("would run: {shown}"));
            return Ok(out);
        }
        // Re-install is a replace: `claude mcp add` refuses a name that is
        // already registered, so an existing entry goes first. A failed
        // remove means "not registered", which is fine.
        let mut rm = Command::new(&cli);
        rm.args(["mcp", "remove", "--scope", opts.scope.as_str(), SERVER_NAME]);
        if let Some(d) = &opts.project_dir {
            rm.current_dir(d);
        }
        let removed = rm.output().map(|o| o.status.success()).unwrap_or(false);
        if removed {
            out.push(format!("replaced the existing {SERVER_NAME} entry"));
        }
        let mut cmd = Command::new(&cli);
        cmd.args(&add);
        if let Some(d) = &opts.project_dir {
            cmd.current_dir(d);
        }
        let result = cmd
            .output()
            .map_err(|e| format!("running {}: {e}", cli.display()))?;
        if !result.status.success() {
            return Err(format!(
                "`{shown}` failed ({}):\n{}{}{}",
                result.status,
                String::from_utf8_lossy(&result.stdout),
                String::from_utf8_lossy(&result.stderr),
                if removed {
                    format!(
                        "\nthe previous {SERVER_NAME} entry in {} scope was already removed",
                        opts.scope.as_str()
                    )
                } else {
                    String::new()
                }
            ));
        }
        out.push(format!(
            "registered {SERVER_NAME} with Claude Code ({} scope): {}",
            opts.scope.as_str(),
            String::from_utf8_lossy(&result.stdout).trim()
        ));
        if opts.scope == Scope::Project {
            out.push(
                "a project-scope server waits for approval: Claude Code asks the first time \
                 it opens in this directory"
                    .to_string(),
            );
        }
        // Claude Code resolves a name local > project > user, and `remove`
        // above only touched the scope being installed. Ask the client which
        // entry it now resolves to, and say so when it is another one.
        let mut get = Command::new(&cli);
        get.args(["mcp", "get", SERVER_NAME]);
        if let Some(d) = &opts.project_dir {
            get.current_dir(d);
        }
        if let Ok(o) = get.output() {
            let text = String::from_utf8_lossy(&o.stdout);
            if let Some(w) = shadow_warning(&text, opts.scope) {
                out.push(w);
            }
        }
        return Ok(out);
    }

    match opts.scope {
        Scope::Project => {
            let path = project_dir(opts)?.join(".mcp.json");
            let existing = match std::fs::read_to_string(&path) {
                Ok(s) => Some(s),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                Err(e) => return Err(format!("reading {}: {e}", path.display())),
            };
            let merged = merge_project_config(existing.as_deref(), &bin, &args)?;
            if opts.dry_run {
                out.push(format!(
                    "would write {} (no `claude` on PATH):\n{merged}",
                    path.display()
                ));
                return Ok(out);
            }
            std::fs::write(&path, merged)
                .map_err(|e| format!("writing {}: {e}", path.display()))?;
            out.push(format!(
                "wrote {} (no `claude` on PATH; Claude Code reads it when opened in this directory \
                 and asks to approve the server the first time)",
                path.display()
            ));
            Ok(out)
        }
        Scope::User => Err(format!(
            "no `claude` on PATH, and the user-scope configuration is Claude Code's own file. \
             Install Claude Code, or run this from a terminal where `claude` is on PATH:\n  \
             claude mcp add --scope user {SERVER_NAME} -- {} {}",
            shell_quote(&bin.display().to_string()),
            args.iter()
                .map(|a| shell_quote(a))
                .collect::<Vec<_>>()
                .join(" ")
        )),
    }
}

/// Quote an argument for a POSIX shell when it needs it, so a printed
/// command can be pasted even with a space in a path.
pub(crate) fn shell_quote(s: &str) -> String {
    let safe = !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./:=@%+,".contains(c));
    if safe {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

/// From `claude mcp get <name>` output: a warning when the entry the
/// client resolves is not in the scope just installed to (a local or
/// project entry shadows a user one), with the command that removes it.
pub(crate) fn shadow_warning(get_output: &str, installed: Scope) -> Option<String> {
    let line = get_output
        .lines()
        .find(|l| l.trim_start().starts_with("Scope:"))?;
    let resolved = line.trim_start()["Scope:".len()..].trim();
    let lower = resolved.to_ascii_lowercase();
    let other = if lower.starts_with("local") {
        "local"
    } else if lower.starts_with("project") {
        "project"
    } else if lower.starts_with("user") {
        "user"
    } else {
        return None;
    };
    if other == installed.as_str() {
        return None;
    }
    Some(format!(
        "warning: Claude Code resolves {SERVER_NAME} to a {other}-scope entry ({resolved}), which \
         takes precedence over the {} entry just written; remove it with \
         `claude mcp remove {SERVER_NAME} -s {other}` for this one to be used",
        installed.as_str()
    ))
}

/// The whole install: register, then the skill. Prints the report; returns
/// the exit code.
pub(crate) fn run(opts: &InstallOptions) -> i32 {
    let mut code = 0;
    match register(opts) {
        Ok(lines) => {
            for l in lines {
                println!("{l}");
            }
        }
        Err(e) => {
            eprintln!("error: {e}");
            code = 1;
        }
    }
    if opts.skill {
        match install_skill(opts) {
            Ok(line) => println!("{line}"),
            Err(e) => {
                eprintln!("error: {e}");
                code = 1;
            }
        }
    }
    if code == 0 && !opts.dry_run {
        println!(
            "done. In Claude Code, `/mcp` lists the server and `/{SKILL_NAME}` loads the skill; \
             `claude mcp list` checks the connection from a shell."
        );
    }
    code
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts(scope: Scope, dir: &Path) -> InstallOptions {
        InstallOptions {
            scope,
            server: Default::default(),
            skill: true,
            dry_run: false,
            binary: Some(PathBuf::from("/opt/rezolus")),
            project_dir: Some(dir.to_path_buf()),
            home: Some(dir.join("home")),
            cli: Some(None),
        }
    }

    #[test]
    fn server_args_carry_the_tiers_and_directories_in_a_fixed_order() {
        assert_eq!(server_args(&Default::default()), vec!["mcp"]);
        let s = super::super::server::ServerOptions {
            allow_mutating: true,
            export_dir: Some(PathBuf::from("/tmp/x")),
            viewer_url: Some("http://h:1".into()),
        };
        assert_eq!(
            server_args(&s),
            vec![
                "mcp",
                "--allow-mutating",
                "--export-dir",
                "/tmp/x",
                "--viewer-url",
                "http://h:1"
            ]
        );
    }

    #[test]
    fn the_project_config_is_merged_not_replaced() {
        let args = vec!["mcp".to_string()];
        let fresh = merge_project_config(None, Path::new("/opt/rezolus"), &args).unwrap();
        let v: serde_json::Value = serde_json::from_str(&fresh).unwrap();
        assert_eq!(v["mcpServers"]["rezolus"]["command"], "/opt/rezolus");
        assert_eq!(
            v["mcpServers"]["rezolus"]["args"],
            serde_json::json!(["mcp"])
        );

        let other = r#"{"mcpServers": {"other": {"command": "x"}, "rezolus": {"command": "old"}}, "keep": 1}"#;
        let merged = merge_project_config(Some(other), Path::new("/opt/rezolus"), &args).unwrap();
        let v: serde_json::Value = serde_json::from_str(&merged).unwrap();
        assert_eq!(
            v["mcpServers"]["other"]["command"], "x",
            "other servers stay"
        );
        assert_eq!(
            v["mcpServers"]["rezolus"]["command"], "/opt/rezolus",
            "ours is replaced"
        );
        assert_eq!(v["keep"], 1, "unknown keys stay");

        assert!(
            merge_project_config(Some("not json"), Path::new("/b"), &args)
                .err()
                .unwrap()
                .contains("not valid JSON")
        );
        assert!(merge_project_config(Some("[1]"), Path::new("/b"), &args)
            .err()
            .unwrap()
            .contains("not a JSON object"));
    }

    #[test]
    fn the_skill_is_written_updated_and_never_clobbers_a_foreign_file() {
        let dir = tempfile::tempdir().unwrap();
        let o = opts(Scope::Project, dir.path());
        let line = install_skill(&o).unwrap();
        let path = dir.path().join(".claude/skills/rezolus-mcp/SKILL.md");
        assert!(line.starts_with("wrote"), "{line}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), SKILL_MD);
        assert!(install_skill(&o).unwrap().starts_with("skill up to date"));

        // An older copy of ours is replaced; a foreign file is refused.
        std::fs::write(&path, "---\nname: rezolus-mcp\n---\nold\n").unwrap();
        assert!(install_skill(&o).unwrap().starts_with("wrote"));
        std::fs::write(&path, "---\nname: something-else\n---\n").unwrap();
        let err = install_skill(&o).err().unwrap();
        assert!(err.contains("not the rezolus-mcp skill"), "{err}");
        assert!(std::fs::read_to_string(&path)
            .unwrap()
            .contains("something-else"));

        // User scope goes under the home given.
        let u = opts(Scope::User, dir.path());
        install_skill(&u).unwrap();
        assert!(dir
            .path()
            .join("home/.claude/skills/rezolus-mcp/SKILL.md")
            .exists());

        // A dry run touches nothing.
        let d = InstallOptions {
            dry_run: true,
            ..opts(Scope::Project, &dir.path().join("dry"))
        };
        assert!(install_skill(&d).unwrap().starts_with("would write"));
        assert!(!dir.path().join("dry").exists());
    }

    #[test]
    fn a_shadowing_entry_is_reported_with_the_command_that_removes_it() {
        let local =
            "rezolus:\n  Scope: Local config (private to you in this project)\n  Status: ok\n";
        let w = shadow_warning(local, Scope::User).unwrap();
        assert!(
            w.contains("local-scope") && w.contains("claude mcp remove rezolus -s local"),
            "{w}"
        );
        let user = "rezolus:\n  Scope: User config (available in all your projects)\n";
        assert_eq!(shadow_warning(user, Scope::User), None);
        let proj = "rezolus:\n  Scope: Project config (shared via .mcp.json)\n";
        assert!(shadow_warning(proj, Scope::User)
            .unwrap()
            .contains("-s project"));
        assert_eq!(shadow_warning(proj, Scope::Project), None);
        assert_eq!(
            shadow_warning("No MCP server named rezolus", Scope::User),
            None
        );
    }

    #[test]
    fn printed_commands_are_shell_quoted() {
        assert_eq!(shell_quote("/opt/rezolus"), "/opt/rezolus");
        assert_eq!(shell_quote("/Users/a b/rez"), "'/Users/a b/rez'");
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
        assert_eq!(shell_quote(""), "''");
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_skill_file_is_refused_not_written_through() {
        let dir = tempfile::tempdir().unwrap();
        let o = opts(Scope::Project, dir.path());
        let skills = dir.path().join(".claude/skills/rezolus-mcp");
        std::fs::create_dir_all(&skills).unwrap();
        let target = dir.path().join("checkout-SKILL.md");
        std::fs::write(&target, "---\nname: rezolus-mcp\n---\nmine\n").unwrap();
        std::os::unix::fs::symlink(&target, skills.join("SKILL.md")).unwrap();
        let err = install_skill(&o).err().unwrap();
        assert!(err.contains("symlink"), "{err}");
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "---\nname: rezolus-mcp\n---\nmine\n"
        );
        // Dangling: refused the same way, not a confusing write error.
        std::fs::remove_file(&target).unwrap();
        assert!(install_skill(&o).err().unwrap().contains("symlink"));
    }

    #[test]
    fn the_embedded_skill_has_the_frontmatter_the_client_needs() {
        assert!(SKILL_MD.starts_with("---\nname: rezolus-mcp\ndescription: "));
        assert!(is_our_skill(SKILL_MD));
        assert!(!is_our_skill("# just a heading\nname: rezolus-mcp"));
        for tool in [
            "describe_recording",
            "describe_metrics",
            "extract_features",
            "query",
            "detect_anomalies",
            "analyze_correlation",
            "add_event",
            "run_checks",
            "export_query",
            "viewer_link",
            "remove_events",
        ] {
            assert!(SKILL_MD.contains(tool), "the skill must mention {tool}");
        }
    }

    #[test]
    fn user_scope_without_the_cli_says_what_to_run() {
        // `opts` says there is no `claude` CLI.
        let dir = tempfile::tempdir().unwrap();
        let r = register(&opts(Scope::User, dir.path()));
        let p = register(&opts(Scope::Project, dir.path()));
        let err = r.err().unwrap();
        assert!(
            err.contains("claude mcp add --scope user rezolus -- /opt/rezolus mcp"),
            "{err}"
        );
        let lines = p.unwrap();
        assert!(lines[0].contains(".mcp.json"), "{lines:?}");
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(dir.path().join(".mcp.json")).unwrap())
                .unwrap();
        assert_eq!(v["mcpServers"]["rezolus"]["command"], "/opt/rezolus");
    }
}
