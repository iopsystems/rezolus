//! `rezolus parquet convert` — turn a raw msgpack recording into parquet.
//!
//! `rezolus record -f raw` writes concatenated msgpack snapshots: cheap to
//! write and cheap to finalize, which is what a long unattended capture needs.
//! Nothing downstream reads that format, though, so this is the offline
//! complement — the conversion `record` would have done at finalize time, run
//! after the fact against a file that has since been moved and compressed.
//!
//! See `docs/parquet-convert-design.md`.

use metriken_exposition::{MsgpackToParquet, ParquetOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// What a candidate input file actually is, decided by magic bytes rather than
/// by extension: these recordings get renamed and re-compressed on their way
/// off the machine that produced them, so the name is not evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InputKind {
    /// Concatenated msgpack snapshots — what this command converts.
    RawMsgpack,
    /// zstd-compressed; decompress before converting.
    Zstd,
    /// Already parquet.
    Parquet,
    /// A tar archive, most likely a `.rez`.
    Tar,
}

/// Classify `path` by its leading bytes.
fn sniff(path: &Path) -> io::Result<InputKind> {
    use std::io::Read;

    // 512 is one tar header block, which is the furthest any magic we check
    // for reaches (`ustar` at offset 257).
    let mut head = [0u8; 512];
    let mut f = std::fs::File::open(path)?;
    let mut filled = 0;
    while filled < head.len() {
        match f.read(&mut head[filled..])? {
            0 => break,
            n => filled += n,
        }
    }
    let head = &head[..filled];

    Ok(if head.starts_with(&[0x28, 0xb5, 0x2f, 0xfd]) {
        InputKind::Zstd
    } else if head.starts_with(b"PAR1") {
        InputKind::Parquet
    } else if head.len() >= 262 && &head[257..262] == b"ustar" {
        InputKind::Tar
    } else {
        InputKind::RawMsgpack
    })
}

/// Where the parquet lands when `-o` was not given: the input path with a
/// trailing `.zst` and then a trailing `.raw` removed, and `.parquet` appended.
/// `rezolus.raw.zst` becomes `rezolus.parquet`.
fn default_output_path(input: &Path) -> PathBuf {
    let mut stem = input.to_path_buf();
    for ext in ["zst", "raw"] {
        if stem
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case(ext))
        {
            stem = stem.with_extension("");
        }
    }

    let mut name = stem.into_os_string();
    name.push(".parquet");
    PathBuf::from(name)
}

/// How many snapshots to read when inferring the interval. Ten deltas is
/// plenty to find the cadence and costs a few hundred microseconds even on a
/// multi-gigabyte recording.
const INTERVAL_SAMPLE_SNAPSHOTS: usize = 11;

/// An inferred interval, plus anything about the sample that makes it a poor
/// description of the recording.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Inferred {
    pub interval: Duration,
    pub concern: Option<IntervalConcern>,
}

/// Why a stamped interval should not be trusted at face value. Both cases are
/// invisible in the output file, which is why they are reported at conversion.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum IntervalConcern {
    /// The median was below 1ms and the stamped value was clamped up to it, so
    /// every rate against this file understates the real cadence.
    Clamped { median: Duration },
    /// Too few sampled gaps cluster around the median for it to stand for a
    /// cadence — a stitched recording, or one full of restart gaps.
    Irregular {
        within: usize,
        of: usize,
        median: Duration,
    },
}

/// How far from the median a gap may fall and still count as on-cadence.
const CADENCE_TOLERANCE: f64 = 0.25;

/// The fraction of gaps that must be on-cadence for the median to describe the
/// recording.
///
/// With `CADENCE_TOLERANCE`, these two exist to stay silent on the case the
/// median was chosen to absorb: one stalled sample in ten gaps leaves 90%
/// on-cadence. Warning there would contradict the inference it reports on.
const CADENCE_QUORUM: f64 = 0.60;

/// Infer the recording's sampling interval from the first snapshots'
/// timestamps, rewinding the reader afterwards.
///
/// The recorder stamps `sampling_interval_ms` from its own `--interval`, which
/// a raw file does not carry. Defaulting to the recorder's 1s default would
/// silently mislabel a recording made at any other cadence, and every rate in
/// the viewer would be wrong by that ratio with nothing on screen to say so.
///
/// Uses the median delta, not the mean, so one stalled sample — a scheduling
/// hiccup, a slow scrape — does not drag the answer off the real cadence.
/// Returns `None` when there are fewer than two snapshots to compare.
fn infer_interval(reader: &mut (impl io::Read + io::Seek)) -> Option<Inferred> {
    use metriken_exposition::Snapshot;

    let mut times = Vec::with_capacity(INTERVAL_SAMPLE_SNAPSHOTS);
    {
        let mut buffered = io::BufReader::new(&mut *reader);
        while times.len() < INTERVAL_SAMPLE_SNAPSHOTS {
            // Any error ends the sample: end of stream on a short recording,
            // or malformed bytes that the conversion pass will report properly.
            let Ok(snapshot) = rmp_serde::from_read::<_, Snapshot>(&mut buffered) else {
                break;
            };
            times.push(snapshot.systemtime());
        }
    }
    let _ = reader.rewind();

    let mut deltas: Vec<Duration> = times
        .windows(2)
        .filter_map(|w| w[1].duration_since(w[0]).ok())
        .collect();
    if deltas.is_empty() {
        return None;
    }

    deltas.sort_unstable();
    let median = deltas[deltas.len() / 2];

    let on_cadence = deltas
        .iter()
        .filter(|d| {
            **d >= median.mul_f64(1.0 - CADENCE_TOLERANCE)
                && **d <= median.mul_f64(1.0 + CADENCE_TOLERANCE)
        })
        .count();

    // The clamp must stay: a sub-millisecond median would otherwise stamp
    // `sampling_interval_ms=0`. A measured cadence is clamped rather than
    // refused the way `interval_millis` refuses an explicit `--interval`, so
    // that such a recording still converts — with a concern attached, since
    // nothing in the output file would otherwise show that it was clamped.
    let ms = (median.as_nanos() as f64 / 1e6).round() as u64;

    // Clamping wins when both apply: it is the more specific fact, and the one
    // with no remedy.
    let concern = if median < Duration::from_millis(1) {
        Some(IntervalConcern::Clamped { median })
    } else if (on_cadence as f64) < CADENCE_QUORUM * deltas.len() as f64 {
        Some(IntervalConcern::Irregular {
            within: on_cadence,
            of: deltas.len(),
            median,
        })
    } else {
        None
    };

    Some(Inferred {
        interval: Duration::from_millis(ms.max(1)),
        concern,
    })
}

