pub(crate) mod ab_tarball;
pub(crate) mod annotate;
pub(crate) mod check;
mod cli;
pub(crate) mod combine;
mod combined_parquet;
mod convert;
pub(crate) mod events;
mod filter;
pub(crate) mod legacy;
pub(crate) mod metadata;
pub(crate) mod select;

// The Args structs are the tools' API for a host that builds its own clap
// tree; this binary mounts `Host::command` and names none of them.
#[allow(unused_imports)]
pub use cli::{
    AnnotateArgs, CheckArgs, CombineArgs, ConvertArgs, FilterArgs, Host, MetadataArgs, Recording,
    ReindexArgs, SnapshotArgs, TableFlag, UpgradeArgs,
};

/// Copy a dendro archive that may still be written into one complete
/// standalone `.dendro` at `output`, which must end in `.dendro` and not
/// exist. Any other file is refused, one an earlier release wrote with the
/// `upgrade` command spelled with `program`.
///
/// **The point is SQLite's `-wal` sidecar.** An archive is a SQLite database
/// in WAL mode: commits land in `<file>-wal` and are checkpointed into the
/// archive in batches, so the archive on its own is a consistent view as of
/// the last checkpoint and nothing newer. `cp` takes that older view, and it
/// is not detectably wrong, it just ends early. The archive is copied in one
/// read snapshot (sidecar included, writer un-paused) with its live tail
/// sealed.
pub(crate) fn snapshot_rez(
    path: &std::path::Path,
    output: &std::path::Path,
    program: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    ::rez::container::require_dendro(path, program)?;
    legacy::check_new_dendro(output)?;
    // Staged beside the output so a failed copy leaves nothing there.
    let staging = legacy::staging_dir(output)?;
    let staged = staging.path().join("snapshot.dendro");
    let src = dendro::archive::Archive::open(path)?;
    crate::dendro_copy::copy(&src, &staged, &dendro::rewrite::CopySpec::everything())?;
    legacy::publish(&staged, output)?;
    println!("Wrote {}", output.display());
    Ok(())
}

/// Rewrite the names index of every sealed segment of the dendro archive at
/// `path` in place, as this release's writer writes them
/// (`metriken_storage::reindex`): each table segment's index then holds what
/// opening its table reads, so the viewer opens the table without fetching
/// the segment. No segment is re-encoded.
///
/// Refuses any file but a dendro archive, an archive before schema 5 (where
/// reading a segment's index reads the segment), naming the `snapshot` that
/// copies it into a current one, and an archive another connection has open
/// (a viewer, a recording): the re-index needs the file to itself, and a
/// viewer cannot open it while the re-index runs.
pub(crate) fn reindex_dendro(
    path: &std::path::Path,
    program: &str,
) -> Result<metriken_storage::reindex::Reindexed, Box<dyn std::error::Error>> {
    ::rez::container::require_dendro(path, program)?;
    let shown = path.display().to_string();
    let in_use = |e: Box<dyn std::error::Error>| -> Box<dyn std::error::Error> {
        match e.downcast_ref::<dendro::Error>() {
            Some(dendro::Error::InUse { .. }) => format!(
                "{shown} is open in another process (a viewer, an MCP server, a recording \
                 or an edit); re-index it once that process closes it, or re-index a \
                 `{program} recording snapshot` of it"
            )
            .into(),
            _ => e,
        }
    };
    let schema = dendro::archive::Archive::open(path)
        .and_then(|a| a.schema_version())
        .map_err(|e| in_use(e.into()))?;
    if schema < 5 {
        let file = ::rez::container::shell_word(&shown);
        return Err(format!(
            "{shown} is an archive of schema {schema}, where reading a segment's index \
             reads the whole segment; copy it into a current archive and re-index the \
             copy:\n    {program} recording snapshot {file} -o NEW.dendro\n    \
             {program} recording reindex NEW.dendro"
        )
        .into());
    }
    let report = metriken_storage::reindex::reindex(path).map_err(in_use)?;
    println!(
        "Re-indexed {shown}: {} of {} segments rewritten, {} already current, {} left as \
         they were",
        report.replaced, report.segments, report.current, report.left
    );
    Ok(report)
}

