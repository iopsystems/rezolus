use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use super::legacy::{self, Container};

/// How `--ab` assigns arms.
#[derive(Clone, Debug, PartialEq, Eq)]
enum AbPlan {
    /// No `--ab`: nothing is stamped, and the archive opens combined.
    Nothing,
    /// Two inputs: each input's recordings take that input's arm.
    Inputs([&'static str; 2]),
    /// One input: the recordings at these positions (0-based, in the
    /// archive's order, as `describe-recording` lists them) take these arms.
    /// For an archive assembled before `--ab` existed, whose two recordings
    /// may carry the same labels.
    Positions([(usize, &'static str); 2]),
}

/// The plan of `--ab baseline=<x> experiment=<y>`. With two inputs, each
/// value names one input: the path as typed, else its file name. With one
/// input, each is the position of one of its recordings.
/// `positions_from` says where the positions come from
/// (`Host::positions_from`), for the refusal of a value that is not one.
fn ab_plan(
    raw: &[String],
    files: &[PathBuf],
    positions_from: &str,
) -> Result<AbPlan, Box<dyn std::error::Error>> {
    if raw.is_empty() {
        return Ok(AbPlan::Nothing);
    }
    let mut pairs: Vec<(&'static str, &str)> = Vec::new();
    for entry in raw {
        let (key, value) = entry
            .split_once('=')
            .ok_or_else(|| format!("--ab entry {entry:?} must be key=value"))?;
        let arm: &'static str = match key {
            "baseline" => "baseline",
            "experiment" => "experiment",
            other => {
                return Err(format!(
                    "--ab key {other:?} not recognized (valid: baseline, experiment)"
                )
                .into());
            }
        };
        if pairs.iter().any(|(a, _)| *a == arm) {
            return Err(format!("--ab {key}= specified more than once").into());
        }
        pairs.push((arm, value));
    }
    if pairs.len() != 2 {
        return Err("--ab requires baseline=<x> and experiment=<y>".into());
    }
    match files.len() {
        1 => {
            let mut out = [(0usize, ""); 2];
            for (slot, (arm, value)) in out.iter_mut().zip(&pairs) {
                let at: usize = value.parse().map_err(|_| {
                    format!(
                        "--ab {arm}={value:?}: with one input, name a recording by its \
                         position (0, 1, ...) {positions_from}"
                    )
                })?;
                *slot = (at, *arm);
            }
            if out[0].0 == out[1].0 {
                return Err(format!("--ab names recording {} as both arms", out[0].0).into());
            }
            Ok(AbPlan::Positions(out))
        }
        2 => {
            let mut arms: [Option<&'static str>; 2] = [None, None];
            for (arm, value) in &pairs {
                // The path as typed first, then a file name, so `a.rez` beside
                // `z/a.rez` names the one typed `a.rez`.
                let typed: Vec<usize> =
                    (0..2).filter(|i| files[*i].as_os_str() == *value).collect();
                let hits = if typed.is_empty() {
                    (0..2)
                        .filter(|i| files[*i].file_name().is_some_and(|n| n == *value))
                        .collect()
                } else {
                    typed
                };
                let at = match hits.as_slice() {
                    [at] => *at,
                    [] => {
                        return Err(format!(
                            "--ab {arm}={value:?} names neither input ({} or {}); --ab \
                             names an input by its path or file name (a source name, which \
                             the 5.x A/B tarball took, is no longer accepted)",
                            files[0].display(),
                            files[1].display()
                        )
                        .into());
                    }
                    _ => {
                        return Err(format!(
                            "--ab {arm}={value:?} names both inputs; give the path as typed"
                        )
                        .into());
                    }
                };
                if let Some(other) = arms[at] {
                    return Err(format!(
                        "--ab names {} as both {other} and {arm}",
                        files[at].display()
                    )
                    .into());
                }
                arms[at] = Some(arm);
            }
            Ok(AbPlan::Inputs([
                arms[0].expect("two distinct inputs named"),
                arms[1].expect("two distinct inputs named"),
            ]))
        }
        n => Err(format!(
            "--ab takes two inputs, one per arm, or one archive and two of its \
             recordings' positions (got {n} inputs)"
        )
        .into()),
    }
}

/// A recording's arm and the input it came from.
type ArmOf<'a> = (&'static str, &'a PathBuf);

impl AbPlan {
    /// The arm each source of the output takes, and the input it came
    /// from: `counts` is how many sources each input contributed, in the
    /// order they were copied. `groups` is the one input's recordings as
    /// `rezolus mcp describe-recording` lists them (its read groups, each by
    /// the catalog positions of its sources), which `--ab baseline=0
    /// experiment=1` positions index; every source of a named recording
    /// takes its arm.
    fn per_recording<'a>(
        &self,
        counts: &[usize],
        files: &'a [PathBuf],
        groups: &[Vec<usize>],
    ) -> Result<Vec<Option<ArmOf<'a>>>, Box<dyn std::error::Error>> {
        let total: usize = counts.iter().sum();
        match self {
            AbPlan::Nothing => Ok(vec![None; total]),
            AbPlan::Inputs(arms) => Ok(counts
                .iter()
                .zip(arms.iter().zip(files))
                .flat_map(|(n, (arm, file))| std::iter::repeat_n(Some((*arm, file)), *n))
                .collect()),
            AbPlan::Positions(picks) => {
                let mut out = vec![None; total];
                for (at, arm) in picks {
                    let group = groups.get(*at).ok_or_else(|| {
                        format!(
                            "--ab {arm}={at}: {} holds {} recording(s), numbered from 0",
                            files[0].display(),
                            groups.len()
                        )
                    })?;
                    for &source in group {
                        let slot = out.get_mut(source).ok_or_else(|| {
                            format!("--ab {arm}={at}: source {source} is not in the output")
                        })?;
                        *slot = Some((*arm, &files[0]));
                    }
                }
                Ok(out)
            }
        }
    }
}

/// A recording's labels.
type Labels = BTreeMap<String, String>;

/// Each recording's labels with its `arm` added (`per` is
/// [`AbPlan::per_recording`]), `None` for one left as it is: no arm to give,
/// or the same arm already. A recording that carries another arm is refused,
/// naming its input; `--ab` never replaces one.
fn stamped_labels(
    labels: &[Labels],
    per: &[Option<ArmOf<'_>>],
) -> Result<Vec<Option<Labels>>, Box<dyn std::error::Error>> {
    if labels.len() != per.len() {
        return Err(format!(
            "the output holds {} recording(s) but {} were copied",
            labels.len(),
            per.len()
        )
        .into());
    }
    labels
        .iter()
        .zip(per)
        .map(|(l, p)| {
            let Some((arm, file)) = p else {
                return Ok(None);
            };
            match l.get(::dashboard::combined::ARM_LABEL).map(String::as_str) {
                Some(existing) if existing.eq_ignore_ascii_case(arm) => Ok(None),
                Some(existing) => Err(format!(
                    "{}: its recording {} is already arm {existing}; --ab would make it {arm} \
                     (it never replaces an arm)",
                    file.display(),
                    crate::parquet_tools::select::render_labels(l)
                )
                .into()),
                None => {
                    let mut l = l.clone();
                    l.insert(
                        ::dashboard::combined::ARM_LABEL.to_string(),
                        arm.to_string(),
                    );
                    Ok(Some(l))
                }
            }
        })
        .collect()
}

/// Refuse a plan an input's recordings already contradict, before anything
/// is copied: each input's source labels, read from the dendro archive it is
/// copied from (`inputs`, the converted ones for a legacy input), named by
/// the input as given (`files`). Whether the arms fill a comparison is
/// checked after the copy (`check_slots`), when the output's recordings are
/// known: a source and its copy in two inputs are one recording.
fn check_ab_plan(
    plan: &AbPlan,
    files: &[PathBuf],
    inputs: &[PathBuf],
) -> Result<(), Box<dyn std::error::Error>> {
    if *plan == AbPlan::Nothing {
        return Ok(());
    }
    let mut labels = Vec::new();
    let mut counts = Vec::new();
    for input in inputs {
        let sources = dendro::archive::Archive::open(input)?.read_sources()?;
        counts.push(sources.len());
        labels.extend(sources.into_iter().map(|s| s.meta.labels));
    }
    let groups = match plan {
        AbPlan::Positions(_) => read_groups_of(&inputs[0])?,
        _ => Vec::new(),
    };
    stamped_labels(&labels, &plan.per_recording(&counts, files, &groups)?)?;
    Ok(())
}

/// How many of `groups` (recordings, each by its sources' positions) hold a
/// source in `touched`: what "stamped arm on N recording(s)" counts.
fn recordings_touched(groups: &[Vec<usize>], touched: &BTreeSet<usize>) -> usize {
    groups
        .iter()
        .filter(|g| g.iter().any(|i| touched.contains(i)))
        .count()
}

/// The recordings of the dendro archive at `path` as every reader presents
/// them (`RezReader::open_recordings`, one per read group), each by the
/// catalog positions of its sources. The readers are dropped before this
/// returns, so a writer can open the archive next.
fn read_groups_of(path: &Path) -> Result<Vec<Vec<usize>>, Box<dyn std::error::Error>> {
    let readers = ::rez::reader::RezReader::open_recordings(
        path,
        metriken_storage::BufferPool::new(16 * 1024 * 1024),
    )?;
    Ok(readers
        .iter()
        .map(|(_, r)| r.sources().iter().map(|s| s.catalog_index).collect())
        .collect())
}

/// Whether the labels an assembly ends with (`stamped`, else the
/// recording's own) fill a comparison's two slots
/// (`dashboard::combined::arm_slots`). With `--ab`, an error when they do
/// not, since the output would not open as the comparison asked for;
/// without, a warning when the inputs' own arms would be refused.
fn check_slots(
    viewer_refuses: &str,
    labels: &[Labels],
    stamped: &[Option<Labels>],
    plan: &AbPlan,
    groups: &[Vec<usize>],
) -> Result<(), Box<dyn std::error::Error>> {
    // One label set per recording: the sources of a read group are one
    // recording unless their final labels differ.
    let mut last: Vec<Labels> = Vec::new();
    for group in groups {
        let mut seen = std::collections::BTreeSet::new();
        for &i in group {
            let l = stamped[i].clone().unwrap_or_else(|| labels[i].clone());
            if seen.insert(l.clone()) {
                last.push(l);
            }
        }
    }
    if let Err(why) = ::dashboard::combined::arm_slots(&last) {
        if *plan == AbPlan::Nothing {
            eprintln!(
                "warning: the assembled recordings' arms do not fill a comparison ({why}); \
                 {viewer_refuses}"
            );
        } else {
            return Err(format!(
                "with these arms the output would not open as a comparison: {why}"
            )
            .into());
        }
    }
    Ok(())
}

#[cfg(test)]
thread_local! {
    /// Fail the relabel after the copy, for a test of what a late failure
    /// leaves behind.
    static FAIL_STAMP: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// Write this file's bytes at the output just before publishing, for a
    /// test of an output that appears while the archive is assembled.
    static APPEAR: std::cell::RefCell<Option<Vec<u8>>> = const { std::cell::RefCell::new(None) };
    /// The lines combine printed on this thread.
    static SAID: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// `println!`, and under test also kept for the test to read (`SAID`).
macro_rules! say {
    ($($arg:tt)*) => {{
        let line = format!($($arg)*);
        #[cfg(test)]
        SAID.with(|s| s.borrow_mut().push(line.clone()));
        println!("{line}");
    }};
}

pub(super) fn run(
    host: &super::Host,
    args: &super::cli::CombineArgs,
) -> Result<(), Box<dyn std::error::Error>> {
    let files = &args.files;
    let output = &args.output;
    let ab_raw = &args.ab;
    for (given, flag) in [
        (args.bypass_time_check, "--bypass-time-check"),
        (args.pinned.is_some(), "--pinned"),
        (args.category.is_some(), "--category"),
    ] {
        if given {
            return Err(format!(
                "{flag} applied to the parquet row-merge and the A/B tarball, which 6.x \
                 does not write; combine writes one .dendro archive (-o <name>.dendro)"
            )
            .into());
        }
    }
    if output.extension().and_then(|e| e.to_str()) != Some("dendro") {
        return Err(format!(
            "{}: combine writes .dendro archives only in 6.x (the parquet row-merge, the \
             .parquet.ab.tar and .rez outputs are gone); give -o {}",
            output.display(),
            legacy::dendro_beside(output).display()
        )
        .into());
    }
    // One input only relabels an archive's recordings (`--ab baseline=0
    // experiment=1`); everything else combines two or more.
    if files.len() < 2 && ab_raw.is_empty() {
        return Err("combine takes two or more inputs (or one archive with --ab)".into());
    }
    legacy::check_new_dendro(output)?;
    let plan = ab_plan(ab_raw, files, &host.positions_from())?;
    combine_dendro(&host.viewer_refuses(), files, output, &plan)
}

/// Assemble one dendro archive from `files`, a source per input recording,
/// in one transaction: the output holds every input or does not exist.
///
/// A `.rez` or parquet input is converted first into the staging directory,
/// as `recording upgrade` converts it. Each input is then copied through
/// `copy_sources_into` in its own read snapshot, its live tail sealed
/// (`crate::dendro_copy`), and each source keeps its uuid, so a source and a
/// copy of it (the same uuid, e.g. a file and its snapshot) stay one
/// recording to every reader (metriken-storage's process groups).
fn combine_dendro(
    viewer_refuses: &str,
    files: &[PathBuf],
    output: &Path,
    plan: &AbPlan,
) -> Result<(), Box<dyn std::error::Error>> {
    use dendro::archive::{Archive, ArchiveMut};
    use dendro::rewrite::{copy_sources_into, CopySpec};

    // Beside the output, never in $TMPDIR: a link across filesystems fails.
    let staging = legacy::staging_dir(output)?;

    let mut inputs: Vec<PathBuf> = Vec::with_capacity(files.len());
    let mut converted = 0usize;
    for (i, file) in files.iter().enumerate() {
        match Container::of(file) {
            Container::Dendro => inputs.push(file.clone()),
            c if c.is_legacy() => {
                let staged = staging.path().join(format!("converted-{i}.dendro"));
                legacy::convert(file, &staged)?;
                converted += 1;
                inputs.push(staged);
            }
            c => {
                return Err(format!(
                    "{} is a {}, not a recording (.dendro, .rez or .parquet)",
                    file.display(),
                    c.name()
                )
                .into())
            }
        }
    }
    if converted > 0 {
        say!("converted {converted} .rez or parquet input(s) to dendro for the assembly");
    }
    check_ab_plan(plan, files, &inputs)?;

    let archives: Vec<Archive> = inputs
        .iter()
        .map(|p| Archive::open(p))
        .collect::<Result<_, _>>()?;
    let mut sources = 0usize;
    let mut counts: Vec<usize> = Vec::with_capacity(archives.len());

    let staged = staging.path().join("combined.dendro");
    let mut dst = ArchiveMut::create(&staged)?;
    dst.transaction(|tx| {
        for src in &archives {
            // The encoder is built in the snapshot the copy reads, so an
            // input still being recorded cannot gain a long stream between
            // the two.
            let n = src.read_snapshot(|src| {
                let encoder = metriken_storage::writer::Encoder::for_archive(src)?;
                copy_sources_into(src, tx, &CopySpec::everything(), &encoder)
            })?;
            counts.push(n);
            sources += n;
        }
        Ok(())
    })?;
    // Stamped on the staged copy, so a refusal leaves no output.
    #[cfg(test)]
    if *plan != AbPlan::Nothing && FAIL_STAMP.with(|f| f.get()) {
        return Err("injected relabel failure".into());
    }
    drop(dst);
    drop(archives);
    // The output's recordings as the readers present them: what --ab
    // positions name, and what fills a comparison's slots.
    let groups = read_groups_of(&staged)?;
    let mut dst = ArchiveMut::open(&staged)?;
    let mut stamped_sources = BTreeSet::new();
    {
        let rows = dst.read_sources()?;
        let labels: Vec<_> = rows.iter().map(|r| r.meta.labels.clone()).collect();
        let per = plan.per_recording(&counts, files, &groups)?;
        let new_labels = stamped_labels(&labels, &per)?;
        check_slots(viewer_refuses, &labels, &new_labels, plan, &groups)?;
        for (i, (row, new)) in rows.iter().zip(new_labels).enumerate() {
            if let Some(new) = new {
                dst.update_source_labels(row.id, &new)?;
                stamped_sources.insert(i);
            }
        }
    }
    drop(dst);
    // Read again after the relabel, which can split a recording (a source
    // and its copy given different arms). The staged file's sidecars stay
    // in the staging directory.
    let final_groups = read_groups_of(&staged)?;
    let recordings = final_groups.len();
    let stamped_recordings = recordings_touched(&final_groups, &stamped_sources);
    #[cfg(test)]
    if let Some(bytes) = APPEAR.with(|a| a.borrow_mut().take()) {
        std::fs::write(output, bytes)?;
    }
    legacy::publish(&staged, output)?;
    report_arms(stamped_recordings, files, plan);
    if recordings == sources {
        say!(
            "wrote {} with {} recording(s) from {} input(s)",
            output.display(),
            recordings,
            files.len()
        );
    } else {
        say!(
            "wrote {} with {} recording(s) ({} sources) from {} input(s)",
            output.display(),
            recordings,
            sources,
            files.len()
        );
    }
    Ok(())
}

/// Say which input or recording became which arm, when `--ab` was given.
fn report_arms(stamped: usize, files: &[PathBuf], plan: &AbPlan) {
    match plan {
        AbPlan::Nothing => return,
        AbPlan::Inputs(arms) => {
            for (file, arm) in files.iter().zip(arms) {
                say!("{}: arm={arm}", file.display());
            }
        }
        AbPlan::Positions(picks) => {
            for (at, arm) in picks {
                say!("{} recording {at}: arm={arm}", files[0].display());
            }
        }
    }
    say!("stamped arm on {stamped} recording(s); the archive opens as an A/B comparison");
}

/// [`run`] for `files` into `output` with no `--ab`, for other modules'
/// tests.
#[cfg(test)]
pub(crate) fn run_combine_for_test(
    files: &[PathBuf],
    output: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    combine_dendro(
        "`rezolus view` refuses it without --baseline/--experiment",
        files,
        output,
        &AbPlan::Nothing,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::dashboard::keys::{KEY_SAMPLING_INTERVAL_MS, KEY_SOURCE};
    use arrow::array::{Int64Array, UInt64Array};
    use arrow::datatypes::{DataType, Field, Schema};
    use arrow::record_batch::RecordBatch;
    use parquet::arrow::ArrowWriter;
    use parquet::file::metadata::KeyValue;
    use parquet::file::properties::WriterProperties;
    use std::collections::HashMap;
    use std::sync::Arc;
    use tempfile::NamedTempFile;

    /// 1 second in nanoseconds — use as a multiplier so test timestamps
    /// are at a realistic scale relative to the sampling interval.
    const SEC: u64 = 1_000_000_000;

    /// A parquet recording as a 5.x recorder wrote one: a timestamp column, a
    /// duration column, and one gauge metric column.
    fn make_test_file(
        timestamps: &[u64],
        metric_name: &str,
        metric_values: &[Option<i64>],
        source: &str,
        sampling_interval_ms: &str,
    ) -> (NamedTempFile, PathBuf) {
        let ts_field =
            Field::new("timestamp", DataType::UInt64, false).with_metadata(HashMap::from([
                ("metric_type".to_string(), "timestamp".to_string()),
                ("unit".to_string(), "nanoseconds".to_string()),
            ]));
        let dur_field =
            Field::new("duration", DataType::UInt64, true).with_metadata(HashMap::from([
                ("metric_type".to_string(), "duration".to_string()),
                ("unit".to_string(), "nanoseconds".to_string()),
            ]));
        let metric_field = Field::new(metric_name, DataType::Int64, true).with_metadata(
            HashMap::from([("metric_type".to_string(), "gauge".to_string())]),
        );

        let schema = Arc::new(Schema::new(vec![ts_field, dur_field, metric_field]));

        let ts_array = UInt64Array::from(timestamps.to_vec());
        let dur_array = UInt64Array::from(vec![None::<u64>; timestamps.len()]);
        let metric_array = Int64Array::from(metric_values.to_vec());

        let batch = RecordBatch::try_new(
            schema.clone(),
            vec![
                Arc::new(ts_array),
                Arc::new(dur_array),
                Arc::new(metric_array),
            ],
        )
        .unwrap();

        let kv = vec![
            KeyValue {
                key: KEY_SOURCE.to_string(),
                value: Some(source.to_string()),
            },
            KeyValue {
                key: KEY_SAMPLING_INTERVAL_MS.to_string(),
                value: Some(sampling_interval_ms.to_string()),
            },
        ];
        let props = WriterProperties::builder()
            .set_key_value_metadata(Some(kv))
            .build();

        let tmp = NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();
        let file = std::fs::File::create(&path).unwrap();
        let mut writer = ArrowWriter::try_new(file, schema, Some(props)).unwrap();
        writer.write(&batch).unwrap();
        writer.close().unwrap();

        (tmp, path)
    }

    /// Parse `args` as the CLI does and run the combine they name.
    fn combine_args(args: Vec<String>) -> Result<(), Box<dyn std::error::Error>> {
        use crate::parquet_tools::{test_support, Recording};
        let Recording::Combine(args) = test_support::parse(args) else {
            panic!("not a combine");
        };
        run(&test_support::host(), &args)
    }

    /// Build the arguments of `parquet combine <FILES...> -o <output>` and
    /// run them, exactly as the CLI does.
    fn run_combine(
        files: &[PathBuf],
        output: &std::path::Path,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut args: Vec<String> = vec!["parquet".to_string(), "combine".to_string()];
        args.extend(files.iter().map(|f| f.to_string_lossy().into_owned()));
        args.push("-o".to_string());
        args.push(output.to_string_lossy().into_owned());
        combine_args(args)
    }

    /// `run_combine` with `--ab` and its values after the inputs.
    fn run_combine_ab(
        files: &[PathBuf],
        output: &std::path::Path,
        ab: &[&str],
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut args: Vec<String> = vec!["parquet".to_string(), "combine".to_string()];
        args.extend(files.iter().map(|f| f.to_string_lossy().into_owned()));
        args.push("-o".to_string());
        args.push(output.to_string_lossy().into_owned());
        if !ab.is_empty() {
            args.push("--ab".to_string());
            args.extend(ab.iter().map(|s| s.to_string()));
        }
        combine_args(args)
    }

    /// A one-recording `.rez` labelled `labels`.
    fn agent_rez(path: &std::path::Path, labels: &[(&str, &str)]) {
        rez::fixtures::rez_with_identity(path, labels, &[("source", "rezolus")]);
    }

    /// The labels of an archive's recordings, in catalog order.
    fn archive_labels(path: &std::path::Path) -> Vec<BTreeMap<String, String>> {
        ::rez::reader::RezReader::open_recordings(
            path,
            metriken_storage::BufferPool::new(16 * 1024 * 1024),
        )
        .unwrap()
        .into_iter()
        .map(|(l, _)| l)
        .collect()
    }

    /// `--ab` stamps `arm` on each input's recordings, from `.rez` and
    /// parquet inputs, and the archive opens as a comparison; without it
    /// nothing is stamped and it opens combined.
    #[test]
    fn combine_ab_stamps_each_inputs_arm() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a.rez"), dir.path().join("b.rez"));
        agent_rez(&a, &[("source", "rezolus"), ("host", "a")]);
        agent_rez(&b, &[("source", "rezolus"), ("host", "b")]);
        let arms = |path: &std::path::Path| -> Vec<(String, Option<String>)> {
            archive_labels(path)
                .into_iter()
                .map(|l| (l["host"].clone(), l.get("arm").cloned()))
                .collect()
        };
        let want = vec![
            ("a".to_string(), Some("baseline".to_string())),
            ("b".to_string(), Some("experiment".to_string())),
        ];

        let ab = dir.path().join("ab.dendro");
        // One input named by its file name, the other by the path as typed.
        let exp = format!("experiment={}", b.display());
        run_combine_ab(&[a.clone(), b.clone()], &ab, &["baseline=a.rez", &exp]).unwrap();
        assert_eq!(arms(&ab), want);
        let readers = ::rez::reader::RezReader::open_recordings(
            &ab,
            metriken_storage::BufferPool::new(16 * 1024 * 1024),
        )
        .unwrap();
        assert!(!crate::viewer::opens_combined(&readers, false));

        let plain = dir.path().join("plain.dendro");
        run_combine_ab(&[a.clone(), b.clone()], &plain, &[]).unwrap();
        assert_eq!(
            arms(&plain)
                .into_iter()
                .map(|(_, arm)| arm)
                .collect::<Vec<_>>(),
            [None, None]
        );
        let readers = ::rez::reader::RezReader::open_recordings(
            &plain,
            metriken_storage::BufferPool::new(16 * 1024 * 1024),
        )
        .unwrap();
        assert!(crate::viewer::opens_combined(&readers, false));
    }

    /// Parquet inputs, one recording each, take their arms.
    #[test]
    fn combine_ab_stamps_parquet_inputs() {
        let (_t1, p1) = make_test_file(
            &[100 * SEC, 200 * SEC],
            "m1",
            &[Some(1), Some(2)],
            "redis",
            "1000",
        );
        let (_t2, p2) = make_test_file(
            &[100 * SEC, 200 * SEC],
            "m2",
            &[Some(3), Some(4)],
            "valkey",
            "1000",
        );
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("ab.dendro");
        let base = format!("baseline={}", p2.display());
        let exp = format!("experiment={}", p1.display());
        run_combine_ab(&[p1, p2], &out, &[&base, &exp]).unwrap();
        let arms: BTreeMap<String, String> = archive_labels(&out)
            .into_iter()
            .map(|l| (l["source"].clone(), l["arm"].clone()))
            .collect();
        assert_eq!(arms["redis"], "experiment");
        assert_eq!(arms["valkey"], "baseline");
    }

    /// An input recording already carrying the arm it would be given keeps
    /// it; one carrying the other arm is refused and leaves no output.
    #[test]
    fn combine_ab_refuses_a_conflicting_arm() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a.rez"), dir.path().join("b.rez"));
        agent_rez(
            &a,
            &[("source", "rezolus"), ("host", "a"), ("arm", "baseline")],
        );
        agent_rez(
            &b,
            &[("source", "rezolus"), ("host", "b"), ("arm", "baseline")],
        );
        let kept = dir.path().join("kept.dendro");
        run_combine_ab(
            &[a.clone(), b.clone()],
            &kept,
            &["baseline=a.rez", "experiment=c.rez"],
        )
        .unwrap_err();
        assert!(!kept.exists());
        let out = dir.path().join("out.dendro");
        let err = run_combine_ab(
            &[a.clone(), b.clone()],
            &out,
            &["baseline=a.rez", "experiment=b.rez"],
        )
        .unwrap_err()
        .to_string();
        assert!(
            err.contains("b.rez") && err.contains("already arm baseline"),
            "{err}"
        );
        // Refused before the copy: the relabel after it is never reached.
        FAIL_STAMP.with(|f| f.set(true));
        let early = run_combine_ab(
            &[a.clone(), b.clone()],
            &dir.path().join("early.dendro"),
            &["baseline=a.rez", "experiment=b.rez"],
        );
        FAIL_STAMP.with(|f| f.set(false));
        let early = early.unwrap_err().to_string();
        assert!(early.contains("already arm baseline"), "{early}");
        assert!(!out.exists(), "a refusal leaves no output");
        let swapped = dir.path().join("swapped.dendro");
        run_combine_ab(
            &[b.clone(), a.clone()],
            &swapped,
            &["baseline=a.rez", "experiment=b.rez"],
        )
        .unwrap_err();
        // The matching arm is kept.
        let c = dir.path().join("c.rez");
        agent_rez(&c, &[("source", "rezolus"), ("host", "c")]);
        let ok = dir.path().join("ok.dendro");
        run_combine_ab(
            &[a.clone(), c.clone()],
            &ok,
            &["baseline=a.rez", "experiment=c.rez"],
        )
        .unwrap();
        let arms: Vec<_> = archive_labels(&ok)
            .into_iter()
            .map(|l| l["arm"].clone())
            .collect();
        assert_eq!(arms, ["baseline", "experiment"]);
    }

    /// An archive assembled without `--ab` (as every `combine` before it
    /// did) whose two recordings carry the same labels: no selector tells
    /// them apart, so `--ab` names them by position, and the result opens as
    /// a comparison with the named recording as the baseline.
    #[test]
    fn combine_ab_relabels_one_archive_by_position() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a.rez"), dir.path().join("b.rez"));
        rez::fixtures::rez_with_identity(
            &a,
            &[("source", "rezolus")],
            &[("source", "rezolus"), ("version", "first")],
        );
        rez::fixtures::rez_with_identity(
            &b,
            &[("source", "rezolus")],
            &[("source", "rezolus"), ("version", "second")],
        );
        let old = dir.path().join("old.dendro");
        run_combine_ab(&[a, b], &old, &[]).unwrap();
        let labels = archive_labels(&old);
        assert_eq!(labels[0], labels[1], "twins: {labels:?}");

        let out = dir.path().join("new.dendro");
        run_combine_ab(
            std::slice::from_ref(&old),
            &out,
            &["baseline=1", "experiment=0"],
        )
        .unwrap();
        let readers = ::rez::reader::RezReader::open_recordings(
            &out,
            metriken_storage::BufferPool::new(16 * 1024 * 1024),
        )
        .unwrap();
        let arms: Vec<(String, String)> = readers
            .iter()
            .map(|(l, r)| {
                use metriken_query::MetricsSource;
                (r.version(), l["arm"].clone())
            })
            .collect();
        assert_eq!(
            arms,
            [
                ("first".to_string(), "experiment".to_string()),
                ("second".to_string(), "baseline".to_string()),
            ],
            "{}",
            out.display()
        );
        assert!(!crate::viewer::opens_combined(&readers, false));
        let err = run_combine_ab(
            std::slice::from_ref(&old),
            &dir.path().join("x.dendro"),
            &["baseline=2", "experiment=0"],
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("holds 2 recording(s)"), "{err}");
    }

    /// An input arm that differs from the one `--ab` gives only in case is
    /// kept, not refused; a plan whose result has more arms than slots is
    /// refused before anything is written.
    #[test]
    fn combine_ab_keeps_a_case_variant_and_refuses_three_arms() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a.rez"), dir.path().join("b.rez"));
        agent_rez(
            &a,
            &[("source", "rezolus"), ("host", "a"), ("arm", "Baseline")],
        );
        agent_rez(&b, &[("source", "rezolus"), ("host", "b")]);
        let out = dir.path().join("ab.dendro");
        run_combine_ab(
            &[a.clone(), b.clone()],
            &out,
            &["baseline=a.rez", "experiment=b.rez"],
        )
        .unwrap();
        let arms: Vec<_> = archive_labels(&out)
            .into_iter()
            .map(|l| l["arm"].clone())
            .collect();
        assert_eq!(arms, ["Baseline", "experiment"]);

        let three = dir.path().join("three.dendro");
        let c = dir.path().join("c.rez");
        agent_rez(
            &c,
            &[("source", "rezolus"), ("host", "c"), ("arm", "control")],
        );
        let (d, e) = (dir.path().join("d.rez"), dir.path().join("e.rez"));
        agent_rez(&d, &[("source", "rezolus"), ("host", "d")]);
        agent_rez(&e, &[("source", "rezolus"), ("host", "e")]);
        run_combine_ab(&[d, e, c.clone()], &three, &[]).unwrap();
        let err = run_combine_ab(
            std::slice::from_ref(&three),
            &dir.path().join("x.dendro"),
            &["baseline=0", "experiment=1"],
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("would not open as a comparison"), "{err}");
        assert!(
            !dir.path().join("x.dendro").exists(),
            "no .dendro is written"
        );
    }

    /// A failure after the copy (here, the relabel) leaves nothing at the
    /// output, so the same command runs again.
    #[test]
    fn a_failed_relabel_leaves_no_output() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a.rez"), dir.path().join("b.rez"));
        agent_rez(&a, &[("source", "rezolus"), ("host", "a")]);
        agent_rez(&b, &[("source", "rezolus"), ("host", "b")]);
        let out = dir.path().join("ab.dendro");
        let ab = ["baseline=a.rez", "experiment=b.rez"];
        FAIL_STAMP.with(|f| f.set(true));
        let err = run_combine_ab(&[a.clone(), b.clone()], &out, &ab);
        FAIL_STAMP.with(|f| f.set(false));
        assert!(err.unwrap_err().to_string().contains("injected"));
        assert!(!out.exists());
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(leftovers.len(), 2, "{leftovers:?}");
        run_combine_ab(&[a, b], &out, &ab).unwrap();
        assert!(out.exists());
    }

    #[test]
    fn combine_ab_takes_two_named_inputs_or_two_positions() {
        let plan = |raw: &[&str], files: &[&str]| {
            ab_plan(
                &raw.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
                &files.iter().map(PathBuf::from).collect::<Vec<_>>(),
                "as `rezolus mcp describe-recording` lists them",
            )
        };
        assert_eq!(
            plan(
                &["experiment=x/a.rez", "baseline=b.rez"],
                &["x/a.rez", "y/b.rez"]
            )
            .unwrap(),
            AbPlan::Inputs(["experiment", "baseline"])
        );
        // The path as typed wins over another input's file name.
        assert_eq!(
            plan(
                &["baseline=a.parquet", "experiment=z/a.parquet"],
                &["z/a.parquet", "a.parquet"]
            )
            .unwrap(),
            AbPlan::Inputs(["experiment", "baseline"])
        );
        assert_eq!(
            plan(&["baseline=1", "experiment=0"], &["old.rez"]).unwrap(),
            AbPlan::Positions([(1, "baseline"), (0, "experiment")])
        );
        assert_eq!(plan(&[], &["a", "b", "c"]).unwrap(), AbPlan::Nothing);
        for (raw, files) in [
            (&["baseline=a", "experiment=b"][..], &["a", "b", "c"][..]),
            (&["baseline=a"][..], &["a", "b"][..]),
            (&["baseline=a", "experiment=a"][..], &["a", "b"][..]),
            (&["baseline=a", "experiment=z"][..], &["a", "b"][..]),
            (&["control=a", "experiment=b"][..], &["a", "b"][..]),
            (
                &["baseline=a.rez", "experiment=b"][..],
                &["x/a.rez", "y/a.rez"][..],
            ),
            (&["baseline=0", "experiment=0"][..], &["old.rez"][..]),
            (&["baseline=first", "experiment=1"][..], &["old.rez"][..]),
        ] {
            assert!(plan(raw, files).is_err(), "{raw:?} {files:?}");
        }
    }

    /// `.rez` inputs of both containers are converted on the way in, and
    /// the assembled archive is readable, a recording per input, each
    /// holding its tables' rows.
    #[test]
    fn combine_converts_rez_inputs_into_one_readable_archive() {
        let dir = tempfile::tempdir().unwrap();
        let v3 = rez::fixtures::committed_rez("v3.rez", dir.path());
        let v2 = rez::fixtures::committed_rez("v2.rez", dir.path());
        let out = dir.path().join("out.dendro");
        run_combine(&[v3.clone(), v2.clone()], &out).unwrap();

        let db = dendro::archive::Archive::open(&out).unwrap();
        let sources = db.read_sources().unwrap();
        assert_eq!(sources.len(), 2, "one source per input recording");
        for s in &sources {
            assert!(s.complete, "an assembled archive is finished");
            let mut streams = db.all_streams(s.id).unwrap();
            streams.sort();
            assert_eq!(streams, ["a", "b"]);
            for stream in &streams {
                assert!(db.total_rows(s.id, stream).unwrap() > 0, "{stream}");
            }
        }
        drop(db);
        let readers = ::rez::reader::RezReader::open_recordings(
            &out,
            metriken_storage::BufferPool::new(64 * 1024 * 1024),
        )
        .unwrap();
        assert_eq!(readers.len(), 2, "both recordings are readable");
        let committed = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("crates/rez/tests/fixtures/rez/v3.rez");
        assert_eq!(
            std::fs::read(&v3).unwrap(),
            std::fs::read(committed).unwrap(),
            "the input is not changed"
        );
    }

    /// Parquet inputs become a recording each, labelled with each file's
    /// `source` as the viewer labels a parquet file.
    #[test]
    fn combine_parquet_inputs_into_a_multi_recording_archive() {
        let (_t1, p1) = make_test_file(
            &[100 * SEC, 200 * SEC],
            "m1",
            &[Some(1), Some(2)],
            "redis",
            "1000",
        );
        let (_t2, p2) = make_test_file(
            &[100 * SEC, 200 * SEC],
            "m2",
            &[Some(3), Some(4)],
            "valkey",
            "1000",
        );
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("ab.dendro");
        run_combine(&[p1, p2], &out).unwrap();
        let sources: std::collections::BTreeSet<String> = archive_labels(&out)
            .into_iter()
            .filter_map(|l| l.get("source").cloned())
            .collect();
        assert_eq!(
            sources,
            ["redis".to_string(), "valkey".to_string()]
                .into_iter()
                .collect()
        );
    }

    /// 6.x writes `.dendro` only: a `.rez`, `.parquet` or `.parquet.ab.tar`
    /// output is refused, naming the `.dendro` to give, as are the row-merge
    /// flags and an existing output; nothing is written.
    #[test]
    fn combine_refuses_every_output_but_a_new_dendro() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a.dendro"), dir.path().join("b.dendro"));
        crate::dendro_copy::fixtures::recorded(&a, 4, true);
        crate::dendro_copy::fixtures::recorded(&b, 4, true);
        for name in ["out.rez", "out.parquet", "out.parquet.ab.tar"] {
            let out = dir.path().join(name);
            let err = run_combine(&[a.clone(), b.clone()], &out)
                .unwrap_err()
                .to_string();
            assert!(err.contains(".dendro archives only"), "{name}: {err}");
            assert!(!out.exists(), "{name}");
        }
        let out = dir.path().join("out.dendro");
        for flags in [&["--pinned", "x"][..], &["--bypass-time-check"][..]] {
            let mut args: Vec<String> = vec!["parquet".into(), "combine".into()];
            args.extend(
                [a.clone(), b.clone()]
                    .iter()
                    .map(|f| f.display().to_string()),
            );
            args.extend(["-o".to_string(), out.display().to_string()]);
            args.extend(flags.iter().map(|s| s.to_string()));
            let err = combine_args(args).unwrap_err().to_string();
            assert!(err.contains("does not write"), "{flags:?}: {err}");
        }
        std::fs::write(&out, b"existing").unwrap();
        let err = run_combine(&[a, b], &out).unwrap_err().to_string();
        assert!(err.contains("already exists"), "{err}");
        assert_eq!(std::fs::read(&out).unwrap(), b"existing");
    }

    /// Two dendro archives become one with a source each, still long and
    /// readable, each recording answering as it did alone.
    #[test]
    fn combine_assembles_dendro_archives() {
        use metriken_query::MetricsSource;
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a.dendro"), dir.path().join("b.dendro"));
        crate::dendro_copy::fixtures::recorded(&a, 8, true);
        crate::dendro_copy::fixtures::recorded(&b, 6, true);
        let out = dir.path().join("ab.dendro");
        run_combine(&[a.clone(), b.clone()], &out).unwrap();

        let db = dendro::archive::Archive::open(&out).unwrap();
        let sources = db.read_sources().unwrap();
        assert_eq!(sources.len(), 2);
        for s in &sources {
            assert!(db
                .all_streams(s.id)
                .unwrap()
                .contains(&"threads/tasks/occupants".to_string()));
        }
        drop(db);
        let readers = ::rez::reader::RezReader::open_recordings(
            &out,
            metriken_storage::BufferPool::new(64 * 1024 * 1024),
        )
        .unwrap();
        assert_eq!(readers.len(), 2);
        let spans: Vec<f64> = readers
            .iter()
            .map(|(_, r)| {
                let (s, e) = r.time_range().unwrap();
                e - s
            })
            .collect();
        assert!(spans.contains(&7.0) && spans.contains(&5.0), "{spans:?}");
    }

    /// Inputs still being recorded have their long tables' last rows in the
    /// WAL, and combine seals that tail with an encoder built from each
    /// input's own streams. The two inputs name their long table differently,
    /// so an encoder that knew no long stream, or only the first input's,
    /// would seal a long tail as wide and combine would fail. The names
    /// differ because the encoder keys long streams by (source id, stream)
    /// and both inputs' source id is 1.
    #[test]
    fn combine_seals_each_input_s_long_tail_as_long() {
        use metriken_query::MetricsSource;
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a.dendro"), dir.path().join("b.dendro"));
        crate::dendro_copy::fixtures::recorded_in(&a, 10, false, "threads/tasks");
        crate::dendro_copy::fixtures::recorded_in(&b, 10, false, "procs/tasks");
        for (input, table) in [(&a, "threads/tasks"), (&b, "procs/tasks")] {
            let db = dendro::archive::Archive::open(input).unwrap();
            let id = db.read_sources().unwrap()[0].id;
            assert!(
                db.live_wal_span(id, table).unwrap().rows > 0,
                "{table}: the fixture leaves long rows in the WAL"
            );
        }
        let out = dir.path().join("ab.dendro");
        run_combine(&[a, b], &out).unwrap();

        let readers = ::rez::reader::RezReader::open_recordings(
            &out,
            metriken_storage::BufferPool::new(64 * 1024 * 1024),
        )
        .unwrap();
        assert_eq!(readers.len(), 2);
        for (_, reader) in &readers {
            let (start, end) = reader.time_range().unwrap();
            let metriken_query::QueryResult::Matrix { result } = reader
                .query_range("irate(task_ops{comm=\"nginx\"}[3s])", start, end, 1.0)
                .unwrap()
            else {
                panic!("a matrix");
            };
            assert_eq!(result.len(), 1, "the occupant is found by its label");
            let last = result[0].values.last().unwrap();
            assert_eq!(
                (last.0, last.1),
                (end, 10.0),
                "through the last row, which came from the WAL: {:?}",
                result[0].values
            );
        }
    }

    /// A recording and its snapshot (one uuid) combine into two sources that
    /// read as one recording, answering as the recording alone: copies are
    /// read once (metriken-storage process groups), so nothing is counted
    /// twice.
    #[test]
    fn a_recording_and_its_copy_combine_into_one_recording() {
        use metriken_query::MetricsSource;
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.dendro");
        crate::dendro_copy::fixtures::recorded(&a, 6, true);
        let copy = dir.path().join("copy.dendro");
        crate::parquet_tools::snapshot_rez(&a, &copy).unwrap();
        let out = dir.path().join("twice.dendro");
        run_combine(&[a.clone(), copy], &out).unwrap();

        let sources = dendro::archive::Archive::open(&out)
            .unwrap()
            .read_sources()
            .unwrap();
        assert_eq!(sources.len(), 2, "both sources are kept");
        let pool = || metriken_storage::BufferPool::new(64 * 1024 * 1024);
        let both = ::rez::reader::RezReader::open_recordings(&out, pool()).unwrap();
        assert_eq!(both.len(), 1, "one recording");
        let alone = ::rez::reader::RezReader::open_recordings(&a, pool()).unwrap();
        let answer = |r: &dyn MetricsSource| {
            let (start, end) = r.time_range().unwrap();
            serde_json::to_value(
                r.query_range("sum(irate(task_ops[3s]))", start, end, 1.0)
                    .unwrap(),
            )
            .unwrap()
        };
        assert_eq!(answer(&both[0].1), answer(&alone[0].1));
    }

    /// metriken-query 0.42 reads each process group once, and these are the
    /// cases that decide a group. Two sources with one `producer_epoch`
    /// whose spans overlap (one agent process recorded twice) are one
    /// recording, answering what either answers alone, not the sum; with
    /// disjoint spans they are two; with different epochs they are two
    /// (the control); and an archive combined with its own snapshot (the
    /// same uuid) is one.
    #[test]
    fn producer_epoch_decides_which_sources_are_one_recording() {
        use ::rez::fixtures::{counter, map, snap_counters, write_dendro, Finish, Source, ANCHOR};
        use metriken_query::MetricsSource;
        let dir = tempfile::tempdir().unwrap();
        let source = |epoch: &str, ticks: std::ops::Range<u64>| Source {
            labels: map(&[("source", "rezolus")]),
            metadata: map(&[("producer_epoch", epoch)]),
            rows: ticks
                .map(|i| {
                    let ts = ANCHOR + i * 1_000_000_000;
                    let w = Some(::rez::window::Window::new(ts - 50_000_000, ts));
                    (snap_counters(ts, vec![counter("ops", "s", 10 * i, w)]), ts)
                })
                .collect(),
        };
        let pool = || metriken_storage::BufferPool::new(16 * 1024 * 1024);
        let open = |name: &str, sources: &[Source]| {
            let path = dir.path().join(name);
            write_dendro(&path, sources, None, Finish::Finalized);
            (
                path.clone(),
                ::rez::reader::RezReader::open_recordings(&path, pool()).unwrap(),
            )
        };
        let answer = |r: &dyn MetricsSource| {
            let (start, end) = r.time_range().unwrap();
            serde_json::to_value(
                r.query_range("sum(irate(ops[2s]))", start, end, 1.0)
                    .unwrap(),
            )
            .unwrap()
        };

        let (one_path, one) = open("one.dendro", &[source("E", 0..6)]);
        let (_, twice) = open("twice.dendro", &[source("E", 0..6), source("E", 0..6)]);
        assert_eq!(
            twice.len(),
            1,
            "one process recorded twice is one recording"
        );
        assert_eq!(answer(&twice[0].1), answer(&one[0].1), "read once");

        let (_, apart) = open("apart.dendro", &[source("E", 0..4), source("E", 10..14)]);
        assert_eq!(apart.len(), 2, "one epoch, disjoint spans: two recordings");
        let (_, other) = open("other.dendro", &[source("E", 0..6), source("F", 0..6)]);
        assert_eq!(other.len(), 2, "different epochs: two recordings");

        let copy = dir.path().join("copy.dendro");
        crate::parquet_tools::snapshot_rez(&one_path, &copy).unwrap();
        let out = dir.path().join("with-copy.dendro");
        run_combine(&[one_path, copy], &out).unwrap();
        let both = ::rez::reader::RezReader::open_recordings(&out, pool()).unwrap();
        assert_eq!(both.len(), 1, "a source and its copy are one recording");
        assert_eq!(answer(&both[0].1), answer(&one[0].1));
    }

    /// `--ab` positions index the recordings `describe-recording` lists (the
    /// read groups), not the archive's sources. An original, its snapshot and
    /// a later recording are three sources but two recordings: position 0 is
    /// the original with its copy, and both of its sources take the arm;
    /// position 1 is the later one; position 2 does not exist.
    #[test]
    fn ab_positions_name_recordings_not_sources() {
        use ::rez::fixtures::{counter, map, snap_counters, write_dendro, Finish, Source, ANCHOR};
        let dir = tempfile::tempdir().unwrap();
        let source = |epoch: &str, ticks: std::ops::Range<u64>| Source {
            labels: map(&[("source", "rezolus")]),
            metadata: map(&[("producer_epoch", epoch)]),
            rows: ticks
                .map(|i| {
                    let ts = ANCHOR + i * 1_000_000_000;
                    let w = Some(::rez::window::Window::new(ts - 50_000_000, ts));
                    (snap_counters(ts, vec![counter("ops", "s", 10 * i, w)]), ts)
                })
                .collect(),
        };
        let a = dir.path().join("a.dendro");
        write_dendro(&a, &[source("E", 0..6)], None, Finish::Finalized);
        let copy = dir.path().join("a-copy.dendro");
        crate::parquet_tools::snapshot_rez(&a, &copy).unwrap();
        let later = dir.path().join("later.dendro");
        write_dendro(&later, &[source("F", 10..16)], None, Finish::Finalized);
        let three = dir.path().join("three.dendro");
        run_combine(&[a, copy, later], &three).unwrap();
        assert_eq!(read_groups_of(&three).unwrap(), [vec![0, 1], vec![2]]);

        let err = run_combine_ab(
            std::slice::from_ref(&three),
            &dir.path().join("x.dendro"),
            &["baseline=0", "experiment=2"],
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("holds 2 recording(s)"), "{err}");

        let ab = dir.path().join("ab.dendro");
        run_combine_ab(
            std::slice::from_ref(&three),
            &ab,
            &["baseline=0", "experiment=1"],
        )
        .unwrap();
        let arms: Vec<Option<String>> = dendro::archive::Archive::open(&ab)
            .unwrap()
            .read_sources()
            .unwrap()
            .into_iter()
            .map(|s| s.meta.labels.get("arm").cloned())
            .collect();
        assert_eq!(
            arms,
            [
                Some("baseline".into()),
                Some("baseline".into()),
                Some("experiment".into())
            ]
        );
        let readers = ::rez::reader::RezReader::open_recordings(
            &ab,
            metriken_storage::BufferPool::new(16 * 1024 * 1024),
        )
        .unwrap();
        assert_eq!(
            readers.len(),
            2,
            "the original and its copy stay one recording"
        );
        // Three sources took an arm, in two recordings, and the report
        // counts recordings.
        let groups = read_groups_of(&ab).unwrap();
        assert_eq!(recordings_touched(&groups, &(0..3).collect()), 2);
        let said = SAID.with(|s| s.take());
        assert!(
            said.iter()
                .any(|l| l.starts_with("stamped arm on 2 recording(s)")),
            "{said:?}"
        );
    }

    /// An output that appears while the archive is assembled is refused and
    /// kept as it was: publishing links, never replaces.
    #[test]
    fn combine_refuses_an_output_that_appeared_meanwhile() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.dendro");
        let b = dir.path().join("b.dendro");
        crate::dendro_copy::fixtures::recorded(&a, 4, true);
        crate::dendro_copy::fixtures::recorded(&b, 4, true);
        let out = dir.path().join("out.dendro");
        APPEAR.with(|x| *x.borrow_mut() = Some(b"theirs".to_vec()));
        let err = run_combine(&[a, b], &out).unwrap_err().to_string();
        assert!(err.contains("already exists"), "{err}");
        assert_eq!(std::fs::read(&out).unwrap(), b"theirs");
    }
}
