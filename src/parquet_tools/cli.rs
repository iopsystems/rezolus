//! The `recording` command line: the clap tree (`Args` structs derived,
//! long help rendered from the [`Host`]), and dispatch to each command.
//!
//! Nothing here names the program that mounts the tools. What differs
//! between programs (the program name in messages and help, the commands it
//! has beside `recording`, the template registry `--templates` falls back
//! to, an alias, the spelling of `filter`'s table selection) comes from the
//! [`Host`], and [`Host::run`] returns the exit status for the program to
//! exit with.

use std::path::{Path, PathBuf};

use ::dashboard::TemplateRegistry;
use clap::{Args, Command, Subcommand};

/// How `filter` spells its table selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TableFlag {
    /// `--samplers A,B`: keeps every table whose name before its first `/`
    /// is named. `--tables` is accepted and hidden from help.
    Samplers,
    /// `--tables A,B`: keeps every table named, and the tables under each
    /// (`cpu` keeps `cpu/usage`). `--samplers` is accepted and hidden from
    /// help.
    Tables,
}

impl TableFlag {
    /// The flag, `--samplers` or `--tables`.
    pub fn flag(self) -> &'static str {
        match self {
            Self::Samplers => "--samplers",
            Self::Tables => "--tables",
        }
    }

    /// What the flag keeps, as `filter`'s help says it.
    fn keeps(self) -> &'static str {
        match self {
            Self::Samplers => {
                "--samplers keeps whole SAMPLERS (a sampler's group\ntables go together)"
            }
            Self::Tables => "--tables keeps whole TABLES (a name keeps the\ntables under it)",
        }
    }

    /// What it selects, in the plural: `samplers` or `tables`.
    pub fn noun(self) -> &'static str {
        match self {
            Self::Samplers => "samplers",
            Self::Tables => "tables",
        }
    }
}

/// What a program mounting the `recording` tools supplies. Build one with
/// [`Host::new`] and the setters; [`Host::command`] is the clap command to
/// mount and [`Host::run`] runs what it parsed.
#[non_exhaustive]
pub struct Host<'a> {
    /// The program's name, as messages and help spell its commands
    /// (`rezolus recording filter ...`).
    pub program: &'a str,
    /// The program's name as a heading word (`Rezolus recordings`). The
    /// program name unless set.
    pub display_name: &'a str,
    /// Another name `recording` answers to (`parquet` for rezolus).
    pub alias: Option<&'static str>,
    /// The template registry for `--templates DIR`, or the program's
    /// built-in set when it is not given.
    pub registry: &'a dyn Fn(Option<&Path>) -> TemplateRegistry,
    /// The producer whose recordings these mostly are. `convert` labels a
    /// recording `source=<agent>` unless `-m source=...` says otherwise,
    /// `metadata` prints `agent <agent> <version>`, `filter` keeps every
    /// metric of a parquet file's columns from this source, and help
    /// examples name files after it. The program name unless set.
    pub agent: &'a str,
    /// How `filter` spells its table selection. `--tables` unless set.
    pub table_flag: TableFlag,
    /// The command that lists an archive's recordings in the order
    /// `combine --ab baseline=0 experiment=1` numbers them. Without one,
    /// messages say "in archive order".
    pub list_recordings: Option<&'a str>,
    /// The program's command that records (`rezolus record`). Help and
    /// messages describe what to do without it when it is `None`, as for
    /// the next three.
    pub record: Option<&'a str>,
    /// The program's command that keeps a rolling buffer (`rezolus
    /// hindsight`).
    pub hindsight: Option<&'a str>,
    /// The program's viewer (`rezolus view`).
    pub view: Option<&'a str>,
    /// The program's query tools (`rezolus mcp`).
    pub mcp: Option<&'a str>,
    /// Where a rolling buffer usually lives
    /// (`/var/lib/rezolus/hindsight.dendro`), for `snapshot`'s example.
    pub buffer_path: Option<&'a str>,
}

impl<'a> Host<'a> {
    /// A host for `program`, whose `--templates` loads through `registry`,
    /// with every other field at its default.
    pub fn new(program: &'a str, registry: &'a dyn Fn(Option<&Path>) -> TemplateRegistry) -> Self {
        Self {
            program,
            display_name: program,
            alias: None,
            registry,
            agent: program,
            table_flag: TableFlag::Tables,
            list_recordings: None,
            record: None,
            hindsight: None,
            view: None,
            mcp: None,
            buffer_path: None,
        }
    }

    pub fn display_name(mut self, name: &'a str) -> Self {
        self.display_name = name;
        self
    }

    pub fn alias(mut self, alias: &'static str) -> Self {
        self.alias = Some(alias);
        self
    }

    pub fn agent(mut self, agent: &'a str) -> Self {
        self.agent = agent;
        self
    }

    pub fn table_flag(mut self, flag: TableFlag) -> Self {
        self.table_flag = flag;
        self
    }

    pub fn list_recordings(mut self, command: &'a str) -> Self {
        self.list_recordings = Some(command);
        self
    }

    pub fn record(mut self, command: &'a str) -> Self {
        self.record = Some(command);
        self
    }

    pub fn hindsight(mut self, command: &'a str) -> Self {
        self.hindsight = Some(command);
        self
    }

    pub fn view(mut self, command: &'a str) -> Self {
        self.view = Some(command);
        self
    }

    pub fn mcp(mut self, command: &'a str) -> Self {
        self.mcp = Some(command);
        self
    }

    pub fn buffer_path(mut self, path: &'a str) -> Self {
        self.buffer_path = Some(path);
        self
    }