/// Convert the legacy recording at `path` (any `.rez`, a parquet file, or a
/// `.parquet.ab.tar`) into a new dendro archive at `output`, or beside
/// `path` with a `.dendro` extension. Never in place and never over an
/// existing file.
fn upgrade_to_dendro(
    path: &std::path::Path,
    output: Option<&std::path::Path>,
    to: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    if to != "dendro" {
        return Err(format!(
            "--to {to}: `recording upgrade` converts to dendro, the only format written"
        )
        .into());
    }
    if !path.exists() {
        return Err(format!("{}: no such file", path.display()).into());
    }
    if legacy::Container::of_path(path) == legacy::Container::Dendro {
        return Err(format!(
            "{} is already a dendro archive; there is nothing to upgrade",
            path.display()
        )
        .into());
    }
    let output = output
        .map(std::path::Path::to_path_buf)
        .unwrap_or_else(|| legacy::dendro_beside(path));
    legacy::check_new_dendro(&output)?;
    // Staged beside the output and moved there whole, so a failed
    // conversion leaves nothing at the output.
    let (_staging, staged) = legacy::stage_beside(&output)?;
    let summary = legacy::convert_quiet(path, &staged)?;
    legacy::publish(&staged, &output)?;
    println!(
        "converted {} ({}) to {}: {summary}",
        path.display(),
        legacy::Container::of_path(path).name(),
        output.display()
    );
    Ok(())
}

/// A [`Host`] for this module's tests, spelled as rezolus's, and the parse
/// the CLI makes.
#[cfg(test)]
pub(crate) mod test_support {
    use super::{Host, Recording};
    use ::dashboard::TemplateRegistry;
    use std::path::Path;

    fn registry(dir: Option<&Path>) -> TemplateRegistry {
        match dir {
            Some(dir) => TemplateRegistry::load(dir).unwrap_or_else(|_| TemplateRegistry::empty()),
            None => TemplateRegistry::empty(),
        }
    }

    pub(crate) fn host() -> Host<'static> {
        Host::new("rezolus", &registry)
            .display_name("Rezolus")
            .agent("rezolus")
            .alias("parquet")
            .table_flag(super::TableFlag::Samplers)
            .list_recordings("rezolus mcp describe-recording")
            .record("rezolus record")
            .hindsight("rezolus hindsight")
            .view("rezolus view")
            .mcp("rezolus mcp")
            .buffer_path("/var/lib/rezolus/hindsight.dendro")
    }

    /// The committed parquet capture (`tests/fixtures/legacy`) upgraded to
    /// `simple.dendro` in `dir`: five samples of a gauge `queue_depth`,
    /// constant at 3, and a counter `http_requests_total`.
    pub(crate) fn simple_dendro(dir: &Path) -> std::path::PathBuf {
        let parquet = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/legacy/simple_capture.parquet");
        let out = dir.join("simple.dendro");
        super::legacy::convert(&parquet, &out).unwrap();
        out
    }

    /// `args` (the program name first, then a subcommand) parsed as the CLI
    /// parses them.
    pub(crate) fn parse(args: impl IntoIterator<Item = String>) -> Recording {
        use clap::FromArgMatches;
        let matches = host().command().try_get_matches_from(args).unwrap();
        Recording::from_arg_matches(&matches).unwrap()
    }
}

