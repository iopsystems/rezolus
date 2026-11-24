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
    SnapshotArgs, TableFlag, UpgradeArgs,
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

/// A test's hook on an in-place copying upgrade.
#[cfg(test)]
type UpgradeHook = Box<dyn FnOnce(&std::path::Path)>;

#[cfg(test)]
thread_local! {
    /// Called with the input of an in-place copying upgrade after the copy is
    /// re-indexed and before it is renamed over the input.
    static BEFORE_RENAME: std::cell::RefCell<Option<UpgradeHook>> =
        const { std::cell::RefCell::new(None) };
}

/// What [`upgrade_dendro`] did.
#[derive(Debug)]
pub(crate) struct DendroUpgrade {
    /// The archive's schema version before the upgrade.
    pub(crate) schema: i64,
    /// Whether the archive was copied into the current layout.
    pub(crate) copied: bool,
    /// The re-index of the archive, or of its copy.
    pub(crate) reindexed: metriken_storage::reindex::Reindexed,
}

/// Bring the dendro archive at `path` to what this release writes: each
/// sealed segment's names index rewritten as this release's writer writes
/// it (`metriken_storage::reindex`), so the viewer opens a table from its
/// segments' indexes without fetching the segments.
///
/// - Without `output`, a schema 5 archive is re-indexed in place, with no
///   segment's data rewritten. A schema 4 archive, where reading a
///   segment's index reads the segment, is copied into the current layout,
///   the copy re-indexed, and the copy renamed over it.
/// - With `output`, which must end in `.dendro` and not exist, the archive
///   is copied there and the copy re-indexed; the input is not changed.
///
/// Changing the input in place is refused while another connection has it
/// open (a viewer, a recording, an edit), and holds the file exclusively
/// until the re-index is done, or until just before the copy is renamed
/// over it, so a viewer cannot open it while the upgrade runs. In place, a symlink's target is
/// upgraded, and a copy that replaces it takes its permissions.
pub(crate) fn upgrade_dendro(
    path: &std::path::Path,
    output: Option<&std::path::Path>,
    program: &str,
) -> Result<DendroUpgrade, Box<dyn std::error::Error>> {
    let shown = path.display().to_string();
    let copy_instead = match output {
        None => format!(
            "; or write an upgraded copy with `{program} recording upgrade {shown} -o \
             NEW.dendro`"
        ),
        Some(_) => String::new(),
    };
    let in_use = |e: Box<dyn std::error::Error>| -> Box<dyn std::error::Error> {
        match e.downcast_ref::<dendro::Error>() {
            Some(dendro::Error::InUse { .. }) => format!(
                "{shown} is open in another process (a viewer, an MCP server, a recording \
                 or an edit); upgrade it once that process closes it{}",
                if copy_instead.is_empty() {
                    String::new()
                } else {
                    format!(", or, if a viewer or a recording has it open{copy_instead}")
                }
            )
            .into(),
            Some(dendro::Error::ReadOnly(_)) => {
                format!("{shown} is not writable{copy_instead}").into()
            }
            _ => e,
        }
    };
    if let Some(output) = output {
        legacy::check_new_dendro(output)?;
    }
    let schema = dendro::archive::Archive::open(path)
        .and_then(|a| a.schema_version())
        .map_err(|e| in_use(e.into()))?;
    let upgrade = if output.is_none() && schema >= 5 {
        let reindexed = metriken_storage::reindex::reindex(path).map_err(in_use)?;
        DendroUpgrade {
            schema,
            copied: false,
            reindexed,
        }
    } else {
        // In place, the copy replaces the file a symlink names, and the
        // input is held exclusively from before the copy until the rename,
        // so no writer, edit or viewer opens the file the rename unlinks.
        let target = match output {
            Some(_) => path.to_path_buf(),
            None => std::fs::canonicalize(path)?,
        };
        let held = match output {
            Some(_) => None,
            None => Some(dendro::archive::ArchiveMut::open(&target).map_err(|e| in_use(e.into()))?),
        };
        let opened;
        let src: &dendro::archive::Archive = match &held {
            Some(held) => held,
            None => {
                opened = dendro::archive::Archive::open(path).map_err(|e| in_use(e.into()))?;
                &opened
            }
        };
        let dest = output.unwrap_or(&target);
        let staging = legacy::staging_dir(dest)?;
        let staged = staging.path().join("upgraded.dendro");
        crate::dendro_copy::copy(src, &staged, &dendro::rewrite::CopySpec::everything())?;
        let reindexed = metriken_storage::reindex::reindex(&staged)?;
        #[cfg(test)]
        BEFORE_RENAME.with(|hook| {
            if let Some(hook) = hook.borrow_mut().take() {
                hook(&target)
            }
        });
        match output {
            Some(output) => legacy::publish(&staged, output)?,
            None => {
                let permissions = std::fs::metadata(&target)?.permissions();
                std::fs::set_permissions(&staged, permissions)?;
                drop(held);
                std::fs::rename(&staged, &target)?;
            }
        }
        DendroUpgrade {
            schema,
            copied: true,
            reindexed,
        }
    };
    let r = &upgrade.reindexed;
    if !upgrade.copied && r.replaced == 0 {
        println!(
            "{shown} is current: {} segments, nothing to upgrade",
            r.segments
        );
    } else {
        let from = if upgrade.schema < 5 {
            format!(" from schema {} to the current layout", upgrade.schema)
        } else {
            String::new()
        };
        let to = output.map_or(String::new(), |o| format!(" into {}", o.display()));
        println!(
            "upgraded {shown}{from}{to}: {} of {} segments re-indexed, {} already current, \
             {} left as they were",
            r.replaced, r.segments, r.current, r.left
        );
    }
    Ok(upgrade)
}