    /// The `recording` command: the derived tree, with the help that names
    /// the program and its commands rendered in.
    pub fn command(&self) -> Command {
        command(self)
    }

    /// Run one `recording` subcommand. Returns the process exit status: 0 on
    /// success, 1 when the command failed, and for `check` its 0/1/2
    /// verdict.
    pub fn run(&self, cmd: Recording) -> i32 {
        run(self, cmd)
    }

    /// `text` with its placeholders rendered: `{program}`, `{agent}`, the
    /// table flag and its noun, and the phrases that name the host's other
    /// commands ([`Host::phrases`]).
    fn render(&self, text: &str) -> String {
        let mut out = text
            .replace("{program}", self.program)
            .replace("{agent}", self.agent)
            .replace("{table_flag}", self.table_flag.flag())
            .replace("{table_noun}", self.table_flag.noun())
            .replace("{table_keep}", self.table_flag.keeps());
        for (key, value) in self.phrases() {
            out = out.replace(key, &value);
        }
        out
    }

    /// Help phrases that name the host's other commands, or say the same
    /// thing without them.
    fn phrases(&self) -> Vec<(&'static str, String)> {
        let p = self.program;
        let code = |c: &str| format!("`{c}`");
        let last_word = |c: &'a str| c.rsplit(' ').next().unwrap_or(c);
        vec![
            (
                "{writers}",
                match (self.record, self.hindsight) {
                    (Some(r), Some(h)) => format!("`{r}` and `{h}` write"),
                    (Some(c), None) | (None, Some(c)) => format!("`{c}` writes"),
                    (None, None) => format!("`{p}` writes"),
                },
            ),
            (
                "{query_engine}",
                self.mcp
                    .map_or_else(|| "the viewer".to_string(), |m| code(&format!("{m} query"))),
            ),
            (
                "{viewer}",
                self.view.map_or_else(|| "the viewer".to_string(), code),
            ),
            (
                "{list_recordings_order}",
                self.list_recordings.map_or_else(
                    || "in archive order".to_string(),
                    |c| format!("as `{c}` prints them (each recording's `(position N)`)"),
                ),
            ),
            (
                "{raw_source}",
                self.record.map_or_else(
                    || "a raw recorder writes them".to_string(),
                    |r| format!("`{r} -o out.raw` wrote them in earlier releases"),
                ),
            ),
            (
                "{raw_example}",
                self.record.map_or_else(String::new, |r| {
                    format!(
                        "# The producing side, with an earlier release, for reference\n    \
                         {r} --url http://AGENT -o {}.raw\n\n    ",
                        self.agent
                    )
                }),
            ),
            (
                "{tag_example}",
                match self.record {
                    Some(r) => format!("# Tag the recording the way `{r} --metadata` would"),
                    None => "# Tag the recording".to_string(),
                },
            ),
            (
                "{appenders}",
                self.hindsight.map_or_else(
                    || "a recorder".to_string(),
                    |h| format!("a recorder or `{}`", last_word(h)),
                ),
            ),
            (
                "{buffer_example}",
                match (self.hindsight, self.buffer_path) {
                    (Some(h), Some(path)) => format!(
                        "# Grab the last N minutes of a running {} buffer\n    \
                         {p} recording snapshot {path} -o incident.dendro\n\n    ",
                        last_word(h)
                    ),
                    _ => String::new(),
                },
            ),
            (
                "{live_example}",
                match self.record {
                    Some(r) => format!("# ...or of a `{r}` still in progress"),
                    None => "# A recording still in progress".to_string(),
                },
            ),
        ]
    }

    /// The parenthetical saying how a recording's labels are set, for a
    /// refusal that cannot relabel one.
    pub(super) fn how_labels_are_set(&self) -> String {
        match self.record {
            Some(r) => format!(" (`{r} --endpoint URL,source=NAME` or `--label k=v`)"),
            None => String::new(),
        }
    }

    /// The tail of a warning that an archive will not open as a comparison.
    pub(super) fn viewer_refuses(&self) -> String {
        match self.view {
            Some(v) => format!("`{v}` refuses it without --baseline/--experiment"),
            None => "it opens as one combined view".to_string(),
        }
    }

    /// Where `combine --ab` positions come from, for its refusal.
    pub(super) fn positions_from(&self) -> String {
        match self.list_recordings {
            Some(c) => format!("as `{c}` prints them (each recording's `(position N)`)"),
            None => "in archive order".to_string(),
        }
    }
}

// The `recording` subcommands. A doc comment here would become the
// `recording` command's about text, which `command` sets from the host.
#[derive(Subcommand, Debug)]
pub enum Recording {
    #[command(about = "Add service extension KPIs or events to a .dendro recording")]
    Annotate(AnnotateArgs),
    #[command(
        about = "Evaluate the checks a recording's KPIs carry and report a verdict per check"
    )]
    Check(CheckArgs),
    #[command(about = "Combine recordings into one multi-recording .dendro archive")]
    Combine(CombineArgs),
    #[command(about = "Display the metadata of a .dendro recording")]
    Metadata(MetadataArgs),
    #[command(about = "Convert a raw msgpack recording into a .dendro archive")]
    Convert(ConvertArgs),
    // About rendered by `command`: it names the table flag's noun.
    Filter(FilterArgs),
    #[command(
        about = "Convert a .rez archive, a .parquet recording or a .parquet.ab.tar to a .dendro archive"
    )]
    Upgrade(UpgradeArgs),
    #[command(about = "Copy an archive that is still being written, without stopping the writer")]
    Snapshot(SnapshotArgs),
}