#[cfg(test)]
mod command_name_tests {
    /// The rename is the point — these subcommands stopped being
    /// parquet-specific once `.rez` arrived — but every existing script and
    /// runbook types `rezolus recording ...`, and breaking those to make a name
    /// tidier is not a trade worth making. Both spellings must reach the same
    /// subcommand, and clap must resolve the alias to the canonical name so
    /// `main`'s dispatch needs only one arm.
    #[test]
    fn parquet_still_reaches_the_recording_subcommand() {
        let app = clap::Command::new("rezolus").subcommand(super::test_support::host().command());

        for spelling in ["recording", "parquet"] {
            let m = app
                .clone()
                .try_get_matches_from(["rezolus", spelling, "metadata", "-i", "x.parquet"])
                .unwrap_or_else(|e| panic!("`rezolus {spelling} metadata` must parse: {e}"));
            let (name, sub) = m.subcommand().expect("a subcommand matched");
            assert_eq!(
                name, "recording",
                "clap must report the canonical name for `{spelling}`, so `main` \
                 dispatches on one arm rather than two"
            );
            assert_eq!(sub.subcommand_name(), Some("metadata"));
        }
    }
}

/// `filter` shows one spelling of its table selection and accepts the
/// other hidden: rezolus shows `--samplers`, a host without samplers shows
/// `--tables`.
#[cfg(test)]
mod table_flag_tests {
    fn filter_help(samplers: bool) -> String {
        let flag = if samplers {
            super::TableFlag::Samplers
        } else {
            super::TableFlag::Tables
        };
        let mut cmd = super::test_support::host().table_flag(flag).command();
        cmd.find_subcommand_mut("filter")
            .expect("filter")
            .render_long_help()
            .to_string()
    }

    #[test]
    fn one_spelling_is_shown_and_both_parse() {
        let rezolus = filter_help(true);
        assert!(rezolus.contains("--samplers <A,B,...>"), "{rezolus}");
        assert!(!rezolus.contains("--tables"), "{rezolus}");
        let other = filter_help(false);
        assert!(other.contains("--tables <A,B,...>"), "{other}");
        assert!(!other.contains("--samplers <"), "{other}");
        for flag in ["--samplers", "--tables"] {
            let parsed = super::test_support::parse(
                ["rezolus", "filter", "x.dendro", flag, "a"].map(String::from),
            );
            let super::Recording::Filter(args) = parsed else {
                panic!("not a filter");
            };
            assert_eq!(
                (args.samplers.is_some(), args.tables.is_some()),
                (flag == "--samplers", flag == "--tables")
            );
        }
    }
}

/// A host with none of rezolus's commands renders help that names none of
/// them, and every placeholder is filled.
#[cfg(test)]
mod other_host_tests {
    fn empty(_: Option<&std::path::Path>) -> ::dashboard::TemplateRegistry {
        ::dashboard::TemplateRegistry::empty()
    }

    #[test]
    fn help_names_only_the_host() {
        let host = super::Host::new("othertool", &empty);
        let mut cmd = host.command();
        let mut texts = vec![cmd.render_long_help().to_string()];
        let names: Vec<String> = cmd
            .get_subcommands()
            .map(|c| c.get_name().to_string())
            .collect();
        for name in names {
            let sub = cmd.find_subcommand_mut(&name).expect("listed");
            texts.push(sub.render_long_help().to_string());
            texts.push(sub.render_help().to_string());
        }
        for text in texts {
            assert!(!text.to_lowercase().contains("rezolus"), "{text}");
            for placeholder in ["{program}", "{agent}", "{table_", "{writers}", "{viewer}"] {
                assert!(!text.contains(placeholder), "{placeholder} in {text}");
            }
            assert!(!text.contains("--samplers <"), "{text}");
        }
    }
}

#[cfg(test)]
mod upgrade_to_dendro_tests {
    use super::upgrade_to_dendro;
    use dendro::archive::Archive;

    /// The committed v2 tar archive (`rez::fixtures::committed_rez`): one
    /// recording, tables `a` and `b`, written whole by the old recorder.
    fn tar_rez(dir: &std::path::Path) -> std::path::PathBuf {
        use metriken_storage::rez::rez::{detect_rez_format, RezFormat};
        let path = ::rez::fixtures::committed_rez("v2.rez", dir);
        assert_eq!(detect_rez_format(&path).unwrap(), RezFormat::V2Tar);
        path
    }