/// Everything the conversion needs beyond the two paths.
#[derive(Debug, Default)]
pub(crate) struct ConvertOptions {
    /// Explicit `--interval`; `None` means infer from the snapshots.
    pub interval: Option<Duration>,
    /// Validated JSON for the `systeminfo` footer key.
    pub systeminfo: Option<String>,
    /// Validated JSON for the `descriptions` footer key.
    pub descriptions: Option<String>,
    /// Repeatable `--metadata key=value`, in the order given.
    pub metadata: Vec<(String, String)>,
    /// Overwrite an existing output.
    pub force: bool,
}

/// What a successful conversion did, for the summary line.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Converted {
    pub rows: i64,
    pub interval: Duration,
    /// False when `--interval` supplied the value.
    pub interval_inferred: bool,
    /// Always `None` for an explicit `--interval`: the operator asserted the
    /// cadence, so the sample's shape is not evidence against it.
    pub concern: Option<IntervalConcern>,
}

#[derive(Debug)]
pub(crate) enum ConvertError {
    /// The input is already parquet — there is nothing to convert.
    InputIsParquet,
    /// The input is a tar archive, most likely a `.rez`.
    InputIsTar,
    /// The output exists and `--force` was not given.
    OutputExists(PathBuf),
    Io(io::Error),
    /// The msgpack stream could not be read, or parquet could not be written.
    Stream(String),
    /// A `--systeminfo` / `--descriptions` argument was not the JSON it must be.
    InvalidJson {
        what: &'static str,
        detail: String,
    },
    /// A `--metadata` argument was not `key=value`.
    BadMetadata(String),
    /// An explicit `--interval` below the millisecond resolution the
    /// `sampling_interval_ms` footer key can represent.
    IntervalTooSmall(Duration),
}

impl std::fmt::Display for ConvertError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InputIsParquet => write!(
                f,
                "input is already a parquet file; no conversion is needed"
            ),
            Self::InputIsTar => write!(
                f,
                "input is a tar archive (a .rez?), not a raw recording; \
                 a .rez cannot be converted to parquet"
            ),
            Self::OutputExists(p) => {
                write!(f, "output {} already exists (use --force)", p.display())
            }
            Self::Io(e) => write!(f, "{e}"),
            Self::Stream(e) => write!(f, "{e}"),
            Self::InvalidJson { what, detail } => write!(f, "{what} is not valid: {detail}"),
            Self::BadMetadata(arg) => {
                write!(f, "--metadata must be key=value, got {arg:?}")
            }
            Self::IntervalTooSmall(d) => write!(
                f,
                "--interval {d:?} is below 1ms; sampling_interval_ms records whole \
                 milliseconds and a sub-millisecond value would be stamped as 0"
            ),
        }
    }
}

impl std::error::Error for ConvertError {}

impl From<io::Error> for ConvertError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

/// Split one `--metadata key=value` argument.
///
/// Unlike `record`, which silently drops an argument it cannot split, a
/// malformed one is an error here: quietly producing a recording that is
/// missing the label you asked for is worse than refusing to start.
fn parse_metadata(arg: &str) -> Result<(String, String), ConvertError> {
    match arg.split_once('=') {
        Some((key, value)) if !key.is_empty() => Ok((key.to_string(), value.to_string())),
        _ => Err(ConvertError::BadMetadata(arg.to_string())),
    }
}

/// What a JSON argument has to look like to be accepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JsonShape {
    /// Any JSON value (`systeminfo` is an opaque blob to us).
    Any,
    /// An object whose values are all strings (`descriptions` is metric → help).
    StringMap,
}

/// Read and validate a JSON argument, from a file or from stdin when `source`
/// is `-`, mirroring how `annotate --systeminfo` takes its input.
///
/// Validation happens before the conversion starts: a typo in a descriptions
/// file should cost a second, not a full re-read of a multi-gigabyte recording.
fn load_json(source: &Path, shape: JsonShape, what: &'static str) -> Result<String, ConvertError> {
    let text = if source.as_os_str() == "-" {
        let mut buf = String::new();
        io::Read::read_to_string(&mut io::stdin(), &mut buf)?;
        buf
    } else {
        std::fs::read_to_string(source)?
    };

    let parsed: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| ConvertError::InvalidJson {
            what,
            detail: e.to_string(),
        })?;

    if shape == JsonShape::StringMap
        && !parsed
            .as_object()
            .is_some_and(|map| map.values().all(serde_json::Value::is_string))
    {
        return Err(ConvertError::InvalidJson {
            what,
            detail: "expected a JSON object mapping metric names to help strings".to_string(),
        });
    }

    // Verbatim, not re-serialized: what the agent captured is what the footer
    // should carry.
    Ok(text)
}