// The `Args` structs carry no doc comments: clap derive would turn them into
// help text. Each field's help is its `help` attribute, as written.

#[derive(Args, Debug)]
pub struct AnnotateArgs {
    #[arg(value_name = "FILE", help = "The recording to annotate (.dendro)")]
    pub file: PathBuf,
    #[arg(
        short = 'o',
        long = "output",
        value_name = "PATH",
        help = "Write the annotated recording to this new .dendro instead of in place"
    )]
    pub output: Option<PathBuf>,
    #[arg(
        long = "queries",
        value_name = "PATH",
        help = "Custom service extension JSON file (overrides built-in template)"
    )]
    pub queries: Option<PathBuf>,
    #[arg(
        long = "undo",
        help = "Remove the KPIs (service extension) from each recording",
        conflicts_with = "queries"
    )]
    pub undo: bool,
    // Help rendered by `command`: it names the program.
    #[arg(long = "filter", conflicts_with = "undo")]
    pub filter: bool,
    #[arg(
        long = "templates",
        value_name = "DIR",
        help = "Accepted and unused: annotate reads no template (a DIR that does not load is warned about)"
    )]
    pub templates: Option<PathBuf>,
    #[arg(
        long = "node",
        value_name = "NAME",
        help = "Refused: set a parquet footer key; a recording's labels are set when it is recorded"
    )]
    pub node: Option<String>,
    #[arg(
        long = "source",
        value_name = "NAME",
        help = "Refused: set a parquet footer key; a recording's labels are set when it is recorded",
        conflicts_with = "undo"
    )]
    pub source: Option<String>,
    #[arg(
        long = "overwrite",
        help = "Applied to --source, which is refused",
        requires = "source"
    )]
    pub overwrite: bool,
    #[arg(
        long = "systeminfo",
        value_name = "PATH",
        help = "Store systeminfo JSON from PATH (or '-' for stdin) in each recording"
    )]
    pub systeminfo: Option<PathBuf>,
    #[arg(
        long = "add-events",
        value_name = "PATH",
        help = "Add one-off events from a JSON/JSONL file (or '-' for stdin). Repeatable.",
        conflicts_with = "undo"
    )]
    pub add_events: Vec<PathBuf>,
    #[arg(
        long = "event",
        value_name = "KV",
        help = "Add a single event inline, e.g. 'time=2026-05-12T15:23Z,kind=restart,description=\"...\"'. \
                Add duration=30s (or duration_ns=N) for a range that the viewer shades from time to \
                time+duration. Repeatable.",
        conflicts_with = "undo"
    )]
    pub event: Vec<String>,
    #[arg(
        long = "clear-events",
        help = "Remove existing events before applying --add-events / --event",
        conflicts_with = "undo"
    )]
    pub clear_events: bool,
}

#[derive(Args, Debug)]
pub struct CheckArgs {
    #[arg(value_name = "FILE", help = "The recording to check (.dendro)")]
    pub file: PathBuf,
    #[arg(
        long = "queries",
        value_name = "PATH",
        help = "Service-extension JSON whose KPI checks to run, instead of the recording's own"
    )]
    pub queries: Option<PathBuf>,
    #[arg(
        id = "RECORDING",
        long = "recording",
        value_name = "KEY=VALUE",
        help = "Which recording of a multi-recording archive to check, as key=value (e.g. --recording source=redis)\n\
                Repeatable: the pairs are ANDed and must together name exactly one\n\
                recording. Without it every recording is checked."
    )]
    pub recording: Vec<String>,
    #[arg(
        long = "annotate",
        help = "Write each FAIL/WARN window into the recording as a kind=check range event"
    )]
    pub annotate: bool,
    #[arg(
        short = 'o',
        long = "output",
        value_name = "PATH",
        help = "With --annotate, write the events into this new .dendro instead of the recording",
        requires = "annotate"
    )]
    pub output: Option<PathBuf>,
    #[arg(
        long = "json",
        help = "Print the verdicts as a JSON array instead of one line per check"
    )]
    pub json: bool,
    #[arg(
        long = "templates",
        value_name = "DIR",
        help = "Directory of service-extension template JSON files to use instead of the built-in set, for a recording with no embedded KPIs (matched to the recording's source metadata)"
    )]
    pub templates: Option<PathBuf>,
}

#[derive(Args, Debug)]
pub struct CombineArgs {
    #[arg(
        value_name = "FILES",
        required = true,
        num_args = 1..,
        help = "Input recordings (.dendro)"
    )]
    pub files: Vec<PathBuf>,
    #[arg(
        short = 'o',
        long = "output",
        value_name = "output",
        help = "The .dendro archive to write; must not exist"
    )]
    pub output: PathBuf,
    #[arg(
        long = "bypass-time-check",
        help = "Applied to the parquet row-merge, which is no longer written; refused"
    )]
    pub bypass_time_check: bool,
    #[arg(
        long = "pinned",
        value_name = "pinned",
        help = "Applied to the parquet row-merge, which is no longer written; refused"
    )]
    pub pinned: Option<String>,
    #[arg(
        long = "ab",
        value_name = "ab",
        num_args = 2,
        help = "Mark the two inputs as the two arms of an A/B comparison: \
                `--ab baseline=<input> experiment=<input>`. Each value names \
                one input (the path as typed, or its file name), and every \
                recording of that input is labelled arm=baseline or \
                arm=experiment, so the viewer opens the archive as a \
                comparison. An input recording that already carries a \
                different arm is refused; the same arm is kept. With one \
                archive input, each value is the position of one of its \
                recordings. Without --ab the archive opens as one combined \
                view."
    )]
    pub ab: Vec<String>,
    #[arg(
        long = "category",
        value_name = "NAME",
        help = "Applied to the A/B tarball, which is no longer written; refused",
        requires = "ab"
    )]
    pub category: Option<String>,
}

