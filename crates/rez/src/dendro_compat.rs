//! Where `rez` ends and [`dendro`] begins.
//!
//! dendro is the crate this format was extracted into, and rezolus now depends
//! on it. Nothing here is on a shipping code path yet; what these tests pin is
//! the **boundary**, because it is not where it was during extraction and the
//! difference is easy to assume away.
//!
//! **dendro 0.1.0 does not read an archive `rez` wrote.** An archive is
//! exactly a file carrying dendro's header stamp — `application_id` plus a
//! `user_version` — and `rez_sqlite` stamps neither. It also writes a
//! `schema_version` TABLE and a `recordings` table where dendro has
//! `sources`. Earlier revisions accepted that shape through compatibility
//! views; `Sniff::Unstamped` and `ReadOnly::LegacySchema` are gone, and
//! `Sniff::NotAnArchive`'s own documentation names `.rez` as an example of
//! what it refuses.
//!
//! That is dendro's decision to make, and it has two consequences for this
//! crate that are worth stating where they cannot be missed:
//!
//! 1. **`crates/rez` stays the reader for legacy archives.** Every `.rez` in
//!    existence — hindsight buffers, archived incident captures, this repo's
//!    fixtures — is unstamped. dendro will not open one, so the code that does
//!    cannot be deleted when the read path moves. See #1224.
//! 2. **Writing dendro-shaped archives is a format break**, not a refactor.
//!    A 6.x archive cannot be opened by a 5.x rezolus, which is what makes it
//!    major-version work rather than something to slip in.

#[cfg(all(test, feature = "test-support"))]
mod tests {
    use crate::rez::recorder_tests_support::{counter, snap};
    use crate::rez_v3_writer::{ManifestSeed, RezArchive, StreamRecorderV3};
    use std::path::Path;

    const ANCHOR: u64 = 1_700_000_000_000_000_000;

    /// A two-sampler archive written by `rez`'s own writer, cleanly finalized.
    fn write_rez_archive(path: &Path) {
        let seed = ManifestSeed {
            labels: [("source".to_string(), "rezolus".to_string())]
                .into_iter()
                .collect(),
            metadata: [("sampling_interval_ms".to_string(), "1000".to_string())]
                .into_iter()
                .collect(),
            clock_anchor_wall_ns: ANCHOR,
        };
        let (archive, writer) = RezArchive::single(path, seed).unwrap();
        let mut rec = StreamRecorderV3::new(writer);
        let mut ts = 1_000_000_000u64;
        for i in 0..4u64 {
            rec.ingest(
                &snap(
                    ts,
                    vec![
                        counter("cpu_cycles", "cpu_usage", i, None),
                        counter("network_bytes", "network_traffic", i * 10, None),
                    ],
                ),
                ts,
                0,
            )
            .unwrap();
            ts += 1_000_000_000;
        }
        archive.finalize_single_rec(rec, (ts, 0)).unwrap();
    }

    /// The boundary, asserted rather than assumed: dendro refuses what `rez`
    /// writes, and refuses it as "not an archive" rather than by failing
    /// somewhere further in.
    ///
    /// If this ever starts failing, one of two things happened and both change
    /// the plan: dendro regained legacy reading, or `rez` started stamping its
    /// archives. Either is good news; neither should arrive unnoticed.
    #[test]
    fn dendro_does_not_read_an_archive_rez_wrote() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("legacy.rez");
        write_rez_archive(&path);

        assert!(
            matches!(
                dendro::archive::sniff(&path).unwrap(),
                dendro::archive::Sniff::NotAnArchive
            ),
            "a rez-written archive carries no dendro stamp, so it must sniff as \
             NotAnArchive — if it sniffs as Stamped, rez started writing dendro's \
             header and the migration in #1224 has partly happened"
        );

        assert!(
            dendro::archive::Archive::open(&path).is_err(),
            "dendro must refuse an unstamped archive; `crates/rez` is what reads \
             these, and that cannot change until the migration in #1224 lands"
        );
    }

    /// And the dependency is functional, not merely declared: an archive
    /// dendro writes, dendro reads.
    ///
    /// Worth having because the test above passes for a dependency that is
    /// broken in every way — "refuses to open a file" is what a non-working
    /// crate does too.
    #[test]
    fn dendro_round_trips_an_archive_of_its_own() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("native.dendro");

        {
            let mut archive = dendro::archive::ArchiveMut::create(&path).unwrap();
            archive
                .transaction(|tx| {
                    tx.insert_source(&dendro::archive::SourceMeta {
                        labels: [("source".to_string(), "rezolus".to_string())]
                            .into_iter()
                            .collect(),
                        metadata: Default::default(),
                        clock_anchor_wall_ns: ANCHOR as i64,
                    })?;
                    Ok(())
                })
                .unwrap();
        }

        assert!(
            matches!(
                dendro::archive::sniff(&path).unwrap(),
                dendro::archive::Sniff::Stamped { .. }
            ),
            "dendro stamps what it creates"
        );

        let archive = dendro::archive::Archive::open(&path).unwrap();
        let sources = archive.read_sources().unwrap();
        assert_eq!(sources.len(), 1);
        assert_eq!(
            sources[0].meta.labels.get("source").map(String::as_str),
            Some("rezolus")
        );
    }
}