/// The interval stamped when a recording is too short to infer one from.
/// Matches `record`'s own `--interval` default.
const FALLBACK_INTERVAL: Duration = Duration::from_millis(1000);

/// The value stamped into `sampling_interval_ms`, rounded to the nearest whole
/// millisecond.
///
/// Below 1ms there is no honest answer, so the interval is refused: rounding
/// 500us up to 1ms would understate every rate computed against the file by
/// half, with nothing in the file to say so.
fn interval_millis(interval: Duration) -> Result<u64, ConvertError> {
    if interval < Duration::from_millis(1) {
        return Err(ConvertError::IntervalTooSmall(interval));
    }

    Ok((interval.as_nanos() as f64 / 1e6).round() as u64)
}

/// Assemble the converter with the file-level metadata keys
/// `recorder::build_parquet_converter` writes, so a converted recording is
/// indistinguishable from one `record -f parquet` produced.
///
/// `source` is stamped before the user's `--metadata`, which lets
/// `--metadata source=…` override it — the same precedence `record` gives.
fn build_converter(interval_ms: u64, opts: &ConvertOptions) -> MsgpackToParquet {
    let mut converter = MsgpackToParquet::with_options(
        ParquetOptions::new().max_batch_size(crate::parquet_metadata::MAX_ROW_GROUP_SIZE),
    )
    .metadata("sampling_interval_ms".to_string(), interval_ms.to_string())
    .metadata("source".to_string(), "rezolus".to_string());

    for (key, value) in &opts.metadata {
        converter = converter.metadata(key.clone(), value.clone());
    }
    if let Some(json) = &opts.systeminfo {
        converter = converter.metadata("systeminfo".to_string(), json.clone());
    }
    if let Some(json) = &opts.descriptions {
        converter = converter.metadata("descriptions".to_string(), json.clone());
    }

    converter
}

/// Decompress a zstd input into a temp file beside the output and hand back an
/// open handle on it.
///
/// The bytes have to land on disk because `convert_file_handle` needs `Seek`:
/// it makes one pass to build the schema across every snapshot and a second to
/// write rows. A zstd stream decoder cannot rewind.
///
/// The temp file goes next to the output rather than in `/tmp` because the
/// expansion is large — 13x on a sample recording — and `/tmp` is often a small
/// tmpfs. If there is room for the result there is room for the intermediate.
fn decompress_beside_output(
    input: &Path,
    out_dir: &Path,
) -> io::Result<(tempfile::NamedTempFile, std::fs::File)> {
    use io::Seek;

    let tmp = tempfile::NamedTempFile::new_in(out_dir)?;
    let mut sink = tmp.as_file().try_clone()?;
    let mut decoder = zstd::Decoder::new(std::fs::File::open(input)?)?;
    io::copy(&mut decoder, &mut sink)?;
    io::Write::flush(&mut sink)?;

    let mut handle = tmp.reopen()?;
    handle.rewind()?;
    Ok((tmp, handle))
}

/// Relax the staged file's 0600 to whatever plain file creation yields here.
///
/// `NamedTempFile` is deliberately private and `persist` keeps that mode, so a
/// converted recording would otherwise be unreadable by the group that can read
/// a `record`-written one.
///
/// The mode comes from a reference file created in the same directory rather
/// than from the process umask, which cannot be read without temporarily
/// setting it.
#[cfg(unix)]
fn apply_default_permissions(target: &Path, dir: &Path) -> io::Result<()> {
    let reference = dir.join(format!(".rezolus-convert-{}.perm", std::process::id()));
    let mode = {
        let _f = std::fs::File::create(&reference)?;
        std::fs::metadata(&reference)?.permissions()
    };
    let _ = std::fs::remove_file(&reference);

    std::fs::set_permissions(target, mode)
}

#[cfg(not(unix))]
fn apply_default_permissions(_target: &Path, _dir: &Path) -> io::Result<()> {
    Ok(())
}

/// Convert a raw recording at `input` into a parquet file at `output`.
fn convert_file(
    input: &Path,
    output: &Path,
    opts: &ConvertOptions,
) -> Result<Converted, ConvertError> {
    use io::Seek;

    if output.exists() && !opts.force {
        return Err(ConvertError::OutputExists(output.to_path_buf()));
    }

    // Validated before any reading or writing: an unusable --interval should
    // cost nothing, not a full decompress-and-convert pass.
    if let Some(explicit) = opts.interval {
        interval_millis(explicit)?;
    }

    match sniff(input)? {
        InputKind::Parquet => Err(ConvertError::InputIsParquet),
        InputKind::Tar => Err(ConvertError::InputIsTar),
        kind => {
            let out_dir = output
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(Path::new("."));

            // `_scratch` must stay bound until the conversion finishes:
            // dropping it deletes the decompressed intermediate that `source`
            // reads from. Holding it also cleans up on the error paths below.
            let _scratch;
            let mut source = if kind == InputKind::Zstd {
                let (tmp, handle) = decompress_beside_output(input, out_dir)?;
                _scratch = Some(tmp);
                handle
            } else {
                _scratch = None;
                std::fs::File::open(input)?
            };

            let (interval, interval_inferred, concern) = match opts.interval {
                Some(explicit) => (explicit, false, None),
                None => match infer_interval(&mut source) {
                    Some(got) => (got.interval, true, got.concern),
                    None => (FALLBACK_INTERVAL, true, None),
                },
            };
            // `infer_interval` rewinds too, but the conversion is wrong in a
            // way nothing downstream can see if the handle is mid-stream, so
            // the failure is checked here rather than assumed away.
            source.rewind()?;

            // Write through a temp file so a failure part-way leaves no
            // half-written parquet for the next command to pick up.
            let staged = tempfile::NamedTempFile::new_in(out_dir)?;
            let rows = build_converter(interval_millis(interval)?, opts)
                .convert_file_handle(source, staged.as_file().try_clone()?)
                .map_err(|e| ConvertError::Stream(e.to_string()))?;
            staged
                .persist(output)
                .map_err(|e| ConvertError::Io(e.error))?;
            apply_default_permissions(output, out_dir)?;

            Ok(Converted {
                rows,
                interval,
                interval_inferred,
                concern,
            })
        }
    }
}