#[derive(Args, Debug)]
pub struct MetadataArgs {
    #[arg(
        short = 'i',
        long = "input",
        value_name = "input",
        help = "The recording (.dendro)"
    )]
    pub input: PathBuf,
    #[arg(long = "json", help = "Output in JSON format (for programmatic use)")]
    pub json: bool,
}

#[derive(Args, Debug)]
pub struct ConvertArgs {
    #[arg(
        value_name = "FILE",
        help = "Raw recording to convert (plain or zstd-compressed). Must be a real file; stdin is not accepted here"
    )]
    pub file: PathBuf,
    #[arg(
        short = 'o',
        long = "output",
        value_name = "PATH",
        help = "Output .dendro path (default: input with a trailing .zst then .raw stripped and .dendro appended). Refuses to overwrite an existing file unless --force"
    )]
    pub output: Option<PathBuf>,
    // `--interval` must never gain a `-i` alias: `recording metadata -i`
    // means the input file, so `convert -i recording.raw` would parse a path
    // as a duration and report "expected number at 0".
    #[arg(
        long = "interval",
        value_name = "DURATION",
        value_parser = clap::value_parser!(humantime::Duration),
        help = "Sampling interval to stamp, as a number with a unit (ns, us, ms, s, m, h) -- 1s, 250ms. Stamped as whole milliseconds, rounded to the nearest; anything below 1ms is rejected. Default: inferred from the median gap between snapshot timestamps, or 1s if the recording holds fewer than two snapshots"
    )]
    pub interval: Option<humantime::Duration>,
    #[arg(
        long = "systeminfo",
        value_name = "PATH",
        help = "JSON hardware summary to embed, as served by the agent's /systeminfo endpoint; any JSON value is accepted and stored verbatim. Use - for stdin (only one of --systeminfo/--descriptions can read stdin)"
    )]
    pub systeminfo: Option<PathBuf>,
    #[arg(
        long = "descriptions",
        value_name = "PATH",
        help = "Metric help text to embed, as served by the agent's /metrics/descriptions endpoint: a flat JSON object of name to string, {\"cpu_usage\":\"CPU time by state\"}. Use - for stdin (only one of --systeminfo/--descriptions can read stdin)"
    )]
    pub descriptions: Option<PathBuf>,
    // Help rendered by `command`: it names the default source.
    #[arg(short = 'm', long = "metadata", value_name = "KEY=VALUE")]
    pub metadata: Vec<String>,
    #[arg(
        long = "force",
        help = "Overwrite the output file if it already exists (without this, an existing output is an error and nothing is written)"
    )]
    pub force: bool,
}

#[derive(Args, Debug)]
pub struct FilterArgs {
    #[arg(value_name = "FILE", help = "Recording to filter (.dendro)")]
    pub file: PathBuf,
    #[arg(
        short = 'o',
        long = "output",
        value_name = "PATH",
        help = "Write the filtered recording to this new .dendro (default: rewrite the input in place)"
    )]
    pub output: Option<PathBuf>,
    #[arg(
        long = "samplers",
        value_name = "A,B,...",
        help = "Comma-separated sampler names to KEEP; every other sampler's tables are dropped. Filtering is by sampler, so a sampler's group tables go together"
    )]
    pub samplers: Option<String>,
    // Help rendered by `command`: it names the table flag.
    #[arg(long = "metrics", value_name = "A,B,...")]
    pub metrics: Option<String>,
    #[arg(
        long = "tables",
        value_name = "A,B,...",
        help = "Comma-separated table names to KEEP; a name also keeps every table under it (`cpu` keeps `cpu` and `cpu/usage`). Every other table is dropped"
    )]
    pub tables: Option<String>,
}

#[derive(Args, Debug)]
pub struct UpgradeArgs {
    #[arg(
        value_name = "FILE",
        help = "The recording to convert: a .rez archive, a .parquet file or a .parquet.ab.tar"
    )]
    pub file: PathBuf,
    #[arg(
        short = 'o',
        long = "output",
        value_name = "DENDRO",
        help = "The .dendro to write (default: the input's name with .dendro for its extension; x.parquet.ab.tar becomes x.dendro); must not exist"
    )]
    pub output: Option<PathBuf>,
    #[arg(
        long = "to",
        value_name = "FORMAT",
        default_value = "dendro",
        value_parser = clap::builder::NonEmptyStringValueParser::new(),
        help = "Target container: `dendro`, the only one written (accepted for scripts that pass it)"
    )]
    pub to: String,
}

#[derive(Args, Debug)]
pub struct SnapshotArgs {
    #[arg(
        value_name = "FILE",
        help = "The archive to snapshot (.dendro; may be open for writing)"
    )]
    pub file: PathBuf,
    #[arg(
        short = 'o',
        long = "output",
        value_name = "DENDRO",
        help = "The .dendro to write. Must not already exist."
    )]
    pub output: PathBuf,
}

