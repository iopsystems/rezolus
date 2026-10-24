use std::collections::BTreeSet;
use std::path::Path;

use super::legacy::Container;

/// What a filter keeps. `samplers` and `tables` select tables (a table is
/// kept when either names it); `metrics` selects columns within the kept
/// tables. `flag` is how the host spells the table selection in messages
/// (`--samplers` for rezolus).
pub(super) struct Keep<'a> {
    pub(super) samplers: Option<&'a BTreeSet<String>>,
    pub(super) tables: Option<&'a BTreeSet<String>>,
    pub(super) metrics: Option<&'a BTreeSet<String>>,
    pub(super) flag: &'a str,
}

impl Keep<'_> {
    fn selects_tables(&self) -> bool {
        self.samplers.is_some() || self.tables.is_some()
    }

    /// Whether the table `table` is kept: every table when neither
    /// `samplers` nor `tables` is given, else one whose sampler (its name
    /// before the first `/`) is in `samplers`, or which is in `tables` or
    /// under a name there (`cpu` keeps `cpu/usage`).
    fn keeps_table(&self, table: &str) -> bool {
        if !self.selects_tables() {
            return true;
        }
        self.samplers
            .is_some_and(|keep| keep.contains(metriken_storage::table_sampler(table)))
            || self
                .tables
                .is_some_and(|keep| keep.iter().any(|name| table_is_under(table, name)))
    }
}

/// Whether `table` is `name` or a table under it (`name/...`).
fn table_is_under(table: &str, name: &str) -> bool {
    table
        .strip_prefix(name)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

/// Returns the process exit status.
pub(super) fn run(host: &super::Host, args: &super::cli::FilterArgs) -> i32 {
    match run_inner(host, args) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("error: {e}");
            1
        }
    }
}

fn run_inner(host: &super::Host, args: &super::cli::FilterArgs) -> Result<(), String> {
    let path = &args.file;
    let output = args.output.as_deref();
    let csv = |list: &Option<String>| -> Option<BTreeSet<String>> {
        list.as_ref().map(|list| {
            list.split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        })
    };
    let samplers = csv(&args.samplers);
    let tables = csv(&args.tables);
    let metrics = csv(&args.metrics);
    let (flag, noun) = (host.table_flag.flag(), host.table_flag.noun());
    ::rez::container::require_dendro(path, host.program)?;

    if samplers.is_none() && tables.is_none() && metrics.is_none() {
        return Err(format!(
            "filtering an archive requires {flag} <a,b,...> ({noun} to keep) \
             and/or --metrics <a,b,...> (metric columns to keep)"
        ));
    }

    let keep = Keep {
        samplers: samplers.as_ref(),
        tables: tables.as_ref(),
        metrics: metrics.as_ref(),
        flag,
    };
    filter(path, &keep, output, host.program)
        .map_err(|e| format!("failed to filter {}: {e}", path.display()))
}

/// Filter the dendro archive at `path` to what `keep` names, in place or
/// into `output` (a new `.dendro`). Any other file is refused, one an
/// earlier release wrote with the `upgrade` command spelled with `program`.
fn filter(
    path: &Path,
    keep: &Keep<'_>,
    output: Option<&Path>,
    program: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    match Container::of_path(path) {
        Container::Dendro => {
            if let Some(output) = output.filter(|o| o.extension().is_none_or(|e| e != "dendro")) {
                return Err(format!(
                    "{}: the output is a .dendro archive; give a name ending in .dendro",
                    output.display()
                )
                .into());
            }
            filter_dendro(path, keep, output)
        }
        c => Err(::rez::container::not_dendro(&path.display().to_string(), c, program).into()),
    }
}

