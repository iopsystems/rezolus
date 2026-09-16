//! Does `dendro` read an archive `rez` wrote?
//!
//! The read path is moving onto [`dendro`], the crate this format was
//! extracted into. Every `.rez` in existence — hindsight buffers, archived
//! incident captures, this repo's own fixtures — was written by `rez_sqlite`,
//! which stamps nothing: no `application_id`, no `user_version`, and a
//! `schema_version` TABLE plus a `recordings` table where dendro has a
//! `user_version` pragma and `sources`.
//!
//! dendro at the pinned rev reads those through compatibility views, and that
//! is the entire basis for swapping the read path without migrating anybody's
//! files. It is also a property of a *specific revision*: dendro's later
//! `efe127e` ("the stamp is the identity, and nothing else is read") deletes
//! the fallback, after which an unstamped file is refused as `NotAnArchive`.
//!
//! So this is a pin test, not a smoke test. If the dependency is ever moved
//! forward past that commit, these fail — which is the point, because the
//! alternative is discovering it when somebody cannot open a capture.

#[cfg(all(test, feature = "test-support"))]
mod tests {
    use crate::rez::recorder_tests_support::{counter, snap};
    use crate::rez_sqlite::RezDb;
    use crate::rez_v3_writer::{ManifestSeed, RezArchive, StreamRecorderV3};
    use std::path::Path;

    const ANCHOR: u64 = 1_700_000_000_000_000_000;

    /// Write a two-sampler archive with `rez`'s own writer, cleanly finalized.
    fn write_rez_archive(path: &Path) {
        let seed = ManifestSeed {
            labels: [
                ("source".to_string(), "rezolus".to_string()),
                ("host".to_string(), "test-host".to_string()),
            ]
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

    /// The load-bearing claim: dendro opens it at all.
    #[test]
    fn dendro_opens_an_archive_rez_wrote() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("compat.rez");
        write_rez_archive(&path);

        // Unstamped, which is what makes this the legacy path rather than the
        // ordinary one.
        assert!(
            matches!(
                dendro::db::sniff(&path).unwrap(),
                dendro::db::Sniff::Unstamped
            ),
            "a rez-written archive carries no dendro stamp; if this ever \
             reports Stamped, rez started writing dendro's header and this \
             test is no longer testing the legacy path"
        );

        dendro::db::Db::open(&path).expect(
            "dendro must read a legacy .rez — if this fails, the pinned rev \
             moved past the commit that removed compatibility views",
        );
    }

    /// Opening is not enough: the two must agree about what is inside.
    /// `recordings` -> `sources` and `all_samplers` -> `all_streams` are the
    /// renames the compatibility views bridge, so they are what to check.
    #[test]
    fn dendro_and_rez_agree_on_the_catalog() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("compat.rez");
        write_rez_archive(&path);

        let rez = RezDb::open(&path).unwrap();
        let den = dendro::db::Db::open(&path).unwrap();

        let rez_recs = rez.read_recordings().unwrap();
        let den_srcs = den.read_sources().unwrap();
        assert_eq!(
            rez_recs.len(),
            den_srcs.len(),
            "recordings/sources count must match"
        );

        for rec in &rez_recs {
            let mut rez_streams = rez.all_samplers(rec.id).unwrap();
            let mut den_streams = den.all_streams(rec.id).unwrap();
            rez_streams.sort();
            den_streams.sort();
            assert_eq!(
                rez_streams, den_streams,
                "the two readers must see the same streams for recording {}",
                rec.id
            );
            assert!(
                !rez_streams.is_empty(),
                "fixture should have produced streams"
            );

            for stream in &rez_streams {
                assert_eq!(
                    rez.read_segments(rec.id, stream).unwrap().len(),
                    den.read_segments(rec.id, stream).unwrap().len(),
                    "segment count must match for {stream}"
                );
                assert_eq!(
                    rez.total_rows(rec.id, stream).unwrap(),
                    den.total_rows(rec.id, stream).unwrap(),
                    "row count must match for {stream}"
                );
            }
        }
    }
}