fn command(host: &Host) -> Command {
    let cmd = Recording::augment_subcommands(Command::new("recording"))
        .about(format!(
            "Inspect and transform {} recordings (.dendro archives)",
            host.display_name
        ))
        .long_about(match host.alias {
            Some(alias) => format!(
                "{}\n\n`{} {alias} ...` still works and does the same thing.",
                host.render(help::RECORDING_LONG),
                host.program
            ),
            None => host.render(help::RECORDING_LONG),
        })
        .subcommand_required(true);
    let cmd = match host.alias {
        Some(alias) => cmd.alias(alias),
        None => cmd,
    };
    let (shown, hidden) = match host.table_flag {
        TableFlag::Samplers => ("samplers", "tables"),
        TableFlag::Tables => ("tables", "samplers"),
    };
    cmd.mut_subcommand("annotate", |c| {
        c.long_about(host.render(help::ANNOTATE_LONG))
            .mut_arg("filter", |a| {
                a.help(host.render(
                    "Refused: filtered a parquet file's columns; use `{program} recording filter`",
                ))
            })
    })
    .mut_subcommand("check", |c| c.long_about(host.render(help::CHECK_LONG)))
    .mut_subcommand("combine", |c| c.long_about(host.render(help::COMBINE_LONG)))
    .mut_subcommand("metadata", |c| {
        c.long_about(host.render(help::METADATA_LONG))
    })
    .mut_subcommand("convert", |c| {
        c.long_about(host.render(help::CONVERT_LONG))
            .mut_arg("metadata", |a| a.help(host.render(help::CONVERT_METADATA)))
    })
    .mut_subcommand("filter", |c| {
        c.about(host.render(help::FILTER_ABOUT))
            .long_about(host.render(help::FILTER_LONG))
            .mut_arg("metrics", |a| a.help(host.render(help::FILTER_METRICS)))
            .mut_arg(hidden, |a| a.hide(true))
            .mut_arg(shown, |a| a.hide(false))
    })
    .mut_subcommand("upgrade", |c| c.long_about(host.render(help::UPGRADE_LONG)))
    .mut_subcommand("snapshot", |c| {
        c.long_about(host.render(help::SNAPSHOT_LONG))
    })
}

fn run(host: &Host, cmd: Recording) -> i32 {
    let result = match cmd {
        Recording::Annotate(args) => {
            // Loaded although annotate reads no template: a `--templates`
            // that does not load is warned about, as for check and filter.
            let _registry = (host.registry)(args.templates.as_deref());
            return super::annotate::run(host, &args);
        }
        Recording::Check(args) => {
            let registry = (host.registry)(args.templates.as_deref());
            return super::check::run(host, &args, &registry);
        }
        Recording::Combine(args) => super::combine::run(host, &args),
        Recording::Convert(args) => return super::convert::run(host, &args),
        Recording::Filter(args) => return super::filter::run(host, &args),
        Recording::Metadata(args) => super::metadata::run(host, &args),
        Recording::Upgrade(args) => {
            super::upgrade_to_dendro(&args.file, args.output.as_deref(), &args.to)
        }
        Recording::Snapshot(args) => super::snapshot_rez(&args.file, &args.output, host.program),
    };
    match result {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("Error: {e}");
            1
        }
    }
}

/// Help text, with its placeholders rendered by `Host::render`.
mod help {
    pub(super) const FILTER_ABOUT: &str =
        "Shrink a .dendro recording to the {table_noun} or metrics named";

    pub(super) const FILTER_METRICS: &str = "Comma-separated metric names to KEEP as COLUMNS; every other metric's columns are dropped and re-encoded. Timestamp and acquisition-window sidecars are always kept; a table left with none of the kept metrics is dropped. Combine with {table_flag}, or use either alone";

    pub(super) const RECORDING_LONG: &str = "Offline operations on recordings: the .dendro archives that\n\
                 {writers}.\n\n\
                 SUBCOMMANDS:\n    \
                 metadata   Describe an archive's recordings, labels, metadata and tables\n    \
                 annotate   Embed service-extension KPIs or events into a recording\n    \
                 check      Evaluate the KPI checks a recording carries and report pass/fail verdicts\n    \
                 combine    Combine recordings into one .dendro archive, optionally labelled as an A/B\n    \
                 convert    Turn a raw msgpack recording (`record -o out.raw` in earlier releases) into a .dendro\n    \
                 filter     Shrink a recording to the {table_noun} or metrics named\n    \
                 snapshot   Copy an archive that is still being written into a complete .dendro\n    \
                 upgrade    Convert a recording an earlier release wrote into a .dendro archive\n\n\
                 Every subcommand but `upgrade` opens .dendro archives only. A .rez archive,\n\
                 a .parquet file or a .parquet.ab.tar A/B tarball is refused with the command\n\
                 that converts it: `{program} recording upgrade FILE`.\n\n\
                 Run `{program} recording <subcommand> --help` for per-subcommand examples.";

    pub(super) const ANNOTATE_LONG: &str = "Embed metadata the viewer reads into each recording of a .dendro archive,\n\
                         in place: service-extension KPIs (--queries, or --undo to remove them),\n\
                         timeline events (--event/--add-events/--clear-events) and a systeminfo JSON\n\
                         (--systeminfo). At least one is required. With -o <new.dendro> the archive\n\
                         is copied there and the copy edited.\n\n\
                         --undo removes each recording's KPIs, and --systeminfo stores a systeminfo\n\
                         JSON in each recording. --source, --node and --filter edited a parquet\n\
                         file's footer and are refused: a recording's labels are set when it is\n\
                         recorded, and `recording filter` shrinks a recording.\n\n\
                         EXAMPLES:\n    \
                         # Embed KPIs into each recording of an archive\n    \
                         {program} recording annotate run.dendro --queries kpis.json\n\n    \
                         # Add a timeline event\n    \
                         {program} recording annotate run.dendro --event 'time=2026-05-12T15:23Z,kind=deploy,description=\"rollout\"'\n\n    \
                         # Leave the original alone: write the annotated copy elsewhere\n    \
                         {program} recording annotate run.dendro --queries kpis.json -o annotated.dendro";