    #[test]
    fn a_tar_archive_converts_to_dendro() {
        let dir = tempfile::tempdir().unwrap();
        let src = tar_rez(dir.path());
        let dest = dir.path().join("out.dendro");
        upgrade_to_dendro(&src, Some(&dest), "dendro").unwrap();

        // Before anything opens the output: a reader's connection leaves
        // `-wal`/`-shm` files of its own, and the claim here is that the
        // conversion leaves neither those nor its staging directory.
        let mut names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        assert_eq!(names, vec!["out.dendro".to_string(), "v2.rez".to_string()]);

        let out = Archive::open(&dest).unwrap();
        let sources = out.read_sources().unwrap();
        assert_eq!(sources.len(), 1);
        assert!(sources[0].complete);
        assert_eq!(
            out.all_streams(sources[0].id).unwrap(),
            vec!["a".to_string(), "b".to_string()]
        );
    }

    #[test]
    fn an_existing_output_is_refused_and_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let src = tar_rez(dir.path());
        let dest = dir.path().join("out.dendro");
        std::fs::write(&dest, b"keep me").unwrap();
        let err = upgrade_to_dendro(&src, Some(&dest), "dendro")
            .unwrap_err()
            .to_string();
        assert!(err.contains("already exists"), "{err}");
        assert_eq!(std::fs::read(&dest).unwrap(), b"keep me");
    }

    /// Without `-o` the output is the input with a `.dendro` extension, and
    /// `--to v3` is refused: 6.x writes .dendro only.
    #[test]
    fn the_default_output_is_beside_the_input_and_v3_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let src = tar_rez(dir.path());
        let err = upgrade_to_dendro(&src, None, "v3").unwrap_err().to_string();
        assert!(err.contains("converts to dendro"), "{err}");
        upgrade_to_dendro(&src, None, "dendro").unwrap();
        assert!(dir.path().join("v2.dendro").exists());
    }

    /// Every legacy fixture converts, and the archive answers what the
    /// original answers.
    #[test]
    fn each_legacy_fixture_converts_and_answers_the_same() {
        use metriken_query::MetricsSource;
        let dir = tempfile::tempdir().unwrap();
        let pool = || metriken_storage::BufferPool::new(64 * 1024 * 1024);
        for name in ["v2.rez", "v3.rez", "v3-live.rez", "v3-multi.rez"] {
            let src = ::rez::fixtures::committed_rez(name, dir.path());
            let dest = dir.path().join(format!("{name}.dendro"));
            upgrade_to_dendro(&src, Some(&dest), "dendro").unwrap();
            let a = metriken_storage::rez::open::open_recordings(&src, pool()).unwrap();
            let b = ::rez::reader::RezReader::open_recordings(&dest, pool()).unwrap();
            assert_eq!(a.len(), b.len(), "{name}");
            for ((la, ra), (lb, rb)) in a.iter().zip(&b) {
                assert_eq!(la, lb, "{name}");
                let (start, end) = ra.time_range().unwrap();
                for q in ["mem_free", "irate(ops[2s])", "histogram_count(lat)"] {
                    let answer = |r: &dyn MetricsSource| {
                        r.query_range(q, start, end, 1.0)
                            .map(|v| serde_json::to_value(v).unwrap())
                            .ok()
                    };
                    assert_eq!(answer(ra), answer(rb), "{name}: {q}");
                }
            }
        }
    }
}

#[cfg(test)]
mod dendro_command_tests {
    use super::{reindex_dendro, snapshot_rez, upgrade_to_dendro};
    use crate::dendro_copy::fixtures;
    use dendro::archive::Archive;