/// Bring the recording at `path` to what this release writes. A dendro
/// archive is upgraded by [`upgrade_dendro`]. A legacy recording (any
/// `.rez`, a parquet file, or a `.parquet.ab.tar`) is converted into a new
/// dendro archive at `output`, or beside `path` with a `.dendro` extension,
/// never in place and never over an existing file.
fn upgrade_to_dendro(
    path: &std::path::Path,
    output: Option<&std::path::Path>,
    to: &str,
    program: &str,
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
        return upgrade_dendro(path, output, program).map(|_| ());
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
        upgrade_to_dendro(&src, Some(&dest), "dendro", "rezolus").unwrap();

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
        let err = upgrade_to_dendro(&src, Some(&dest), "dendro", "rezolus")
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
        let err = upgrade_to_dendro(&src, None, "v3", "rezolus")
            .unwrap_err()
            .to_string();
        assert!(err.contains("converts to dendro"), "{err}");
        upgrade_to_dendro(&src, None, "dendro", "rezolus").unwrap();
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
            upgrade_to_dendro(&src, Some(&dest), "dendro", "rezolus").unwrap();
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
    use super::{snapshot_rez, upgrade_dendro, upgrade_to_dendro};
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

    /// A copy in `dir` of `tests/fixtures/released/v5.29.2.dendro`, a 5.x
    /// archive (schema 4).
    fn schema_4(dir: &std::path::Path) -> std::path::PathBuf {
        let path = dir.join("old.dendro");
        std::fs::copy(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/released/v5.29.2.dendro"),
            &path,
        )
        .unwrap();
        path
    }

    /// `upgrade` of an archive an earlier 6.0 build wrote re-indexes it in
    /// place, and every query answers as before; a second upgrade finds it
    /// current and changes nothing.
    #[test]
    fn upgrade_reindexes_an_earlier_archive_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let path = earlier(dir.path());
        let (before, old) = (answers(&path), indexes(&path));

        let first = upgrade_dendro(&path, None, "rezolus").unwrap();
        assert!(!first.copied && first.schema == 5, "{first:?}");
        let r = first.reindexed;
        assert!(r.replaced > 0, "{r:?}");
        assert_eq!(r.segments, r.replaced + r.current + r.left);
        assert_ne!(indexes(&path), old);
        assert_eq!(answers(&path), before);

        let bytes = std::fs::read(&path).unwrap();
        let again = upgrade_dendro(&path, None, "rezolus").unwrap().reindexed;
        assert_eq!(
            (again.replaced, again.current),
            (0, r.replaced + r.current),
            "{again:?}"
        );
        assert_eq!(
            std::fs::read(&path).unwrap(),
            bytes,
            "a current archive is left as it is"
        );
    }

    /// `upgrade -o` of a dendro archive writes an upgraded copy there and
    /// leaves the input as it was; an existing output is refused.
    #[test]
    fn upgrade_to_an_output_leaves_the_input() {
        let dir = tempfile::tempdir().unwrap();
        let path = earlier(dir.path());
        let (bytes, before) = (std::fs::read(&path).unwrap(), answers(&path));
        let out = dir.path().join("new.dendro");
        let up = upgrade_dendro(&path, Some(&out), "rezolus").unwrap();
        assert!(up.copied, "{up:?}");
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert_eq!(answers(&out), before);
        let r = metriken_storage::reindex::reindex(&out).unwrap();
        assert_eq!(r.replaced, 0, "the copy is current: {r:?}");
        assert!(
            upgrade_dendro(&path, Some(&out), "rezolus").is_err(),
            "never over a file"
        );
    }

    /// `upgrade` of a 5.x archive (schema 4) copies it into the current
    /// layout in place: the file becomes schema 5 with current indexes, and
    /// every query answers as before.
    #[test]
    fn upgrade_copies_a_schema_4_archive_into_the_current_layout() {
        let dir = tempfile::tempdir().unwrap();
        let path = schema_4(dir.path());
        let before = answers(&path);
        let up = upgrade_dendro(&path, None, "rezolus").unwrap();
        assert!(up.copied && up.schema == 4, "{up:?}");
        for suffix in ["-wal", "-shm"] {
            let sidecar = dir.path().join(format!("old.dendro{suffix}"));
            assert!(
                !sidecar.exists(),
                "{} beside the renamed copy",
                sidecar.display()
            );
        }
        assert_eq!(Archive::open(&path).unwrap().schema_version().unwrap(), 5);
        assert_eq!(answers(&path), before);
        let again = upgrade_dendro(&path, None, "rezolus").unwrap();
        assert!(!again.copied && again.reindexed.replaced == 0, "{again:?}");
    }

    /// A schema 4 upgrade in place holds the input from before the copy
    /// until the rename: neither a reader nor an edit can open it between
    /// the re-index of the copy and the rename.
    #[test]
    fn upgrade_holds_a_schema_4_input_until_the_rename() {
        let dir = tempfile::tempdir().unwrap();
        let path = schema_4(dir.path());
        let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let record = seen.clone();
        super::BEFORE_RENAME.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move |input: &std::path::Path| {
                let read = Archive::open(input).is_ok();
                let edit = dendro::archive::ArchiveMut::open(input).is_ok();
                record.borrow_mut().push((read, edit));
            }))
        });
        upgrade_dendro(&path, None, "rezolus").unwrap();
        assert_eq!(*seen.borrow(), [(false, false)]);
    }

    /// `upgrade` in place of a symlink upgrades the file it names, keeping
    /// the link and the file's permissions.
    #[cfg(unix)]
    #[test]
    fn upgrade_in_place_follows_a_symlink_and_keeps_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = schema_4(dir.path());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
        let link = dir.path().join("link.dendro");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        upgrade_dendro(&link, None, "rezolus").unwrap();
        assert!(std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(Archive::open(&path).unwrap().schema_version().unwrap(), 5);
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o640);
    }

    /// `upgrade` in place of a read-only archive says it is not writable and
    /// names `-o`, which writes an upgraded copy.
    #[cfg(unix)]
    #[test]
    fn upgrade_of_a_read_only_archive_names_the_copy() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = earlier(dir.path());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o444)).unwrap();
        let err = upgrade_dendro(&path, None, "tool").unwrap_err().to_string();
        assert!(err.contains("is not writable"), "{err}");
        assert!(err.contains("-o NEW.dendro"), "{err}");
        let out = dir.path().join("copy.dendro");
        upgrade_dendro(&path, Some(&out), "tool").unwrap();
    }

    /// `upgrade` in place refuses an archive a reader holds open (as the
    /// viewer keeps one), saying another process has it open, for schema 5
    /// and schema 4 alike, and leaves it as it was.
    #[test]
    fn upgrade_refuses_an_archive_open_for_reading() {
        let dir = tempfile::tempdir().unwrap();
        for path in [earlier(dir.path()), schema_4(dir.path())] {
            let bytes = std::fs::read(&path).unwrap();
            let held = Archive::open(&path).unwrap();
            let err = upgrade_dendro(&path, None, "tool").unwrap_err().to_string();
            assert!(err.contains("is open in another process"), "{err}");
            drop(held);
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
        }
    }

    /// `upgrade` refuses an archive another connection holds exclusively (an
    /// in-place edit), at the schema read, saying another process has it
    /// open.
    #[test]
    fn upgrade_refuses_an_archive_an_edit_holds() {
        let dir = tempfile::tempdir().unwrap();
        let path = earlier(dir.path());
        let edit = dendro::archive::ArchiveMut::open(&path).unwrap();
        let err = upgrade_dendro(&path, None, "tool").unwrap_err().to_string();
        assert!(err.contains("is open in another process"), "{err}");
        drop(edit);
    }

    /// `upgrade` in place refuses an archive a writer holds, saying another
    /// process has it open.
    #[test]
    fn upgrade_refuses_an_archive_being_written() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("live.dendro");
        let writer = metriken_storage::ArchiveWriter::create(
            &path,
            metriken_storage::WriterConfig::default(),
        )
        .unwrap();
        let err = upgrade_dendro(&path, None, "tool").unwrap_err().to_string();
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

    /// `upgrade` of an archive this release recorded finds it current and
    /// leaves it as it is.
    #[test]
    fn upgrade_finds_a_current_dendro_input_current() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rec.dendro");
        fixtures::recorded(&path, 4, true);
        let bytes = std::fs::read(&path).unwrap();
        upgrade_to_dendro(&path, None, "dendro", "rezolus").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
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
        upgrade_to_dendro(&tar, None, "dendro", "rezolus").unwrap();
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