    pub(super) const CHECK_LONG: &str = "Run every KPI check over a recording and print one line per check\n\
                         (PASS / WARN / FAIL / INDETERMINATE / ERROR), then a summary line.\n\n\
                         THE FILE: checks live on KPIs in a service-extension JSON file, the same\n\
                         file `recording annotate --queries` embeds for the viewer's KPI dashboard:\n\n    \
                         {\n      \
                           \"service_name\": \"myservice\",\n      \
                           \"kpis\": [\n        \
                             {\"role\": \"latency\", \"title\": \"p99 request latency\",\n         \
                              \"query\": \"request_latency_seconds\", \"type\": \"histogram\",\n         \
                              \"check\": {\"above\": 0.25, \"quantile\": 0.99, \"for\": \"10s\"}},\n        \
                             {\"role\": \"throughput\", \"title\": \"Request rate\",\n         \
                              \"query\": \"sum(irate(http_requests_total[10s]))\", \"type\": \"delta_counter\",\n         \
                              \"check\": {\"below\": 100, \"severity\": \"warn\"}}\n      \
                           ]\n    \
                         }\n\n\
                         `service_name` (required) names the service; `aliases` (a list) and\n\
                         `service_metadata` (a map) are optional. Each KPI needs `role` (any word;\n\
                         it groups charts on the dashboard), `title` (keep it unique in the file:\n\
                         it names the check in output and events), `query` (PromQL) and `type`:\n\
                         `gauge`, `histogram`, or a counter kind (`delta_counter`, `counter`).\n\
                         Any other `type` is charted as a counter and evaluated as written.\n\
                         Optional: `description`, `unit_system`, `subtype`, `percentiles`,\n\
                         `subgroup`, `subgroup_description`, `full_width`, `denominator`, `check`.\n\
                         A KPI without `check` is a chart only and is skipped here.\n\n\
                         THE CHECK: exactly one of `above` / `below` is the threshold, compared in\n\
                         the query's own unit (seconds for the histogram above, requests per\n\
                         second for the rate); the comparison is strict, so a value equal to the\n\
                         threshold passes. `for` (default 0s) is the shortest run that counts,\n\
                         written as 500ms, 10s, 1m30s, 2h, 1d, 1.5s, or a bare number of seconds.\n\
                         `severity` is `fail` (default) or `warn`. A histogram KPI needs `quantile`\n\
                         in (0, 1]: its query names the raw histogram and the check evaluates\n\
                         histogram_quantile(<quantile>, <query>) itself, so leave\n\
                         histogram_quantile out of the query. `quantile` on any other type is an\n\
                         ERROR. Unknown keys inside `check` are rejected.\n\n\
                         WHICH CHECKS RUN: the KPIs embedded in the recording (by\n\
                         `recording annotate --queries`); else the built-in template whose\n\
                         `service_name` or `aliases` match the recording's `source` metadata, the\n\
                         same lookup the viewer makes. --templates <DIR> uses the template JSON\n\
                         files in DIR instead of the built-in set for that fallback. --queries\n\
                         <file> runs that file's checks instead of either, and writes nothing.\n\n\
                         HOW A CHECK IS EVALUATED: the query runs over the whole recording through\n\
                         the same engine as {query_engine}, on a grid of one point per step\n\
                         (the recording's sampling interval, at most 1s), and must yield exactly\n\
                         one series (aggregate with sum(...) or add label matchers if it yields\n\
                         several). A query that matches nothing is an ERROR, not a pass. Each\n\
                         point is classified: where a value carries an acquisition-window\n\
                         uncertainty band (rate(), irate(), histogram quantiles) the band is\n\
                         compared, so `above` fires only when the whole band is above the\n\
                         threshold and `below` only when it is entirely below; a point whose band\n\
                         straddles the threshold, or one interpolated across a span the recording\n\
                         never observed, is INDETERMINATE. A run is consecutive points that do\n\
                         not pass; a passing point, or a gap of more than 1.5 steps between\n\
                         points (the engine emits no point where the recording has no data),\n\
                         ends it. A run violates only if every point in it violates; one\n\
                         indeterminate point makes the whole run indeterminate, so a long\n\
                         violation with an intermittent straddle is one long INDETERMINATE window\n\
                         rather than short violations that never reach `for`. The step is the grid\n\
                         step or the series' own point spacing, whichever is coarser (a 10 s\n\
                         sampler on a 1 s grid yields a point per 10 s, each spanning 10 s). A run's\n\
                         span is (last point - first point + step); it is a window when\n\
                         span >= for. A check with a violating window is FAIL (WARN for severity\n\
                         warn) even if it also has indeterminate windows; with only indeterminate\n\
                         windows it is INDETERMINATE; otherwise PASS.\n\n\
                         EXIT STATUS: 2 if any check could not be evaluated (or the command itself\n\
                         failed), else 1 if any check with severity `fail` failed, else 0. WARN and\n\
                         INDETERMINATE never fail the run. No checks at all is 0, with a note on\n\
                         stderr.\n\n\
                         MULTI-RECORDING ARCHIVE: with no --recording every recording is checked and\n\
                         each line is prefixed with its labels; --recording k=v narrows to one. A\n\
                         recording and its copies (one source copied, or recorded twice from one\n\
                         agent process) are one recording.\n\n\
                         --annotate writes each FAIL/WARN window into the recording as a range\n\
                         event: kind=check, timestamp=window start, duration_ns=span (the event\n\
                         ends one step after the last violating point), description=the KPI title,\n\
                         details=the condition and, on a second line, the check as JSON. The event\n\
                         id is a hash of the title, the evaluated query, the condition and the\n\
                         window start: running again rewrites a window that grew and adds nothing\n\
                         for one that did not, and a window that no longer fires keeps its old\n\
                         event. The events go into the recording they were evaluated against (each\n\
                         recording its own when no --recording is given), in its sources' metadata:\n\
                         in place, or into a copy with -o <new.dendro>. With --json the annotation\n\
                         report goes to stderr so stdout stays one JSON array.\n\n\
                         --json prints an array with one object per check: `recording` (labels),\n\
                         `title`, `query` (as evaluated), `check`, `status`, `windows` and\n\
                         `indeterminate` (each with `start` and `end` as RFC 3339, `start_ns`,\n\
                         `duration_ns`, `points`), and `error` (ERROR only).\n\n\
                         EXAMPLES:\n    \
                         # Run the checks a recording already carries\n    \
                         {program} recording check run.dendro\n\n    \
                         # Run checks from a file instead (nothing is written to the recording)\n    \
                         {program} recording check run.dendro --queries kpis.json\n\n    \
                         # CI: fail the job on a violated check, and store the verdicts as events\n    \
                         {program} recording check run.dendro --annotate || exit 1\n\n    \
                         # One recording of a multi-recording archive, machine-readable\n    \
                         {program} recording check fleet.dendro --recording host=web-01 --json";