/// Filter a dendro archive by sampler and/or by metric column, through
/// dendro's `copy_sources_into` (`crate::dendro_copy`).
///
/// - `keep_samplers` keeps a stream when its sampler is named; an occupant
///   stream (`<table>/occupants`) counts as its table's, so it goes with it.
/// - `keep_metrics` trims columns with metriken-storage's `KeepMetrics`,
///   which keeps a long table long (its `occupant` column and layout
///   markers) and copies occupant streams whole, re-encoding with the
///   writer's properties. A table left with no kept metric is dropped by the
///   copy, and its occupant stream is then removed here, since dendro only
///   sees the stream's own columns.
///
/// The live tail of each kept stream is sealed into the copy, as a snapshot
/// seals it.
fn filter_dendro(
    path: &Path,
    keep: &Keep<'_>,
    output: Option<&Path>,
) -> Result<(), Box<dyn std::error::Error>> {
    use ::rez::occupants::table_of;
    use dendro::archive::{Archive, ArchiveMut};
    use dendro::rewrite::CopySpec;
    use metriken_storage::table_sampler;

    // An `-o` naming another file must be a new `.dendro`; one naming the
    // input itself (`./live.dendro` for `live.dendro`) filters in place.
    let in_place = output.is_none_or(|o| super::legacy::same_path(o, path));
    if let Some(output) = output.filter(|_| !in_place) {
        super::legacy::check_new_dendro(output)?;
    }

    let src = Archive::open(path)?;
    let tables: Vec<String> = crate::dendro_copy::streams(&src)?
        .into_iter()
        .filter(|s| table_of(s).is_none())
        .collect();
    let present: BTreeSet<String> = tables
        .iter()
        .map(|t| table_sampler(t).to_string())
        .collect();
    if let Some(names) = keep.tables {
        let unmatched: Vec<&str> = names
            .iter()
            .map(String::as_str)
            .filter(|n| !tables.iter().any(|t| table_is_under(t, n)))
            .collect();
        if !unmatched.is_empty() {
            return Err(format!(
                "no table named {} in {}; it holds: {}",
                unmatched.join(", "),
                path.display(),
                tables.join(", "),
            )
            .into());
        }
    }
    if let Some(names) = keep.samplers {
        let unmatched: Vec<&str> = names
            .iter()
            .map(String::as_str)
            .filter(|s| !present.contains(*s))
            .collect();
        if !unmatched.is_empty() {
            return Err(format!(
                "no sampler named {} in {}; it holds: {}",
                unmatched.join(", "),
                path.display(),
                present
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>()
                    .join(", "),
            )
            .into());
        }
    }

    let dest = output.unwrap_or(path);
    let staging = super::legacy::staging_dir(dest)?;
    let staged = staging.path().join("filtered.dendro");

    let keep_stream =
        |stream: &str| -> bool { keep.keeps_table(table_of(stream).unwrap_or(stream)) };
    let keep_columns = keep.metrics.map(metriken_storage::KeepMetrics::new);
    let spec = CopySpec {
        keep_streams: Some(&keep_stream),
        keep_columns: keep_columns
            .as_ref()
            .map(|k| k as &dyn dendro::rewrite::ColumnFilter),
        writer_props: Some(metriken_storage::segment_props(
            metriken_storage::default_compression(),
        )),
        ..CopySpec::everything()
    };
    crate::dendro_copy::copy(&src, &staged, &spec)?;
    drop(src);

    // An occupant stream whose table did not survive the projection names
    // occupants of nothing.
    let mut dst = ArchiveMut::open(&staged)?;
    let mut kept = 0usize;
    for source in dst.read_sources()? {
        let streams = dst.all_streams(source.id)?;
        let tables: BTreeSet<&str> = streams
            .iter()
            .map(String::as_str)
            .filter(|s| table_of(s).is_none())
            .collect();
        kept += tables.len();
        let orphan = |stream: &str| table_of(stream).is_some_and(|t| !tables.contains(t));
        if streams.iter().any(|s| orphan(s)) {
            dst.evict_streams_before(source.id, i64::MAX, &orphan)?;
        }
    }
    drop(dst);

    if kept == 0 {
        return Err(format!(
            "filtering {} kept no tables — refusing to overwrite it with an empty archive; \
             check the --metrics / {} names",
            path.display(),
            keep.flag,
        )
        .into());
    }
    // In place, the filtered copy replaces the input, as asked; to a new
    // output, nothing that appeared there meanwhile is replaced.
    if in_place {
        std::fs::rename(&staged, path)?;
    } else {
        super::legacy::publish(&staged, dest)?;
    }
    println!(
        "Filtered {:?}: kept {} of {} tables",
        dest,
        kept,
        tables.len()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `--samplers` keeps a table by its name before the first `/`;
    /// `--tables` keeps a table by its whole name or a `/`-prefix of it, so
    /// it can select one group of a sampler. Given both, a table either
    /// keeps is kept.
    #[test]
    fn tables_select_by_name_or_prefix_and_samplers_by_sampler() {
        let names = |n: &[&str]| n.iter().map(|s| s.to_string()).collect::<BTreeSet<_>>();
        let (cpu, cpu_usage) = (names(&["cpu"]), names(&["cpu/usage"]));
        fn by<'a>(
            samplers: Option<&'a BTreeSet<String>>,
            tables: Option<&'a BTreeSet<String>>,
        ) -> Keep<'a> {
            Keep {
                samplers,
                tables,
                metrics: None,
                flag: "--samplers",
            }
        }
        let all = by(None, None);
        assert!(all.keeps_table("cpu/usage") && all.keeps_table("mem"));

        let sampler = by(Some(&cpu), None);
        assert!(sampler.keeps_table("cpu") && sampler.keeps_table("cpu/usage"));
        assert!(!sampler.keeps_table("cpufreq") && !sampler.keeps_table("mem"));
        assert!(!by(Some(&cpu_usage), None).keeps_table("cpu/usage"));

        let table = by(None, Some(&cpu_usage));
        assert!(table.keeps_table("cpu/usage") && table.keeps_table("cpu/usage/x"));
        assert!(!table.keeps_table("cpu") && !table.keeps_table("cpu/usage2"));
        assert!(by(None, Some(&cpu)).keeps_table("cpu/usage"));

        let mem = names(&["mem"]);
        let both = by(Some(&mem), Some(&cpu_usage));
        assert!(both.keeps_table("mem/info") && both.keeps_table("cpu/usage"));
        assert!(!both.keeps_table("cpu/freq"));
    }

    /// A [`Keep`] of `samplers` and `metrics`, spelled as rezolus spells it.
    fn keep<'a>(
        samplers: Option<&'a BTreeSet<String>>,
        metrics: Option<&'a BTreeSet<String>>,
    ) -> Keep<'a> {
        Keep {
            samplers,
            tables: None,
            metrics,
            flag: "--samplers",
        }
    }

    /// A `.rez` is refused, naming the upgrade, and left as it was.
    #[test]
    fn a_legacy_recording_is_refused_with_the_upgrade() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["v3.rez", "v2.rez"] {
            let path = ::rez::fixtures::committed_rez(name, dir.path());
            let before = std::fs::read(&path).unwrap();
            let out = dir.path().join(format!("{name}.dendro"));
            let err = filter(&path, &keep(Some(&set(&["a"])), None), Some(&out), "tool")
                .unwrap_err()
                .to_string();
            assert!(err.contains("tool recording upgrade"), "{name}: {err}");
            assert!(!out.exists(), "{name}");
            assert_eq!(std::fs::read(&path).unwrap(), before, "{name} unchanged");
        }
    }

    fn dendro_streams(path: &Path) -> Vec<String> {
        let db = dendro::archive::Archive::open(path).unwrap();
        let id = db.read_sources().unwrap()[0].id;
        let mut s = db.all_streams(id).unwrap();
        s.sort();
        s
    }

    fn set(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    /// A filter in place replaces the input with the filtered copy, also when
    /// `-o` names the input by another spelling; an `-o` naming another file
    /// that exists is refused before anything is written.
    #[test]
    fn filter_in_place_and_an_existing_output() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("rec.dendro");
        crate::dendro_copy::fixtures::recorded(&src, 10, true);
        let all = dendro_streams(&src);
        assert!(all.len() > 2, "{all:?}");
        let taken = dir.path().join("taken.dendro");
        std::fs::write(&taken, b"theirs").unwrap();
        let err = filter_dendro(&src, &keep(Some(&set(&["threads"])), None), Some(&taken))
            .unwrap_err()
            .to_string();
        assert!(err.contains("already exists"), "{err}");
        assert_eq!(std::fs::read(&taken).unwrap(), b"theirs");
        assert_eq!(dendro_streams(&src), all);

        std::fs::create_dir(dir.path().join("sub")).unwrap();
        let respelled = dir.path().join("sub").join("..").join("rec.dendro");
        filter_dendro(
            &src,
            &keep(Some(&set(&["threads"])), None),
            Some(&respelled),
        )
        .unwrap();
        assert_eq!(
            dendro_streams(&src),
            vec![
                "threads/tasks".to_string(),
                "threads/tasks/occupants".to_string()
            ]
        );
    }

    /// `--samplers` keeps a long table's occupant stream with its table.
    #[test]
    fn filter_dendro_by_sampler_keeps_the_occupant_stream_with_its_table() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("rec.dendro");
        crate::dendro_copy::fixtures::recorded(&src, 10, true);
        let out = dir.path().join("threads.dendro");
        filter_dendro(&src, &keep(Some(&set(&["threads"])), None), Some(&out)).unwrap();
        assert_eq!(
            dendro_streams(&out),
            vec![
                "threads/tasks".to_string(),
                "threads/tasks/occupants".to_string()
            ]
        );
    }

    /// `--metrics` trims columns and keeps a long table long; a table with no
    /// kept metric goes, and so does its occupant stream.
    #[test]
    fn filter_dendro_by_metric_keeps_long_tables_long() {
        use metriken_query::MetricsSource;
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("rec.dendro");
        crate::dendro_copy::fixtures::recorded(&src, 10, true);

        let mem = dir.path().join("mem.dendro");
        filter_dendro(&src, &keep(None, Some(&set(&["mem_free"]))), Some(&mem)).unwrap();
        assert_eq!(dendro_streams(&mem), vec!["memory/meminfo".to_string()]);

        let tasks = dir.path().join("tasks.dendro");
        filter_dendro(&src, &keep(None, Some(&set(&["task_ops"]))), Some(&tasks)).unwrap();
        assert_eq!(
            dendro_streams(&tasks),
            vec![
                "threads/tasks".to_string(),
                "threads/tasks/occupants".to_string()
            ]
        );
        let open = |p: &Path| {
            ::rez::reader::RezReader::open_recordings(
                p,
                metriken_storage::BufferPool::new(64 * 1024 * 1024),
            )
            .unwrap()
            .remove(0)
            .1
        };
        let (a, b) = (open(&src), open(&tasks));
        let q = "sum by (comm) (rate(task_ops[3s]))";
        let (start, end) = a.time_range().unwrap();
        let answer = |r: &::rez::reader::RezReader| {
            let metriken_query::QueryResult::Matrix { result } =
                r.query_range(q, start, end + 1.0, 1.0).unwrap()
            else {
                panic!("a matrix");
            };
            let mut out: Vec<(String, Vec<(f64, f64)>)> = result
                .into_iter()
                .map(|s| (s.metric.get("comm").cloned().unwrap_or_default(), s.values))
                .collect();
            out.sort_by(|x, y| x.0.cmp(&y.0));
            out
        };
        let (x, y) = (answer(&a), answer(&b));
        assert_eq!(x.len(), 2, "two threads, by their labels: {x:?}");
        assert_eq!(x, y, "the kept metric answers the same");
    }

    /// The `.rez` guards hold for a dendro archive: an unknown sampler is
    /// refused, and so is a filter that would keep nothing; the input is
    /// untouched either way.
    #[test]
    fn filter_dendro_refuses_an_unknown_sampler_and_an_empty_result() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("rec.dendro");
        crate::dendro_copy::fixtures::recorded(&src, 4, true);
        let before = std::fs::read(&src).unwrap();
        let err = filter_dendro(&src, &keep(Some(&set(&["nope"])), None), None).unwrap_err();
        assert!(err.to_string().contains("no sampler named nope"), "{err}");
        let err = filter_dendro(&src, &keep(None, Some(&set(&["absent"]))), None).unwrap_err();
        assert!(err.to_string().contains("kept no tables"), "{err}");
        assert_eq!(std::fs::read(&src).unwrap(), before);
    }
}