/// Gather the options from parsed arguments, validating everything that can be
/// validated before the conversion starts reading.
fn options_from_args(args: &clap::ArgMatches) -> Result<ConvertOptions, ConvertError> {
    let metadata = args
        .get_many::<String>("metadata")
        .unwrap_or_default()
        .map(|s| parse_metadata(s))
        .collect::<Result<Vec<_>, _>>()?;

    let systeminfo = args
        .get_one::<PathBuf>("systeminfo")
        .map(|p| load_json(p, JsonShape::Any, "systeminfo"))
        .transpose()?;

    let descriptions = args
        .get_one::<PathBuf>("descriptions")
        .map(|p| load_json(p, JsonShape::StringMap, "descriptions"))
        .transpose()?;

    Ok(ConvertOptions {
        interval: args.get_one::<humantime::Duration>("interval").map(|d| **d),
        systeminfo,
        descriptions,
        metadata,
        force: args.get_flag("force"),
    })
}

pub(crate) fn run(args: &clap::ArgMatches) {
    let input = args.get_one::<PathBuf>("FILE").expect("FILE is required");
    let output = args
        .get_one::<PathBuf>("output")
        .cloned()
        .unwrap_or_else(|| default_output_path(input));

    let result = options_from_args(args).and_then(|opts| {
        let done = convert_file(input, &output, &opts)?;
        Ok((done, opts))
    });

    match result {
        Ok((done, opts)) => {
            let how = if done.interval_inferred {
                "inferred"
            } else {
                "given"
            };
            println!(
                "Wrote {} ({} rows, {:?} interval, {how})",
                output.display(),
                done.rows,
                done.interval,
            );
            // Advisory, on stderr, exit code unchanged: the stamped value is
            // the best reading of the recording, not a failure to produce one.
            match done.concern {
                Some(IntervalConcern::Clamped { median }) => eprintln!(
                    "warning: snapshots are {median:?} apart, faster than the whole \
                     milliseconds sampling_interval_ms can hold; it was stamped as \
                     {:?}, so rates against this file understate the real cadence. \
                     --interval cannot express this either.",
                    done.interval,
                ),
                Some(IntervalConcern::Irregular { within, of, median }) => eprintln!(
                    "warning: snapshot spacing is irregular — only {within} of {of} \
                     sampled gaps are within {:.0}% of the {median:?} median, so no \
                     single interval describes this recording. {:?} was stamped; pass \
                     --interval to set it yourself.",
                    CADENCE_TOLERANCE * 100.0,
                    done.interval,
                ),
                None => {}
            }
            // Say what is missing rather than letting it be discovered in the
            // viewer as absent help text and a blank hardware summary.
            let missing: Vec<&str> = [
                opts.systeminfo.is_none().then_some("systeminfo"),
                opts.descriptions.is_none().then_some("descriptions"),
            ]
            .into_iter()
            .flatten()
            .collect();
            if !missing.is_empty() {
                // `annotate` is only offered for systeminfo: it has no
                // --descriptions route, so naming it for both would send the
                // reader after a flag that does not exist.
                let recovery = if opts.systeminfo.is_none() {
                    " `rezolus parquet annotate <file> --systeminfo <path>` can add the \
                     hardware summary later; descriptions can only be set here."
                } else {
                    " Descriptions can only be set here, by reconverting with --force."
                };
                println!(
                    "Note: no {} in the output — a raw recording does not carry {}.{}",
                    missing.join(" or "),
                    if missing.len() == 1 { "it" } else { "them" },
                    recovery,
                );
            }
        }
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Seek, Write};

    /// Write `bytes` to a temp file and classify it.
    fn sniff_bytes(bytes: &[u8]) -> InputKind {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(bytes).unwrap();
        f.flush().unwrap();
        sniff(f.path()).unwrap()
    }

    /// A minimal well-formed raw stream: one msgpack snapshot.
    fn raw_bytes() -> Vec<u8> {
        // The first byte of a V2 snapshot is a msgpack fixarray header.
        vec![0x96, 0x92, 0xce, 0x6a, 0x7e, 0x0c, 0xc8]
    }

    #[test]
    fn sniff_detects_zstd() {
        assert_eq!(
            sniff_bytes(&[0x28, 0xb5, 0x2f, 0xfd, 0x00, 0x01]),
            InputKind::Zstd
        );
    }

    #[test]
    fn sniff_detects_parquet() {
        assert_eq!(sniff_bytes(b"PAR1\x15\x04"), InputKind::Parquet);
    }

    #[test]
    fn sniff_detects_tar() {
        // `ustar` lives at offset 257 in a tar header block.
        let mut bytes = vec![0u8; 512];
        bytes[..8].copy_from_slice(b"manifest");
        bytes[257..262].copy_from_slice(b"ustar");
        assert_eq!(sniff_bytes(&bytes), InputKind::Tar);
    }

    #[test]
    fn sniff_detects_raw_msgpack() {
        assert_eq!(sniff_bytes(&raw_bytes()), InputKind::RawMsgpack);
    }

    #[test]
    fn sniff_ignores_the_extension() {
        // A zstd file that was renamed to look plain, and a plain file that was
        // renamed to look compressed. Both are shaped like the real mistakes
        // that happen when recordings are copied between machines.
        let dir = tempfile::tempdir().unwrap();

        let lying_plain = dir.path().join("rezolus.raw");
        std::fs::write(&lying_plain, [0x28, 0xb5, 0x2f, 0xfd, 0x00]).unwrap();
        assert_eq!(sniff(&lying_plain).unwrap(), InputKind::Zstd);

        let lying_zst = dir.path().join("rezolus.raw.zst");
        std::fs::write(&lying_zst, raw_bytes()).unwrap();
        assert_eq!(sniff(&lying_zst).unwrap(), InputKind::RawMsgpack);
    }

    #[test]
    fn default_output_strips_zst_and_raw() {
        assert_eq!(
            default_output_path(Path::new("/logs/run7/rezolus.raw.zst")),
            PathBuf::from("/logs/run7/rezolus.parquet")
        );
    }

    #[test]
    fn default_output_strips_raw_alone() {
        assert_eq!(
            default_output_path(Path::new("rezolus.raw")),
            PathBuf::from("rezolus.parquet")
        );
    }

    #[test]
    fn default_output_appends_to_an_extensionless_name() {
        assert_eq!(
            default_output_path(Path::new("capture")),
            PathBuf::from("capture.parquet")
        );
    }

    #[test]
    fn default_output_keeps_an_unrecognized_extension() {
        // Only `.zst` and `.raw` are stripped. Anything else is kept, so the
        // output can never collide with the input we are still reading.
        assert_eq!(
            default_output_path(Path::new("capture.msgpack")),
            PathBuf::from("capture.msgpack.parquet")
        );
    }

    /// A raw stream: one msgpack snapshot per timestamp (nanoseconds), each
    /// carrying a single counter so the stream is shaped like a real one.
    fn raw_stream(timestamps_ns: &[u64]) -> Vec<u8> {
        use metriken_exposition::{Counter, Snapshot, SnapshotV2};
        use std::collections::HashMap;
        use std::time::SystemTime;

        let mut out = Vec::new();
        for (i, ts) in timestamps_ns.iter().enumerate() {
            let snap = Snapshot::V2(SnapshotV2 {
                systemtime: SystemTime::UNIX_EPOCH + Duration::from_nanos(*ts),
                duration: Duration::ZERO,
                metadata: HashMap::new(),
                counters: vec![Counter::new(
                    "cpu_cycles".to_string(),
                    i as u64,
                    [("metric".to_string(), "cpu_cycles".to_string())]
                        .into_iter()
                        .collect(),
                )],
                gauges: Vec::new(),
                histograms: Vec::new(),
            });
            out.extend_from_slice(&Snapshot::to_msgpack(&snap).unwrap());
        }
        out
    }

    /// Timestamps `count` apart by `step_ns`, starting at an arbitrary epoch.
    fn cadence(count: usize, step_ns: u64) -> Vec<u64> {
        (0..count as u64)
            .map(|i| 1_786_645_704_000_000_000 + i * step_ns)
            .collect()
    }

    /// The inferred interval, asserting the sample raised no concern.
    fn inferred_clean(bytes: Vec<u8>) -> Duration {
        let mut r = Cursor::new(bytes);
        let got = infer_interval(&mut r).expect("expected an interval");
        assert_eq!(got.concern, None, "expected no concern");
        got.interval
    }

    #[test]
    fn infer_interval_finds_one_second() {
        assert_eq!(
            inferred_clean(raw_stream(&cadence(5, 1_000_000_000))),
            Duration::from_millis(1000)
        );
    }

    #[test]
    fn infer_interval_finds_a_sub_second_cadence() {
        assert_eq!(
            inferred_clean(raw_stream(&cadence(5, 250_000_000))),
            Duration::from_millis(250)
        );
    }

    #[test]
    fn infer_interval_ignores_a_stalled_sample() {
        // Deltas of 1s, 1s, 3s, 1s. The mean would say 1.5s; the median says
        // 1s, which is the cadence the recorder was actually asked for.
        let base = 1_786_645_704_000_000_000u64;
        let ts = [
            base,
            base + 1_000_000_000,
            base + 2_000_000_000,
            base + 5_000_000_000,
            base + 6_000_000_000,
        ];
        // No concern: warning about the very sample the median absorbs would
        // contradict the inference. This pins CADENCE_TOLERANCE/QUORUM.
        assert_eq!(inferred_clean(raw_stream(&ts)), Duration::from_millis(1000));
    }

    #[test]
    fn infer_interval_flags_a_clamped_sub_millisecond_cadence() {
        // 500us apart: unrepresentable in sampling_interval_ms, stamped as 1ms.
        let mut r = Cursor::new(raw_stream(&cadence(5, 500_000)));
        let got = infer_interval(&mut r).unwrap();

        assert_eq!(got.interval, Duration::from_millis(1));
        assert_eq!(
            got.concern,
            Some(IntervalConcern::Clamped {
                median: Duration::from_micros(500)
            })
        );
    }

    #[test]
    fn infer_interval_flags_spacing_with_no_dominant_cadence() {
        // Half the gaps 1s, half 250ms — a recording stitched from two sources.
        // Whatever the median lands on describes only half the file.
        let base = 1_786_645_704_000_000_000u64;
        let mut ts = vec![base];
        for step in [
            1_000_000_000u64,
            250_000_000,
            1_000_000_000,
            250_000_000,
            1_000_000_000,
            250_000_000,
        ] {
            ts.push(ts.last().unwrap() + step);
        }
        let mut r = Cursor::new(raw_stream(&ts));
        let got = infer_interval(&mut r).unwrap();

        match got.concern {
            Some(IntervalConcern::Irregular { within, of, .. }) => {
                assert_eq!(of, 6, "six gaps sampled");
                assert!(within < 4, "expected a minority on-cadence, got {within}");
            }
            other => panic!("expected Irregular, got {other:?}"),
        }
    }

    #[test]
    fn infer_interval_returns_none_for_a_single_snapshot() {
        let mut r = Cursor::new(raw_stream(&cadence(1, 1_000_000_000)));
        assert!(infer_interval(&mut r).is_none());
    }

    #[test]
    fn an_explicit_interval_never_raises_a_concern() {
        // The operator asserted the cadence; the sample's shape is not evidence
        // against it, and interval_millis already refused what it could not
        // represent.
        let dir = tempfile::tempdir().unwrap();
        let base = 1_786_645_704_000_000_000u64;
        let ts = [base, base + 1_000_000_000, base + 30_000_000_000];
        let input = write_input(dir.path(), "r.raw", &raw_stream(&ts));
        let output = dir.path().join("out.parquet");

        let opts = ConvertOptions {
            interval: Some(Duration::from_millis(1000)),
            ..Default::default()
        };
        let done = convert_file(&input, &output, &opts).unwrap();

        assert_eq!(done.concern, None);
    }

    #[test]
    fn a_concern_still_produces_a_converted_file() {
        // The warning is advisory: the stamped value is the best reading
        // available, not a failure.
        let dir = tempfile::tempdir().unwrap();
        let input = write_input(dir.path(), "r.raw", &raw_stream(&cadence(5, 500_000)));
        let output = dir.path().join("out.parquet");

        let done = convert_file(&input, &output, &ConvertOptions::default()).unwrap();

        assert!(done.concern.is_some());
        assert_eq!(
            footer(&output)
                .get("sampling_interval_ms")
                .map(String::as_str),
            Some("1")
        );
    }

    #[test]
    fn infer_interval_rewinds_for_the_conversion_pass() {
        // The converter reads the same handle afterwards and needs every
        // snapshot, including the ones inference consumed.
        let bytes = raw_stream(&cadence(3, 1_000_000_000));
        let mut r = Cursor::new(bytes.clone());
        infer_interval(&mut r);
        assert_eq!(r.stream_position().unwrap(), 0);
    }

    /// File-level metadata of a parquet file, as a lookup.
    fn footer(path: &Path) -> std::collections::HashMap<String, String> {
        crate::parquet_tools::read_file_metadata(path)
            .unwrap()
            .into_iter()
            .filter_map(|kv| kv.value.map(|v| (kv.key, v)))
            .collect()
    }

    /// Write `bytes` into `dir` as `name`.
    fn write_input(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, bytes).unwrap();
        p
    }

    #[test]
    fn converts_a_plain_raw_recording() {
        let dir = tempfile::tempdir().unwrap();
        let input = write_input(
            dir.path(),
            "rezolus.raw",
            &raw_stream(&cadence(3, 1_000_000_000)),
        );
        let output = dir.path().join("out.parquet");

        let done = convert_file(&input, &output, &ConvertOptions::default()).unwrap();

        assert_eq!(done.rows, 3);
        assert_eq!(done.interval, Duration::from_millis(1000));
        assert!(done.interval_inferred);

        let meta = footer(&output);
        assert_eq!(
            meta.get("sampling_interval_ms").map(String::as_str),
            Some("1000")
        );
        assert_eq!(meta.get("source").map(String::as_str), Some("rezolus"));
    }

    #[cfg(unix)]
    #[test]
    fn output_permissions_match_a_normally_created_file() {
        // The conversion stages through a temp file, which is 0600. A recording
        // that lands unreadable by the group that has to analyze it is a
        // regression against `record`, which creates its output normally.
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let input = write_input(dir.path(), "r.raw", &raw_stream(&cadence(2, 1_000_000_000)));
        let output = dir.path().join("out.parquet");

        convert_file(&input, &output, &ConvertOptions::default()).unwrap();

        let reference = dir.path().join("reference");
        std::fs::File::create(&reference).unwrap();
        let want = std::fs::metadata(&reference).unwrap().permissions().mode() & 0o777;
        let got = std::fs::metadata(&output).unwrap().permissions().mode() & 0o777;

        assert_eq!(got, want, "expected {want:o}, got {got:o}");
    }

    #[test]
    fn converts_a_zstd_compressed_recording() {
        let dir = tempfile::tempdir().unwrap();
        let raw = raw_stream(&cadence(4, 1_000_000_000));
        let input = write_input(
            dir.path(),
            "rezolus.raw.zst",
            &zstd::encode_all(Cursor::new(&raw), 3).unwrap(),
        );
        let output = dir.path().join("out.parquet");

        let done = convert_file(&input, &output, &ConvertOptions::default()).unwrap();

        assert_eq!(done.rows, 4);
        assert_eq!(
            footer(&output).get("source").map(String::as_str),
            Some("rezolus")
        );
    }

    #[test]
    fn stamps_systeminfo_descriptions_and_user_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let input = write_input(dir.path(), "r.raw", &raw_stream(&cadence(2, 1_000_000_000)));
        let output = dir.path().join("out.parquet");

        let opts = ConvertOptions {
            systeminfo: Some(r#"{"os":"linux"}"#.to_string()),
            descriptions: Some(r#"{"cpu_cycles":"CPU cycles"}"#.to_string()),
            metadata: vec![("run".to_string(), "boat-7".to_string())],
            ..Default::default()
        };
        convert_file(&input, &output, &opts).unwrap();

        let meta = footer(&output);
        assert_eq!(
            meta.get("systeminfo").map(String::as_str),
            Some(r#"{"os":"linux"}"#)
        );
        assert_eq!(
            meta.get("descriptions").map(String::as_str),
            Some(r#"{"cpu_cycles":"CPU cycles"}"#)
        );
        assert_eq!(meta.get("run").map(String::as_str), Some("boat-7"));
    }

    #[test]
    fn user_metadata_can_override_the_source() {
        let dir = tempfile::tempdir().unwrap();
        let input = write_input(dir.path(), "r.raw", &raw_stream(&cadence(2, 1_000_000_000)));
        let output = dir.path().join("out.parquet");

        let opts = ConvertOptions {
            metadata: vec![("source".to_string(), "llm-perf".to_string())],
            ..Default::default()
        };
        convert_file(&input, &output, &opts).unwrap();

        let meta = footer(&output);
        assert_eq!(meta.get("source").map(String::as_str), Some("llm-perf"));
    }

    #[test]
    fn explicit_interval_overrides_inference() {
        let dir = tempfile::tempdir().unwrap();
        // Snapshots are 1s apart, but the operator says the recording is 5s.
        let input = write_input(dir.path(), "r.raw", &raw_stream(&cadence(3, 1_000_000_000)));
        let output = dir.path().join("out.parquet");

        let opts = ConvertOptions {
            interval: Some(Duration::from_millis(5000)),
            ..Default::default()
        };
        let done = convert_file(&input, &output, &opts).unwrap();

        assert_eq!(done.interval, Duration::from_millis(5000));
        assert!(!done.interval_inferred);
        assert_eq!(
            footer(&output)
                .get("sampling_interval_ms")
                .map(String::as_str),
            Some("5000")
        );
    }

    #[test]
    fn sub_millisecond_interval_is_rejected() {
        // `sampling_interval_ms` holds whole milliseconds, so 500us would be
        // stamped as 0 and every rate computed against this file would be
        // divided by a zero interval. Refuse rather than write that.
        let dir = tempfile::tempdir().unwrap();
        let input = write_input(dir.path(), "r.raw", &raw_stream(&cadence(2, 500_000)));
        let output = dir.path().join("out.parquet");

        let opts = ConvertOptions {
            interval: Some(Duration::from_micros(500)),
            ..Default::default()
        };
        let err = convert_file(&input, &output, &opts).unwrap_err();

        assert!(
            matches!(err, ConvertError::IntervalTooSmall(_)),
            "got {err:?}"
        );
        assert!(!output.exists(), "nothing should be written");
    }

    #[test]
    fn sub_millisecond_precision_rounds_to_the_nearest_millisecond() {
        // 1.5ms is representable-ish; truncation would call it 1ms.
        let dir = tempfile::tempdir().unwrap();
        let input = write_input(dir.path(), "r.raw", &raw_stream(&cadence(2, 1_500_000)));
        let output = dir.path().join("out.parquet");

        let opts = ConvertOptions {
            interval: Some(Duration::from_micros(1500)),
            ..Default::default()
        };
        convert_file(&input, &output, &opts).unwrap();

        assert_eq!(
            footer(&output)
                .get("sampling_interval_ms")
                .map(String::as_str),
            Some("2")
        );
    }

    #[test]
    fn single_snapshot_recording_falls_back_to_one_second() {
        let dir = tempfile::tempdir().unwrap();
        let input = write_input(dir.path(), "r.raw", &raw_stream(&cadence(1, 1_000_000_000)));
        let output = dir.path().join("out.parquet");

        let done = convert_file(&input, &output, &ConvertOptions::default()).unwrap();

        assert_eq!(done.interval, Duration::from_millis(1000));
        assert!(done.interval_inferred);
    }

    #[test]
    fn refuses_to_overwrite_an_existing_output() {
        let dir = tempfile::tempdir().unwrap();
        let input = write_input(dir.path(), "r.raw", &raw_stream(&cadence(2, 1_000_000_000)));
        let output = write_input(dir.path(), "out.parquet", b"precious");

        let err = convert_file(&input, &output, &ConvertOptions::default()).unwrap_err();

        assert!(matches!(err, ConvertError::OutputExists(_)), "got {err:?}");
        assert_eq!(std::fs::read(&output).unwrap(), b"precious");
    }

    #[test]
    fn force_overwrites_an_existing_output() {
        let dir = tempfile::tempdir().unwrap();
        let input = write_input(dir.path(), "r.raw", &raw_stream(&cadence(2, 1_000_000_000)));
        let output = write_input(dir.path(), "out.parquet", b"stale");

        let opts = ConvertOptions {
            force: true,
            ..Default::default()
        };
        convert_file(&input, &output, &opts).unwrap();

        assert_eq!(
            footer(&output).get("source").map(String::as_str),
            Some("rezolus")
        );
    }

    #[test]
    fn rejects_a_parquet_input() {
        let dir = tempfile::tempdir().unwrap();
        let input = write_input(dir.path(), "already.parquet", b"PAR1\x00\x00");
        let err = convert_file(
            &input,
            &dir.path().join("out.parquet"),
            &ConvertOptions::default(),
        )
        .unwrap_err();
        assert!(matches!(err, ConvertError::InputIsParquet), "got {err:?}");
    }

    #[test]
    fn rejects_a_rez_archive_input() {
        let dir = tempfile::tempdir().unwrap();
        let mut tar = vec![0u8; 512];
        tar[..8].copy_from_slice(b"manifest");
        tar[257..262].copy_from_slice(b"ustar");
        let input = write_input(dir.path(), "capture.rez", &tar);

        let err = convert_file(
            &input,
            &dir.path().join("out.parquet"),
            &ConvertOptions::default(),
        )
        .unwrap_err();
        assert!(matches!(err, ConvertError::InputIsTar), "got {err:?}");
    }

    #[test]
    fn a_truncated_stream_leaves_no_output_behind() {
        let dir = tempfile::tempdir().unwrap();
        let mut bytes = raw_stream(&cadence(3, 1_000_000_000));
        bytes.truncate(bytes.len() - 5);
        let input = write_input(dir.path(), "cut.raw", &bytes);
        let output = dir.path().join("out.parquet");

        let err = convert_file(&input, &output, &ConvertOptions::default()).unwrap_err();

        assert!(matches!(err, ConvertError::Stream(_)), "got {err:?}");
        assert!(
            !output.exists(),
            "a failed conversion must not leave a partial parquet behind"
        );
    }

    #[test]
    fn parse_metadata_splits_key_and_value() {
        assert_eq!(
            parse_metadata("run=boat-7").unwrap(),
            ("run".to_string(), "boat-7".to_string())
        );
    }

    #[test]
    fn parse_metadata_splits_on_the_first_equals_only() {
        // Values legitimately contain '=' — a base64 tag, a query string.
        assert_eq!(
            parse_metadata("tag=a=b=c").unwrap(),
            ("tag".to_string(), "a=b=c".to_string())
        );
    }

    #[test]
    fn parse_metadata_allows_an_empty_value() {
        assert_eq!(
            parse_metadata("note=").unwrap(),
            ("note".to_string(), String::new())
        );
    }

    #[test]
    fn parse_metadata_rejects_a_missing_equals() {
        let err = parse_metadata("source rezolus").unwrap_err();
        assert!(matches!(err, ConvertError::BadMetadata(_)), "got {err:?}");
    }

    #[test]
    fn parse_metadata_rejects_an_empty_key() {
        let err = parse_metadata("=value").unwrap_err();
        assert!(matches!(err, ConvertError::BadMetadata(_)), "got {err:?}");
    }

    #[test]
    fn load_json_returns_the_file_verbatim() {
        let dir = tempfile::tempdir().unwrap();
        let p = write_input(dir.path(), "sysinfo.json", br#"{"os":"linux","cpus":8}"#);

        let got = load_json(&p, JsonShape::Any, "systeminfo").unwrap();

        // Verbatim, not re-serialized: the agent's blob goes into the footer
        // exactly as it was captured.
        assert_eq!(got, r#"{"os":"linux","cpus":8}"#);
    }

    #[test]
    fn load_json_rejects_malformed_json() {
        let dir = tempfile::tempdir().unwrap();
        let p = write_input(dir.path(), "sysinfo.json", b"{not json");

        let err = load_json(&p, JsonShape::Any, "systeminfo").unwrap_err();

        assert!(
            matches!(
                err,
                ConvertError::InvalidJson {
                    what: "systeminfo",
                    ..
                }
            ),
            "got {err:?}"
        );
    }

    #[test]
    fn load_json_accepts_a_descriptions_map() {
        let dir = tempfile::tempdir().unwrap();
        let p = write_input(dir.path(), "d.json", br#"{"cpu_cycles":"CPU cycles"}"#);
        assert!(load_json(&p, JsonShape::StringMap, "descriptions").is_ok());
    }

    #[test]
    fn load_json_rejects_descriptions_that_are_not_a_map() {
        let dir = tempfile::tempdir().unwrap();
        let p = write_input(dir.path(), "d.json", br#"["cpu_cycles"]"#);

        let err = load_json(&p, JsonShape::StringMap, "descriptions").unwrap_err();

        assert!(
            matches!(
                err,
                ConvertError::InvalidJson {
                    what: "descriptions",
                    ..
                }
            ),
            "got {err:?}"
        );
    }

    #[test]
    fn load_json_rejects_descriptions_with_non_string_values() {
        // Valid JSON, wrong shape: the viewer reads these as help text and
        // would get a number.
        let dir = tempfile::tempdir().unwrap();
        let p = write_input(dir.path(), "d.json", br#"{"cpu_cycles":42}"#);

        let err = load_json(&p, JsonShape::StringMap, "descriptions").unwrap_err();

        assert!(
            matches!(
                err,
                ConvertError::InvalidJson {
                    what: "descriptions",
                    ..
                }
            ),
            "got {err:?}"
        );
    }

    #[test]
    fn load_json_reports_a_missing_file() {
        let err = load_json(
            Path::new("/nonexistent/x.json"),
            JsonShape::Any,
            "systeminfo",
        )
        .unwrap_err();
        assert!(matches!(err, ConvertError::Io(_)), "got {err:?}");
    }

    #[test]
    fn sniff_reports_a_short_file_as_raw() {
        // Too short to match any magic. It will fail later as a malformed
        // snapshot, with a message about the stream rather than about the file
        // type — sniff's job is only to route.
        assert_eq!(sniff_bytes(&[0x96]), InputKind::RawMsgpack);
    }
}