    pub(super) const COMBINE_LONG: &str = "Assemble two or more .dendro archives into one, a recording per input\n\
                         recording, which the viewer opens as one combined view. Requires at least\n\
                         two inputs (one archive with --ab, below) and an output (-o) ending in\n\
                         .dendro. A recording and its copy (a snapshot, a hindsight dump) given\n\
                         together are read as one recording, as {viewer} reads them.\n\n\
                         With `--ab baseline=<input> experiment=<input>` each input's recordings\n\
                         are labelled arm=baseline or arm=experiment and the viewer opens a\n\
                         baseline/experiment comparison; an input recording that already carries\n\
                         the other arm is refused, naming the input. With two inputs, each value\n\
                         is an input's path or file name. With ONE archive input, each value is\n\
                         the position of one of its recordings, from 0,\n\
                         {list_recordings_order}.\n\
                         Every source of a recording, such as a source and its snapshot, takes\n\
                         its arm. That is the way to compare the two recordings of an archive\n\
                         combined without --ab, whose labels may be identical.\n\n\
                         --pinned, --category and --bypass-time-check applied to the parquet\n\
                         row-merge and the A/B tarball, which are no longer written; they are\n\
                         refused.\n\n\
                         EXAMPLES:\n    \
                         # Two hosts recorded separately in one run: one combined view\n    \
                         {program} recording combine web-01.dendro web-02.dendro -o run.dendro\n\n    \
                         # A/B compare two captures\n    \
                         {program} recording combine a.dendro b.dendro --ab baseline=a.dendro experiment=b.dendro -o ab.dendro\n\n    \
                         # Label the two recordings of one archive as an A/B, by position\n    \
                         {program} recording combine run.dendro --ab baseline=0 experiment=1 -o ab.dendro";

    pub(super) const METADATA_LONG: &str =
        "Describe a .dendro archive: its recordings, their labels and metadata, and\n\
                         each table with its cadence; --json emits the description as JSON.\n\n\
                         EXAMPLES:\n    \
                         # Describe an archive\n    \
                         {program} recording metadata -i {agent}.dendro\n\n    \
                         # The same, as JSON\n    \
                         {program} recording metadata -i {agent}.dendro --json";

    pub(super) const CONVERT_LONG: &str = "Convert a raw recording (concatenated msgpack snapshots, one per sampling\n\
                         tick, as {raw_source})\n\
                         into a .dendro archive that the viewer and the rest of\n\
                         `{program} recording` can read.\n\n\
                         The input may be plain or zstd-compressed; which one it is is detected from\n\
                         the file's contents, not its name. The output path defaults to the input\n\
                         with a trailing .zst and then a trailing .raw stripped, and .dendro\n\
                         appended: {agent}.raw.zst becomes {agent}.dendro, and a name with neither\n\
                         suffix just gains one (capture.msgpack becomes capture.msgpack.dendro). An\n\
                         -o that does not end in .dendro is refused.\n\n\
                         The archive holds one source labelled source={agent} (or the value of\n\
                         --metadata source=...). The sampling interval stamped into its metadata is\n\
                         inferred from the median gap between snapshot timestamps unless --interval\n\
                         says otherwise. A recording with fewer than two snapshots falls back to 1s,\n\
                         so pass --interval if such a recording was made at another cadence.\n\
                         Inference warns on stderr when the sampled gaps have no dominant cadence, or\n\
                         when they are closer together than the whole milliseconds the stamped value\n\
                         can hold; the conversion still succeeds in both cases.\n\n\
                         A raw recording carries no systeminfo or metric descriptions: the recorder\n\
                         fetches those over HTTP from the running {agent} agent while recording (its\n\
                         /systeminfo and /metrics/descriptions endpoints) and they never enter the\n\
                         snapshot stream. Without them the conversion still succeeds and every metric\n\
                         still queries normally; what you lose is the viewer's hardware panel and the\n\
                         help text beside each metric. Stamp them here if you saved them -- and if\n\
                         that agent is still running, the same two endpoints can be curled now, at\n\
                         conversion time, to recover them. They can only be supplied here, which\n\
                         means reconverting the original raw input over the output (--force).\n\n\
                         EXAMPLES:\n    \
                         {raw_example}# Convert a compressed recording (writes {agent}.dendro)\n    \
                         {program} recording convert {agent}.raw.zst\n\n    \
                         # Choose the output path\n    \
                         {program} recording convert {agent}.raw -o run7.dendro\n\n    \
                         # A recording made at a non-default cadence\n    \
                         {program} recording convert {agent}.raw --interval 250ms\n\n    \
                         # Save the two metadata blobs at record time, from the agent being recorded\n    \
                         curl -s http://AGENT/systeminfo > sysinfo.json\n    \
                         curl -s http://AGENT/metrics/descriptions > help.json\n\n    \
                         # ...then stamp them into the conversion\n    \
                         {program} recording convert {agent}.raw --systeminfo sysinfo.json --descriptions help.json\n\n    \
                         # Or pipe one of them straight in, with - for stdin\n    \
                         curl -s http://AGENT/systeminfo | {program} recording convert {agent}.raw --systeminfo -\n\n    \
                         {tag_example}\n    \
                         {program} recording convert {agent}.raw -m source=llm-perf -m run=boat-7\n\n    \
                         # Replace an output left by an earlier attempt\n    \
                         {program} recording convert {agent}.raw --force\n\n    \
                         # Descriptions were missed the first time -- reconvert over the old output\n    \
                         {program} recording convert {agent}.raw --descriptions help.json --force";