    /// A copy in `dir` of `tests/fixtures/upgraded/v3.dendro`, which an
    /// earlier release wrote: its segments carry the earlier indexes.
    fn earlier(dir: &std::path::Path) -> std::path::PathBuf {
        let path = dir.join("v3.dendro");
        std::fs::copy(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/upgraded/v3.dendro"),
            &path,
        )
        .unwrap();
        path
    }

    /// Every sealed segment's index in the archive at `path`.
    fn indexes(path: &std::path::Path) -> Vec<Option<Vec<u8>>> {
        let archive = Archive::open(path).unwrap();
        let mut out = Vec::new();
        for source in archive.read_sources().unwrap() {
            for stream in archive.all_streams(source.id).unwrap() {
                for (_, index) in archive.read_segment_indexes(source.id, &stream).unwrap() {
                    out.push(index);
                }
            }
        }
        out
    }

    /// Every counter's rate and every gauge of the archive at `path`.
    fn answers(path: &std::path::Path) -> Vec<serde_json::Value> {
        use metriken_query::MetricsSource;
        let readers = ::rez::reader::RezReader::open_recordings(
            path,
            metriken_storage::BufferPool::new(16 << 20),
        )
        .unwrap();
        let r = &readers[0].1;
        let (start, end) = r.time_range().unwrap();
        let queries: Vec<String> = r
            .counter_names()
            .into_iter()
            .map(|c| format!("sum(irate({c}[2s]))"))
            .chain(r.gauge_names())
            .collect();
        assert!(!queries.is_empty());
        queries
            .iter()
            .map(|q| serde_json::to_value(r.query_range(q, start, end, 1.0).unwrap()).unwrap())
            .collect()
    }

    /// `reindex` of an archive an earlier release wrote replaces its table
    /// segments' indexes, and every query answers as before; a second run
    /// finds every index current.
    #[test]
    fn reindex_replaces_earlier_indexes_and_answers_as_before() {
        let dir = tempfile::tempdir().unwrap();
        let path = earlier(dir.path());
        let (before, old) = (answers(&path), indexes(&path));

        let first = reindex_dendro(&path, "rezolus").unwrap();
        assert!(first.replaced > 0, "{first:?}");
        assert_eq!(first.segments, first.replaced + first.current + first.left);
        assert_ne!(indexes(&path), old);
        assert_eq!(answers(&path), before);

        let again = reindex_dendro(&path, "rezolus").unwrap();
        assert_eq!(
            (again.replaced, again.current),
            (0, first.replaced + first.current),
            "{again:?}"
        );
    }