    pub(super) const FILTER_LONG: &str = "Shrink a .dendro recording: {table_keep} and --metrics keeps metric COLUMNS (re-encoding each\n\
                         table's segments, always keeping the timestamp and acquisition-window\n\
                         sidecars; a table left with none of the kept metrics is dropped). Use\n\
                         either alone or together; at least one is required.\n\n\
                         The archive is rewritten in place, or with -o/--output written to a new\n\
                         .dendro, leaving the original untouched.\n\n\
                         EXAMPLES:\n    \
                         # Keep only two {table_noun} of a .dendro, writing a slimmed copy\n    \
                         {program} recording filter capture.dendro {table_flag} cpu_usage,scheduler -o slim.dendro\n\n    \
                         # Keep only specific metric columns, in place\n    \
                         {program} recording filter capture.dendro --metrics cpu_usage,cpu_frequency";

    pub(super) const UPGRADE_LONG: &str =
        "Write a new .dendro archive holding a recording an earlier release wrote:\n\
                         a v1, v2 or v3 `.rez`, a standalone `.parquet`, or a `.parquet.ab.tar`\n\
                         A/B tarball. Every other command opens .dendro archives only and refuses\n\
                         these with this command.\n\n\
                         A v3 `.rez`'s recordings become sources and its tables streams; segment,\n\
                         WAL and caller-row bytes are copied unchanged. A tar `.rez`'s recordings\n\
                         become sources with their labels, metadata, clock anchor and `complete`\n\
                         flag. A parquet file becomes one source, labelled with the file's\n\
                         `source` when that names one source and no series carries its own;\n\
                         a combined parquet (5.x `recording combine`, or the 5.x recorder given\n\
                         several endpoints) becomes one source per input file, labelled with\n\
                         its `source` and its `node` or `instance`, which the viewer opens as\n\
                         one combined view; a column carrying an exporter's own `source` forms\n\
                         a source of that name.\n\
                         An A/B tarball becomes two sources, its baseline labelled arm=baseline\n\
                         and its experiment arm=experiment, which the viewer opens as a\n\
                         comparison. Tar and parquet tables are re-encoded.\n\n\
                         The input is never changed. -o defaults to the input's name with .dendro\n\
                         for its extension (x.parquet.ab.tar becomes x.dendro), and an existing\n\
                         output is refused.\n\n\
                         EXAMPLES:\n    \
                         # Writes old.dendro beside old.rez\n    \
                         {program} recording upgrade old.rez\n\n    \
                         # A parquet recording, to a chosen name\n    \
                         {program} recording upgrade capture.parquet -o capture.dendro\n\n    \
                         # An A/B tarball: writes ab.dendro, a two-arm comparison\n    \
                         {program} recording upgrade ab.parquet.ab.tar";

    pub(super) const SNAPSHOT_LONG: &str = "Take a complete, standalone .dendro copy of an archive while\n\
                         {appenders} is still appending to it.\n\n\
                         WHY NOT `cp`: an archive is a SQLite database in WAL mode, so recent\n\
                         commits live in a `<file>-wal` SIDECAR until they are checkpointed into\n\
                         the archive. Copying the archive alone leaves them behind, and the copy\n\
                         reads as a recording that stopped early.\n\n\
                         The archive is copied in one read snapshot, its live tail sealed into the\n\
                         copy, so the copy holds no WAL rows. The writer is never paused and the\n\
                         original is not modified. The output must end in .dendro and must not\n\
                         exist.\n\n\
                         The result is what to hand to someone else, upload to the static-site\n\
                         viewer (a browser is given one file and cannot see a sidecar), or keep\n\
                         as an incident artifact.\n\n\
                         EXAMPLES:\n    \
                         {buffer_example}{live_example}\n    \
                         {program} recording snapshot {agent}.dendro -o so-far.dendro";

    pub(super) const CONVERT_METADATA: &str = "Add a file-level metadata tag as key=value; repeat for multiple tags. Split on the first =, so values may contain = and may be empty. Keys are free-form, but `source` is the one the viewer reads to identify where a recording came from (it defaults to {agent}). Tags are applied after the two keys convert derives, `source` and `sampling_interval_ms`, so a tag with either name wins -- set the interval with --interval rather than -m sampling_interval_ms=";
}