    /// `reindex` refuses an archive before schema 5, naming the snapshot
    /// that copies it into a current one, and leaves it as it was.
    #[test]
    fn reindex_refuses_a_schema_4_archive_with_the_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("old.dendro");
        std::fs::copy(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/released/v5.29.2.dendro"),
            &path,
        )
        .unwrap();
        let bytes = std::fs::read(&path).unwrap();
        let err = reindex_dendro(&path, "tool").unwrap_err().to_string();
        assert!(err.contains("schema 4"), "{err}");
        assert!(err.contains("tool recording snapshot"), "{err}");
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }

    /// `reindex` refuses an archive a reader holds open (as the viewer
    /// keeps one), saying another process has it open.
    #[test]
    fn reindex_refuses_an_archive_open_for_reading() {
        let dir = tempfile::tempdir().unwrap();
        let path = earlier(dir.path());
        let held = Archive::open(&path).unwrap();
        let err = reindex_dendro(&path, "tool").unwrap_err().to_string();
        assert!(err.contains("is open in another process"), "{err}");
        drop(held);
    }

    /// `reindex` refuses an archive another connection holds exclusively (an
    /// in-place edit), at the schema read, saying another process has it
    /// open.
    #[test]
    fn reindex_refuses_an_archive_an_edit_holds() {
        let dir = tempfile::tempdir().unwrap();
        let path = earlier(dir.path());
        let edit = dendro::archive::ArchiveMut::open(&path).unwrap();
        let err = reindex_dendro(&path, "tool").unwrap_err().to_string();
        assert!(err.contains("is open in another process"), "{err}");
        drop(edit);
    }

    /// `reindex` refuses an archive a writer holds, saying another process
    /// has it open.
    #[test]
    fn reindex_refuses_an_archive_being_written() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("live.dendro");
        let writer = metriken_storage::ArchiveWriter::create(
            &path,
            metriken_storage::WriterConfig::default(),
        )
        .unwrap();
        let err = reindex_dendro(&path, "tool").unwrap_err().to_string();
        assert!(err.contains("is open in another process"), "{err}");
        drop(writer);
    }

    /// `snapshot` of a dendro archive with rows still in its WAL: the copy
    /// holds every row, all sealed, and nothing is left beside it.
    #[test]
    fn a_dendro_snapshot_is_sealed_and_whole() {
        let dir = tempfile::tempdir().unwrap();
        let live = dir.path().join("live.dendro");
        fixtures::recorded(&live, 10, false);
        let out = dir.path().join("snap.dendro");
        snapshot_rez(&live, &out, "rezolus").unwrap();
        for suffix in ["-wal", "-shm"] {
            assert!(!dir.path().join(format!("snap.dendro{suffix}")).exists());
        }

        let (src, snap) = (Archive::open(&live).unwrap(), Archive::open(&out).unwrap());
        let (a, b) = (
            src.read_sources().unwrap()[0].id,
            snap.read_sources().unwrap()[0].id,
        );
        let streams = crate::dendro_copy::streams(&src).unwrap();
        assert!(streams.iter().any(|s| s.ends_with("/occupants")));
        for stream in streams {
            let (_, sealed) = src.segment_span(a, &stream).unwrap();
            let wal = src.live_wal_span(a, &stream).unwrap();
            let (_, got) = snap.segment_span(b, &stream).unwrap();
            assert_eq!(got.rows, sealed.rows + wal.rows, "{stream}");
            assert_eq!(snap.live_wal_span(b, &stream).unwrap().rows, 0, "{stream}");
        }
        assert!(
            snapshot_rez(&live, &out, "rezolus").is_err(),
            "never over an existing file"
        );
    }

    /// `upgrade` names a dendro input for what it is.
    #[test]
    fn upgrade_names_a_dendro_input() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rec.dendro");
        fixtures::recorded(&path, 4, true);
        let err = upgrade_to_dendro(&path, Some(&dir.path().join("out.dendro")), "dendro")
            .unwrap_err()
            .to_string();
        assert!(err.contains("already a dendro archive"), "{err}");
    }

    /// `snapshot` of a `.rez` is refused, naming the upgrade, and writes
    /// nothing.
    #[test]
    fn a_rez_snapshot_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let live = ::rez::fixtures::committed_rez("v3-live.rez", dir.path());
        let out = dir.path().join("snap.dendro");
        let err = snapshot_rez(&live, &out, "tool").unwrap_err().to_string();
        assert!(err.contains("tool recording upgrade"), "{err}");
        assert!(!out.exists());
    }

    /// `upgrade` converts a `.parquet.ab.tar` into a two-arm archive beside
    /// it.
    #[test]
    fn upgrade_converts_an_ab_tarball() {
        let dir = tempfile::tempdir().unwrap();
        let tar = super::ab_tarball::tests::ab_tarball(dir.path());
        upgrade_to_dendro(&tar, None, "dendro").unwrap();
        let out = dir.path().join("combined.dendro");
        let sources = Archive::open(&out).unwrap().read_sources().unwrap();
        let arms: Vec<_> = sources
            .iter()
            .map(|s| s.meta.labels.get("arm").cloned())
            .collect();
        assert_eq!(
            arms,
            [Some("baseline".to_string()), Some("experiment".to_string())]
        );
    }
}
