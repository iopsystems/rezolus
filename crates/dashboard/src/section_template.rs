//! Section templates: dashboard sections as a JSON document, expanded
//! against a recording's metric catalog.
//!
//! A section template (schema 1) describes what the Rust section generators
//! build: views of groups, subgroups and plots, with the choices a generator
//! makes from the metric catalog expressed as predicates. It is a document
//! separate from [`ServiceExtension`](crate::ServiceExtension): a service
//! extension lists KPI queries, a section template describes whole sections.
//!
//! # Document
//!
//! ```json
//! {
//!   "schema": 1,
//!   "revision": "rezolus 6.0.0",
//!   "params": ["throughput"],
//!   "let": { "multi_cpu": { "pred": { "distinct_gt": { "metrics": ["cpu_usage"], "by": ["id"], "n": 1 } } } },
//!   "sections": [
//!     { "builtin": "query" },
//!     { "name": "CPU", "route": "/cpu", "let": {}, "metadata": {}, "groups": [ ... ] }
//!   ]
//! }
//! ```
//!
//! - `revision` is informational: which producer build the template
//!   describes.
//! - `params` names the viewer parameters the template reads. A parameter
//!   the viewer sets that is not declared here is not visible to the
//!   template.
//! - `sections` lists at most [`MAX_SECTIONS`] sections in navigation
//!   order. `{"builtin": "query"}` places the query explorer at `/query`.
//!   A section's `name` is 1 to [`MAX_SECTION_NAME_BYTES`] bytes. Its
//!   `route` is `/` and then segments of `a-z`, `0-9`, `_` and `-`
//!   separated by single `/`, at most [`MAX_SECTION_NAME_BYTES`] bytes. No
//!   two sections share a route.
//! - A section's `metadata` maps each key of the view's metadata to
//!   `{"value": V}`, the JSON value `V`, or `{"computed": "gpu_selector",
//!   "probe": [..]}`, the GPU selector over the probe metrics, present when
//!   they name more than one GPU.
//! - A group is `{"name", "id", "metadata", "subgroups"}`, where `metadata`
//!   is a map of JSON values copied as given. A subgroup is `{"name",
//!   "description", "plots"}`, both strings optional.
//!
//! Every node and every struct in this description is a JSON object; an
//! array in its place is refused. No object anywhere in the document,
//! metadata values included, may repeat a key.
//!
//! # Lets
//!
//! `let` maps a name to one of:
//!
//! - a string;
//! - `{"first_present": {"list": [..], "default": ..}}`: the first metric
//!   of `list` the catalog has, else `default`;
//! - `{"pred": P}`: whether predicate `P` holds.
//!
//! Lets are evaluated when a section is expanded, with only the parameters
//! in scope: a let cannot use another let, and a `var` or `${..}` in a let
//! that names one is refused. Document lets are visible in every section.
//! A section let cannot reuse a document let's name, and no let can reuse a
//! parameter's.
//!
//! # Nodes
//!
//! A list of groups, subgroups or plots holds elements and two kinds of
//! node, which expand into elements:
//!
//! - `{"when": P, "then": [..], "else": [..]}`: `then` when predicate `P`
//!   holds, else `else` (optional).
//! - `{"repeat": {"var": "op", "in": V}, "body": [..]}`: `body` once per
//!   value of `V`, with the variable bound for that pass. `V` is
//!   `{"values": [..]}`, a list of strings (bound as `${op}`) or of objects
//!   of strings with the same fields (bound as `${op.field}`), or
//!   `{"label_values": {"metric": .., "label": .., "sort": "str"|"int"}}`,
//!   the values of `label` on `metric`'s series, bound as `${op}`. `str`
//!   (the default) sorts them as strings; `int` puts those that parse as
//!   integers first, in numeric order with ties (`"01"` and `"1"`) in
//!   string order, and the rest after them as strings. A repeat variable
//!   cannot reuse a name already in scope. An empty `values` list binds
//!   nothing, so `parse` refuses a body that uses its variable.
//!
//! A plot list also takes:
//!
//! - `{"split": {"when": P, "plots": [A, B]}}`: `A` and `B` side by side
//!   when `P` holds, else `A` alone at full width. Neither plot takes a
//!   `width`.
//! - `{"rate_mean": {"title", "id", "selector", "rate", "mean_unit"}}`:
//!   [`SubGroup::histogram_rate_mean`]. `rate` is a rate query over a
//!   separate counter, or absent to derive the rate from the histogram;
//!   `mean_unit` is `time` or `bytes`.
//!
//! A group or subgroup left with nothing in it after expansion is omitted.
//!
//! # Predicates
//!
//! - `{"present": m}`: metric `m` exists as a counter, gauge or histogram.
//! - `{"has_counter": m}`: `m` exists as a counter.
//! - `{"has_label": {"metric", "label", "value"}}`: some series of `metric`
//!   has `label`, equal to `value` when given.
//! - `{"distinct_gt": {"metrics", "by", "n", "each", "int"}}`: more than
//!   `n` distinct values of the `by` labels. With `"each": true`, `by` is
//!   one label, `int` is not given, and any one metric's values of the
//!   label count on their own (as [`metric_unique_label_count`]). Without
//!   it, label tuples are counted across the counter and gauge series of
//!   every metric, with the labels listed in `int` (each one of the `by`
//!   labels) compared as integers (`"01"` and `"1"` are one value),
//!   skipping a series that lacks an `int` label or whose `int` label does
//!   not parse; a missing non-`int` label counts as absent. Tuples are
//!   counted by a 128-bit hash: two distinct tuples whose hashes collide
//!   would count as one, which at that width does not happen in practice.
//! - `{"any": [..]}`, `{"all": [..]}`, `{"not": P}`.
//! - `{"var": name}`: a `pred` let.
//! - `{"param": name}`: the viewer set the declared parameter.
//!
//! # Plots
//!
//! A plot is built with the same constructors and options the generators
//! use, so a template that makes the same calls renders the same JSON:
//! `kind` is `gauge` or `counter` (which need a `unit`), `histogram` (a
//! `unit` and a `subtype`, `percentiles` or `buckets`), or `latency`
//! ([`PlotOpts::histogram_latency`], which takes neither). `unit` is one of
//! `count`, `rate`, `time`, `bytes`, `datarate`, `bitrate`, `percentage`,
//! `frequency`, `power`. `width` is absent or `full`. `range` is `[min,
//! max]` with `min` below `max`, and cannot be given with
//! `percentage_range`. The other fields (`unit_system`,
//! `description`, `percentiles`, `axis_label`, `row_label`, `style`,
//! `log_scale`, `query_total`) map to the [`PlotOpts`] option of that name.
//!
//! # Substitution
//!
//! `${name}` and `${name.field}` are replaced in every string field except
//! these: names (`let` keys, `params` entries, a repeat's `var`, a `var` or
//! `param` predicate's operand, metadata keys), enumerated values (`kind`,
//! `unit`, `subtype`, `width`, `mean_unit`, `sort`, `computed`,
//! `builtin`), `revision`, a section's `name` and `route`, repeat `values`
//! entries, and `metadata` values. A name is a repeat binding, a string or
//! `first_present` let, or a declared parameter; [`SectionTemplate::parse`]
//! refuses a name that is none of these where it appears. A `${param}` the
//! viewer did not set is an error where it is expanded; lets are expanded
//! with their section, whether or not the section uses them. There is no
//! escape for a literal `${`.
//!
//! A value bound by `label_values` comes from the catalog. In a `query`,
//! `query_total`, or a `rate_mean`'s `selector` or `rate` (a PromQL
//! field), a reference to one must sit inside a double-quoted string
//! (`{k="${v}"}`). `parse` refuses one bare, in a single-quoted or
//! backtick string, or after a `#` (a comment runs to the end of the
//! field, since a template string holds no newline), and refuses any `${`
//! that follows a `\` in a PromQL field. Inside the double-quoted
//! string, the value's `\`, `"`, newlines and carriage returns are escaped
//! as in a PromQL string literal; other control characters are inserted as
//! given. In a regex matcher (`=~"${v}"`, `!~"${v}"`) the value is
//! still read as a regex, so a value such as `.*` matches every series; a
//! template that wants an exact match uses `=`. In other fields, and for
//! every other binding and parameter, the value is inserted as given.
//!
//! # Limits
//!
//! [`SectionTemplate::parse`] refuses a document with an array or object
//! of more than [`MAX_ENTRIES`] entries, a string or key over
//! [`MAX_STRING_BYTES`], a string holding a C0 control character (U+0000
//! to U+001F), a name over [`MAX_NAME_BYTES`], more than
//! [`MAX_SECTIONS`] sections, or a `distinct_gt` with more than
//! [`MAX_BY_LABELS`] `by` labels. It reads the document in time and memory
//! linear in its size, but has no limit on that size: a caller reading
//! templates from an untrusted source caps the document first. The
//! Rezolus template is about 106 KB compact, so a viewer reading templates
//! from a stream handshake can refuse a document over 1 MiB.
//!
//! The limits below apply to each section's expansion on its own, so a
//! document of [`MAX_SECTIONS`] sections can cost that many times as much.
//! Expanding one section, its lets included:
//!
//! - visits at most [`MAX_NODES`] nodes, counting elements, `when` and
//!   `repeat` nodes, repeat passes, and each field a pass over objects
//!   binds;
//! - produces at most [`MAX_TEXT_BYTES`] bytes of output, counted in
//!   memory: the length of each string it builds, checked before each
//!   piece is appended; the size of the metadata and percentiles it copies
//!   (each JSON value counts as `size_of::<Value>()` plus its strings and
//!   keys); the size of each group, subgroup and plot struct it creates;
//!   and the copies [`SubGroup::histogram_rate_mean`] makes of its
//!   selector, title and id. A C0 control character, which a catalog value
//!   or a parameter can bring in (template strings cannot hold one),
//!   counts as the six bytes JSON writes it as. A percentile counts as 8
//!   bytes and serializes as up to 25, so the serialized view is at most
//!   about three times the count;
//! - spends at most [`MAX_FUEL`] units of fuel, which count the input it
//!   processes: one unit per iteration of every loop (a node, a predicate,
//!   a reference, a series walked, a counter name, a binding), one per
//!   byte it scans, compares or copies (template text, label names looked
//!   up, label values, bound values and names, output), and
//!   [`SOURCE_CALL_FUEL`] per call into the [`MetricsSource`]. Each
//!   metric's series of each type and the counter names are read from the
//!   source once per section expansion and then from a memo, so a metric
//!   named many times costs one source read.
//!
//! The invariant the fuel counter rests on: every loop in expansion
//! charges at least one unit per iteration, and every string it copies,
//! scans or compares charges at least its length once. A lookup in an
//! ordered map charges the key's bytes once per level of the map, and one
//! in a hash map twice; a sort is charged per level of comparisons. A comment at each loop in the code names its
//! charge.
//!
//! Measured on a 9.6-hour, 1.3 GB recording of a 16-CPU host opened as an
//! archive (series of 5 to 11 labels, 112 to 211 bytes each; most of
//! `task_cpu_usage`'s carry 10), after the reader has read each metric
//! once: the slowest construction found that runs to the fuel limit (a
//! `distinct_gt` with `each` over 4096 metric names, on each of 8100
//! repeat passes) takes 0.34 s, so a document of [`MAX_SECTIONS`] such
//! sections takes about 22 s to expand in full. The first read of a
//! metric from a cold archive is outside the fuel count and is paid once
//! per process: 2.5 s for `task_cpu_usage` on that recording. The Rezolus
//! template's sixteen sections take 54 ms together on that recording when
//! cold and 0.4 ms when warm.
//!
//! The Rezolus template's headroom: on that recording its largest section
//! spends 45,926 units (`/cgroups`; `/cpu` 42,855). Measured by
//! duplicating the recording's per-CPU series with new ids up to 4096
//! CPUs, `/cpu` grows by 332 to 375 units per CPU and `/scheduler` by 165
//! to 186 (a little more per CPU as the label maps' lookups deepen), so
//! `/cpu` would reach the limit near 40,000 CPUs; no other section grows
//! with CPU count. The bench test catalog grows by 44 and 12 units per
//! CPU, because its series do not carry the recording's five provenance
//! labels (`endpoint`, `exported_source`, `host`, `sampler`, `source`,
//! about 110 bytes per series). On the test catalog of every metric the
//! generators name, widened to 256 devices of every kind with ten labels
//! per series, the largest section is `/network` at 2.3 million units
//! (about 9 million at 1024 devices).
//!
//! A section that would exceed any of these is refused with an error of
//! kind [`TemplateErrorKind::Limit`]; the other sections are unaffected.
//! `parse` reports the first problem it finds, not all of them, and an
//! error quotes at most 200 characters of the text it names.
//!
//! # Versioning
//!
//! `schema` versions the language. An expander handles every schema up to
//! [`MAX_SCHEMA`]. [`SectionTemplate::parse`] reads `schema` before
//! anything else, so a document with a larger schema is refused as such
//! whatever else it holds. A document with an unknown field or node
//! anywhere, or that fails any other check `parse` makes in any branch, is
//! refused whole, and the caller uses another template for that source.

use std::cell::{Cell, OnceCell, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::rc::Rc;

use serde::de::{DeserializeOwned, Error as _};
use serde::{Deserialize, Deserializer};
use serde_json::Value;

use crate::MetricsSource;
use crate::plot::*;

/// The highest schema this expander handles.
pub const MAX_SCHEMA: u32 = 1;

/// The most nodes one section's expansion visits.
pub const MAX_NODES: usize = 20_000;

/// The most bytes of output one section's expansion produces; see the
/// module docs for how each is counted.
pub const MAX_TEXT_BYTES: usize = 4 << 20;

/// The most fuel one section's expansion spends; see the module docs for
/// what each unit is.
pub const MAX_FUEL: usize = 16 << 20;

/// The most sections a document holds.
pub const MAX_SECTIONS: usize = 64;

/// The most `by` labels a `distinct_gt` names.
pub const MAX_BY_LABELS: usize = 16;

/// The longest section name or route, in bytes.
pub const MAX_SECTION_NAME_BYTES: usize = 64;

/// The most entries in any JSON array or object of a document.
pub const MAX_ENTRIES: usize = 4096;

/// The longest string (or object key) in a document, in bytes.
pub const MAX_STRING_BYTES: usize = 16 << 10;

/// The longest let, parameter, repeat variable or field name, in bytes.
pub const MAX_NAME_BYTES: usize = 64;

/// A parsed and checked section template document.
#[derive(Debug, Clone)]
pub struct SectionTemplate {
    schema: u32,
    revision: Option<String>,
    params: BTreeSet<String>,
    lets: BTreeMap<String, LetDef>,
    sections: Vec<SectionDef>,
}

struct Document {
    revision: Option<String>,
    params: Vec<String>,
    lets: BTreeMap<String, LetDef>,
    /// Read one at a time, so an error names the section.
    sections: Vec<Value>,
}

#[derive(Debug, Clone)]
enum SectionDef {
    /// `{"builtin": "query"}`: the query explorer.
    Builtin,
    Section(Box<SectionBody>),
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum Builtin {
    Query,
}

#[derive(Debug, Clone)]
struct SectionBody {
    name: String,
    route: String,
    lets: BTreeMap<String, LetDef>,
    metadata: BTreeMap<String, MetaDef>,
    groups: Vec<Item<GroupDef>>,
}

#[derive(Debug, Clone)]
enum LetDef {
    Text(String),
    FirstPresent(FirstPresent),
    Pred(Pred),
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct FirstPresent {
    list: Vec<String>,
    default: String,
}

#[derive(Debug, Clone)]
enum MetaDef {
    Static(Value),
    GpuSelector(Vec<String>),
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum Computed {
    GpuSelector,
}

/// A predicate over the metric catalog.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
enum Pred {
    Present(String),
    HasCounter(String),
    #[serde(deserialize_with = "object")]
    HasLabel(HasLabel),
    #[serde(deserialize_with = "object")]
    DistinctGt(DistinctGt),
    Any(Vec<Pred>),
    All(Vec<Pred>),
    Not(Box<Pred>),
    Var(String),
    Param(String),
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct HasLabel {
    metric: String,
    label: String,
    #[serde(default)]
    value: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct DistinctGt {
    metrics: Vec<String>,
    by: Vec<String>,
    n: usize,
    #[serde(default)]
    each: bool,
    #[serde(default)]
    int: Vec<String>,
}

/// An element of a list, or a node that expands into elements.
#[derive(Debug, Clone)]
enum Item<T> {
    When(WhenNode<T>),
    Repeat(RepeatNode<T>),
    Leaf(T),
}

#[derive(Debug, Clone)]
struct WhenNode<T> {
    when: Pred,
    then: Vec<Item<T>>,
    otherwise: Vec<Item<T>>,
}

#[derive(Debug, Clone)]
struct RepeatNode<T> {
    repeat: Repeat,
    body: Vec<Item<T>>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Repeat {
    var: String,
    #[serde(rename = "in")]
    over: RepeatOver,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
enum RepeatOver {
    Values(Vec<RepeatValue>),
    #[serde(deserialize_with = "object")]
    LabelValues(LabelValues),
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct LabelValues {
    metric: String,
    label: String,
    #[serde(default)]
    sort: LabelSort,
}

#[derive(Debug, Clone)]
enum RepeatValue {
    Text(String),
    Fields(BTreeMap<String, String>),
}

#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
enum LabelSort {
    #[default]
    Str,
    Int,
}

#[derive(Debug, Clone)]
struct GroupDef {
    name: String,
    id: String,
    metadata: BTreeMap<String, Value>,
    subgroups: Vec<Item<SubGroupDef>>,
}

#[derive(Debug, Clone)]
struct SubGroupDef {
    name: Option<String>,
    description: Option<String>,
    plots: Vec<Item<PlotItem>>,
}

#[derive(Debug, Clone)]
enum PlotItem {
    Split(Box<SplitDef>),
    RateMean(RateMeanDef),
    Plot(Box<PlotDef>),
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct SplitDef {
    when: Pred,
    #[serde(deserialize_with = "two_plots")]
    plots: [PlotDef; 2],
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RateMeanDef {
    title: String,
    id: String,
    selector: String,
    #[serde(default)]
    rate: Option<String>,
    mean_unit: MeanUnit,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum MeanUnit {
    Time,
    Bytes,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct PlotDef {
    kind: PlotKind,
    title: String,
    id: String,
    #[serde(default)]
    unit: Option<Unit>,
    #[serde(default)]
    subtype: Option<Subtype>,
    query: String,
    #[serde(default)]
    width: Option<Width>,
    #[serde(default)]
    percentage_range: bool,
    #[serde(default)]
    range: Option<[f64; 2]>,
    #[serde(default)]
    unit_system: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    percentiles: Option<Vec<f64>>,
    #[serde(default)]
    axis_label: Option<String>,
    #[serde(default)]
    row_label: Option<String>,
    #[serde(default)]
    style: Option<String>,
    #[serde(default)]
    log_scale: Option<bool>,
    #[serde(default)]
    query_total: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum PlotKind {
    Gauge,
    Counter,
    Histogram,
    Latency,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Subtype {
    Percentiles,
    Buckets,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Width {
    Full,
}

// Reading the document.
//
// The JSON text is parsed once, by `UniqueKeys`, into a `Value` that has
// no repeated key, no array or object over `MAX_ENTRIES` entries and no
// string over `MAX_STRING_BYTES`. The nodes that hold other nodes (a
// section, group, subgroup, `when`, `repeat`, plot item, let and
// metadata entry) are then read from that `Value` by `FromJson`, which
// moves each part out of its parent; so parsing is linear in the
// document. A serde impl that read a `Value` and then deserialized from
// it would copy each subtree once per level above it.
//
// Nodes are told apart by the key that names them (`when`, `repeat`,
// `split`, ...), and every field not consumed is an error. The leaves (a
// plot, predicate, repeat head, split, `rate_mean`, `first_present`) are
// read through their serde impls with `deny_unknown_fields`, from an
// owned `Value`, which moves rather than copies. `leaf`, `object` and
// `two_plots` refuse anything but a JSON object for each struct: serde's
// derived struct impls also accept a JSON array of the fields in order,
// which would let `["gauge", "T", "id", ...]` be a plot.

fn kind_of(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

/// `text` for an error message: Debug-quoted, cut after 200 characters.
struct Ex<'a>(&'a str);

fn ex(text: &str) -> Ex<'_> {
    Ex(text)
}

impl std::fmt::Debug for Ex<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        const KEEP: usize = 200;
        match self.0.char_indices().nth(KEEP) {
            None => write!(f, "{:?}", self.0),
            Some((cut, _)) => write!(f, "{:?}... ({} bytes)", &self.0[..cut], self.0.len()),
        }
    }
}

fn read<T: DeserializeOwned, E: serde::de::Error>(value: Value) -> Result<T, E> {
    if !value.is_object() {
        return Err(E::custom(format!(
            "expected an object, found {}",
            kind_of(&value)
        )));
    }
    T::deserialize(value).map_err(E::custom)
}

/// `deserialize_with` for a struct-typed field: [`read`].
fn object<'de, D: Deserializer<'de>, T: DeserializeOwned>(d: D) -> Result<T, D::Error> {
    read(Value::deserialize(d)?)
}

/// A split's two plots, each an object.
fn two_plots<'de, D: Deserializer<'de>>(d: D) -> Result<[PlotDef; 2], D::Error> {
    let values = Vec::<Value>::deserialize(d)?;
    let [a, b]: [Value; 2] = values.try_into().map_err(|v: Vec<Value>| {
        D::Error::custom(format!("a split has 2 plots, not {}", v.len()))
    })?;
    Ok([read(a)?, read(b)?])
}

/// A JSON value within the parse-time caps, whose objects have no
/// repeated key. `serde_json::Value` keeps the last of a repeated key and
/// drops the others silently.
struct UniqueKeys(Value);

fn check_string<E: serde::de::Error>(s: &str) -> Result<(), E> {
    if s.len() > MAX_STRING_BYTES {
        return Err(E::custom(format!(
            "a string of {} bytes: at most {MAX_STRING_BYTES}",
            s.len()
        )));
    }
    if let Some(c) = s.chars().find(|c| is_c0(*c)) {
        return Err(E::custom(format!(
            "{:?} holds the control character U+{:04X}",
            ex(s),
            u32::from(c)
        )));
    }
    Ok(())
}

/// A C0 control character, which JSON escapes as six bytes (`\u0001`).
fn is_c0(c: char) -> bool {
    u32::from(c) < 0x20
}

/// How many C0 control characters `text` holds.
fn c0_count(text: &str) -> usize {
    text.chars().filter(|c| is_c0(*c)).count()
}

impl<'de> Deserialize<'de> for UniqueKeys {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct Visit;
        impl<'de> serde::de::Visitor<'de> for Visit {
            type Value = UniqueKeys;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a JSON value")
            }
            fn visit_bool<E>(self, v: bool) -> Result<UniqueKeys, E> {
                Ok(UniqueKeys(Value::Bool(v)))
            }
            fn visit_i64<E>(self, v: i64) -> Result<UniqueKeys, E> {
                Ok(UniqueKeys(Value::from(v)))
            }
            fn visit_u64<E>(self, v: u64) -> Result<UniqueKeys, E> {
                Ok(UniqueKeys(Value::from(v)))
            }
            fn visit_f64<E>(self, v: f64) -> Result<UniqueKeys, E> {
                Ok(UniqueKeys(Value::from(v)))
            }
            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<UniqueKeys, E> {
                check_string(v)?;
                Ok(UniqueKeys(Value::String(v.to_string())))
            }
            fn visit_string<E: serde::de::Error>(self, v: String) -> Result<UniqueKeys, E> {
                check_string(&v)?;
                Ok(UniqueKeys(Value::String(v)))
            }
            fn visit_unit<E>(self) -> Result<UniqueKeys, E> {
                Ok(UniqueKeys(Value::Null))
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> Result<UniqueKeys, A::Error> {
                let mut out = Vec::new();
                while let Some(UniqueKeys(v)) = seq.next_element()? {
                    if out.len() == MAX_ENTRIES {
                        return Err(A::Error::custom(format!(
                            "an array of more than {MAX_ENTRIES} entries"
                        )));
                    }
                    out.push(v);
                }
                Ok(UniqueKeys(Value::Array(out)))
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> Result<UniqueKeys, A::Error> {
                let mut out = serde_json::Map::new();
                while let Some(key) = map.next_key::<String>()? {
                    check_string(&key)?;
                    let UniqueKeys(value) = map.next_value()?;
                    if out.contains_key(&key) {
                        return Err(A::Error::custom(format!("key {:?} is repeated", ex(&key))));
                    }
                    if out.len() == MAX_ENTRIES {
                        return Err(A::Error::custom(format!(
                            "an object of more than {MAX_ENTRIES} entries"
                        )));
                    }
                    out.insert(key, value);
                }
                Ok(UniqueKeys(Value::Object(out)))
            }
        }
        d.deserialize_any(Visit)
    }
}

/// Why a part of the document could not be read: the message, and where.
struct JsonErr {
    /// Innermost first: field names and list indices.
    path: Vec<Seg>,
    msg: String,
}

enum Seg {
    Key(String),
    Index(usize),
}

impl JsonErr {
    fn new(msg: impl Into<String>) -> Self {
        Self {
            path: Vec::new(),
            msg: msg.into(),
        }
    }

    fn at(mut self, seg: Seg) -> Self {
        self.path.push(seg);
        self
    }
}

impl std::fmt::Display for JsonErr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut path = String::new();
        for seg in self.path.iter().rev() {
            match seg {
                Seg::Key(k) => {
                    if !path.is_empty() {
                        path.push('.');
                    }
                    path.push_str(k);
                }
                Seg::Index(i) => path.push_str(&format!("[{i}]")),
            }
        }
        if path.is_empty() {
            f.write_str(&self.msg)
        } else {
            write!(f, "{path}: {}", self.msg)
        }
    }
}

type Json<T> = Result<T, JsonErr>;

/// Read from an owned JSON value, moving its parts.
trait FromJson: Sized {
    fn from_json(value: Value) -> Json<Self>;
}

/// A value read through its serde impl: an enumerated value or a number.
fn serde_value<T: DeserializeOwned>(value: Value) -> Json<T> {
    T::deserialize(value).map_err(|e| JsonErr::new(e.to_string()))
}

/// A leaf struct read through its serde impl, refusing a non-object.
fn leaf<T: DeserializeOwned>(value: Value) -> Json<T> {
    read::<T, serde_json::Error>(value).map_err(|e| JsonErr::new(e.to_string()))
}

macro_rules! leaf_from_json {
    ($($t:ty),*) => {
        $(impl FromJson for $t {
            fn from_json(value: Value) -> Json<Self> {
                leaf(value)
            }
        })*
    };
}

leaf_from_json!(Pred, Repeat, FirstPresent, SplitDef, RateMeanDef, PlotDef);

impl FromJson for Value {
    fn from_json(value: Value) -> Json<Self> {
        Ok(value)
    }
}

impl FromJson for String {
    fn from_json(value: Value) -> Json<Self> {
        match value {
            Value::String(s) => Ok(s),
            other => Err(JsonErr::new(format!(
                "expected a string, found {}",
                kind_of(&other)
            ))),
        }
    }
}

impl<T: FromJson> FromJson for Vec<T> {
    fn from_json(value: Value) -> Json<Self> {
        let Value::Array(items) = value else {
            return Err(JsonErr::new(format!(
                "expected an array, found {}",
                kind_of(&value)
            )));
        };
        items
            .into_iter()
            .enumerate()
            .map(|(i, v)| T::from_json(v).map_err(|e| e.at(Seg::Index(i))))
            .collect()
    }
}

impl<T: FromJson> FromJson for BTreeMap<String, T> {
    fn from_json(value: Value) -> Json<Self> {
        let Value::Object(map) = value else {
            return Err(JsonErr::new(format!(
                "expected an object, found {}",
                kind_of(&value)
            )));
        };
        map.into_iter()
            .map(|(k, v)| match T::from_json(v) {
                Ok(t) => Ok((k, t)),
                Err(e) => Err(e.at(Seg::Key(format!("{:?}", ex(&k))))),
            })
            .collect()
    }
}

/// An object being read: each field is taken out of it, and what is left
/// at the end is an unknown field.
struct Obj(serde_json::Map<String, Value>);

impl Obj {
    fn new(value: Value) -> Json<Self> {
        match value {
            Value::Object(map) => Ok(Self(map)),
            other => Err(JsonErr::new(format!(
                "expected an object, found {}",
                kind_of(&other)
            ))),
        }
    }

    fn has(&self, key: &str) -> bool {
        self.0.contains_key(key)
    }

    fn take<T>(&mut self, key: &str, read: impl FnOnce(Value) -> Json<T>) -> Json<Option<T>> {
        match self.0.remove(key) {
            None | Some(Value::Null) => Ok(None),
            Some(v) => read(v)
                .map(Some)
                .map_err(|e| e.at(Seg::Key(key.to_string()))),
        }
    }

    fn opt<T: FromJson>(&mut self, key: &str) -> Json<Option<T>> {
        self.take(key, T::from_json)
    }

    fn req<T: FromJson>(&mut self, key: &str) -> Json<T> {
        self.opt(key)?
            .ok_or_else(|| JsonErr::new(format!("missing field `{key}`")))
    }

    fn or_default<T: FromJson + Default>(&mut self, key: &str) -> Json<T> {
        Ok(self.opt(key)?.unwrap_or_default())
    }

    /// Refuse a field not taken; `fields` are the ones this object takes.
    fn done(self, fields: &[&str]) -> Json<()> {
        match self.0.keys().next() {
            None => Ok(()),
            Some(k) => Err(JsonErr::new(format!(
                "unknown field {:?}, expected one of `{}`",
                ex(k),
                fields.join("`, `")
            ))),
        }
    }
}

impl FromJson for Document {
    fn from_json(value: Value) -> Json<Self> {
        let mut o = Obj::new(value)?;
        // `parse` has read and checked `schema`.
        o.0.remove("schema");
        let doc = Self {
            revision: o.opt("revision")?,
            params: o.or_default("params")?,
            lets: o.or_default("let")?,
            sections: o.req("sections")?,
        };
        o.done(&["schema", "revision", "params", "let", "sections"])?;
        Ok(doc)
    }
}

impl FromJson for SectionDef {
    fn from_json(value: Value) -> Json<Self> {
        let mut o = Obj::new(value)?;
        if o.has("builtin") {
            let Builtin::Query = o
                .take("builtin", serde_value::<Builtin>)?
                .ok_or_else(|| JsonErr::new("`builtin` is null"))?;
            o.done(&["builtin"])?;
            return Ok(Self::Builtin);
        }
        let body = SectionBody {
            name: o.req("name")?,
            route: o.req("route")?,
            lets: o.or_default("let")?,
            metadata: o.or_default("metadata")?,
            groups: o.or_default("groups")?,
        };
        o.done(&["name", "route", "let", "metadata", "groups"])?;
        Ok(Self::Section(Box::new(body)))
    }
}

impl FromJson for LetDef {
    fn from_json(value: Value) -> Json<Self> {
        if let Value::String(text) = value {
            return Ok(Self::Text(text));
        }
        let mut o = Obj::new(value)?;
        let def = if o.has("first_present") {
            let fp = o.req("first_present")?;
            o.done(&["first_present"])?;
            Self::FirstPresent(fp)
        } else if o.has("pred") {
            let pred = o.req("pred")?;
            o.done(&["pred"])?;
            Self::Pred(pred)
        } else {
            return Err(JsonErr::new(
                "a let is a string, {\"first_present\": ..} or {\"pred\": ..}",
            ));
        };
        Ok(def)
    }
}

impl FromJson for MetaDef {
    fn from_json(value: Value) -> Json<Self> {
        let mut o = Obj::new(value).map_err(|_| {
            JsonErr::new("section metadata is {\"value\": ..} or {\"computed\": ..}")
        })?;
        if o.has("computed") {
            let Computed::GpuSelector = o
                .take("computed", serde_value::<Computed>)?
                .ok_or_else(|| JsonErr::new("`computed` is null"))?;
            let probe = o.req("probe")?;
            o.done(&["computed", "probe"])?;
            Ok(Self::GpuSelector(probe))
        } else if o.has("value") {
            let value = o.0.remove("value").unwrap_or(Value::Null);
            o.done(&["value"])?;
            Ok(Self::Static(value))
        } else {
            Err(JsonErr::new(
                "section metadata is {\"value\": ..} or {\"computed\": ..}",
            ))
        }
    }
}

impl<T: FromJson> FromJson for Item<T> {
    fn from_json(value: Value) -> Json<Self> {
        let mut o = Obj::new(value)?;
        if o.has("when") {
            let node = WhenNode {
                when: o.req("when")?,
                then: o.req("then")?,
                otherwise: o.or_default("else")?,
            };
            o.done(&["when", "then", "else"])?;
            Ok(Self::When(node))
        } else if o.has("repeat") {
            let node = RepeatNode {
                repeat: o.req("repeat")?,
                body: o.req("body")?,
            };
            o.done(&["repeat", "body"])?;
            Ok(Self::Repeat(node))
        } else {
            Ok(Self::Leaf(T::from_json(Value::Object(o.0))?))
        }
    }
}

impl FromJson for GroupDef {
    fn from_json(value: Value) -> Json<Self> {
        let mut o = Obj::new(value)?;
        let group = Self {
            name: o.req("name")?,
            id: o.req("id")?,
            metadata: o.or_default("metadata")?,
            subgroups: o.or_default("subgroups")?,
        };
        o.done(&["name", "id", "metadata", "subgroups"])?;
        Ok(group)
    }
}

impl FromJson for SubGroupDef {
    fn from_json(value: Value) -> Json<Self> {
        let mut o = Obj::new(value)?;
        let sub = Self {
            name: o.opt("name")?,
            description: o.opt("description")?,
            plots: o.or_default("plots")?,
        };
        o.done(&["name", "description", "plots"])?;
        Ok(sub)
    }
}

impl FromJson for PlotItem {
    fn from_json(value: Value) -> Json<Self> {
        let mut o = Obj::new(value)?;
        if o.has("split") {
            let split = o.req("split")?;
            o.done(&["split"])?;
            Ok(Self::Split(Box::new(split)))
        } else if o.has("rate_mean") {
            let rm = o.req("rate_mean")?;
            o.done(&["rate_mean"])?;
            Ok(Self::RateMean(rm))
        } else {
            Ok(Self::Plot(Box::new(leaf(Value::Object(o.0))?)))
        }
    }
}

impl<'de> Deserialize<'de> for RepeatValue {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        match Value::deserialize(d)? {
            Value::String(text) => Ok(Self::Text(text)),
            Value::Object(map) => {
                let mut fields = BTreeMap::new();
                for (field, value) in map {
                    let Value::String(text) = value else {
                        return Err(D::Error::custom(format!(
                            "repeat value field {:?} is not a string",
                            ex(&field)
                        )));
                    };
                    fields.insert(field, text);
                }
                Ok(Self::Fields(fields))
            }
            other => Err(D::Error::custom(format!(
                "repeat value {} is not a string or an object of strings",
                kind_of(&other)
            ))),
        }
    }
}

/// What kind of problem a [`TemplateError`] reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum TemplateErrorKind {
    /// [`SectionTemplate::parse`] refused the document.
    Invalid,
    /// Expanding a section passed one of the limits in the module docs.
    Limit,
    /// A string being expanded uses a declared parameter the viewer did
    /// not set.
    UnsetParam,
}

/// Why a template was refused, or a section could not be expanded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemplateError {
    kind: TemplateErrorKind,
    message: String,
}

impl TemplateError {
    fn new(kind: TemplateErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    /// What kind of problem this is.
    pub fn kind(&self) -> TemplateErrorKind {
        self.kind
    }

    /// The message, as [`Display`](std::fmt::Display) prints it.
    pub fn message(&self) -> &str {
        &self.message
    }

    /// The same error, with `prefix: ` before its message.
    fn context(self, prefix: impl std::fmt::Display) -> Self {
        Self {
            kind: self.kind,
            message: format!("{prefix}: {}", self.message),
        }
    }
}

impl std::fmt::Display for TemplateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for TemplateError {}

fn err<T>(msg: impl Into<String>) -> Result<T, TemplateError> {
    Err(TemplateError::new(TemplateErrorKind::Invalid, msg))
}

/// A [`TemplateError`] of kind [`TemplateErrorKind::Invalid`].
fn invalid(msg: impl Into<String>) -> TemplateError {
    TemplateError::new(TemplateErrorKind::Invalid, msg)
}

/// How a section is named in an error: its index and route.
fn section_label(index: usize, value: &Value) -> String {
    if let Some(route) = value.get("route").and_then(Value::as_str) {
        format!("section {index} ({:?})", ex(route))
    } else if value.get("builtin").is_some() {
        format!("section {index} (builtin)")
    } else {
        format!("section {index}")
    }
}

impl SectionTemplate {
    /// Parse and check a template: its schema, the shape of every node, and
    /// the names, enumerated values and `${..}` syntax in both branches of
    /// every `when` and the body of every `repeat`.
    pub fn parse(json: &str) -> Result<Self, TemplateError> {
        let UniqueKeys(value) = serde_json::from_str(json)
            .map_err(|e| invalid(format!("not a section template: {e}")))?;
        if !value.is_object() {
            return err(format!(
                "not a section template: expected an object, found {}",
                kind_of(&value)
            ));
        }
        // `schema` alone first, so a newer document is refused as newer
        // rather than for a field this expander does not know.
        let schema = match value.get("schema") {
            None => return err("not a section template: no `schema`"),
            Some(schema) => schema.as_u64().ok_or_else(|| {
                invalid(format!(
                    "section template schema {schema} is not an integer"
                ))
            })?,
        };
        if schema == 0 || schema > u64::from(MAX_SCHEMA) {
            return err(format!(
                "section template schema {schema} is not one this viewer expands (1 to {MAX_SCHEMA})"
            ));
        }
        let doc = Document::from_json(value)
            .map_err(|e| invalid(format!("not a section template: {e}")))?;
        if doc.sections.len() > MAX_SECTIONS {
            return err(format!(
                "{} sections: at most {MAX_SECTIONS}",
                doc.sections.len()
            ));
        }
        let mut sections = Vec::with_capacity(doc.sections.len());
        for (index, value) in doc.sections.into_iter().enumerate() {
            let label = section_label(index, &value);
            let section =
                SectionDef::from_json(value).map_err(|e| invalid(format!("{label}: {e}")))?;
            sections.push(section);
        }
        let mut params = BTreeSet::new();
        for param in doc.params {
            if !is_name(&param) {
                return err(format!("parameter name {:?} is not a name", ex(&param)));
            }
            if !params.insert(param.clone()) {
                return err(format!("parameter {:?} is declared twice", ex(&param)));
            }
        }
        let template = Self {
            schema: schema as u32,
            revision: doc.revision,
            params,
            lets: doc.lets,
            sections,
        };
        template.check()?;
        Ok(template)
    }

    /// The document's schema.
    pub fn schema(&self) -> u32 {
        self.schema
    }

    /// The parameters the document declares, in name order.
    pub fn params(&self) -> impl Iterator<Item = &str> {
        self.params.iter().map(String::as_str)
    }

    /// The document's `revision`, if it gave one.
    pub fn revision(&self) -> Option<&str> {
        self.revision.as_deref()
    }

    /// The sections, in order, as the navigation lists them. The query
    /// explorer is `("Query Explorer", "/query")`.
    pub fn sections(&self) -> Vec<Section> {
        self.sections
            .iter()
            .map(|s| match s {
                SectionDef::Builtin => Section {
                    name: "Query Explorer".to_string(),
                    route: "/query".to_string(),
                },
                SectionDef::Section(body) => Section {
                    name: body.name.clone(),
                    route: body.route.clone(),
                },
            })
            .collect()
    }

    /// Render the section at `route` against `data`, with `sections` as the
    /// view's navigation and `params` the viewer parameters. `Ok(None)` for
    /// a route the template does not have.
    pub fn expand(
        &self,
        data: &dyn MetricsSource,
        route: &str,
        sections: Vec<Section>,
        params: &BTreeMap<String, String>,
    ) -> Result<Option<View>, TemplateError> {
        for section in &self.sections {
            match section {
                SectionDef::Builtin if route == "/query" => {
                    return Ok(Some(View::new(data, sections)));
                }
                SectionDef::Section(body) if body.route == route => {
                    // An undeclared parameter in `params` is never read:
                    // `check` refused every reference to one.
                    return self
                        .expand_body(data, body, sections, params, &Budget::default())
                        .map(Some)
                        .map_err(|e| e.context(route));
                }
                _ => {}
            }
        }
        Ok(None)
    }

    fn expand_body(
        &self,
        data: &dyn MetricsSource,
        body: &SectionBody,
        sections: Vec<Section>,
        params: &BTreeMap<String, String>,
        budget: &Budget,
    ) -> Result<View, TemplateError> {
        let mut scope = Scope::new(data, params, budget);
        scope.bind_lets(&self.lets)?;
        scope.bind_lets(&body.lets)?;
        let mut view = View::new(data, sections);
        for (key, meta) in &body.metadata {
            if let Some(value) = scope.meta(meta)? {
                scope.spend_hash_lookup(key.len())?;
                view.metadata.insert(key.clone(), value);
            }
        }
        walk(&body.groups, &mut scope, &mut |def, scope| {
            if let Some(group) = scope.group(def)? {
                view.group(group);
            }
            Ok(())
        })?;
        Ok(view)
    }

    /// Everything [`parse`](Self::parse) checks past the shape of the JSON.
    fn check(&self) -> Result<(), TemplateError> {
        let mut routes = BTreeSet::new();
        for (index, section) in self.sections.iter().enumerate() {
            let route = match section {
                SectionDef::Builtin => "/query",
                SectionDef::Section(body) => {
                    check_section_name(&body.name, &body.route)
                        .map_err(|e| e.context(format!("section {index}")))?;
                    &body.route
                }
            };
            if !routes.insert(route) {
                return err(format!("two sections have route {:?}", ex(route)));
            }
        }
        let mut names = Names::new(&self.params);
        names
            .lets(&self.lets)
            .map_err(|e| e.context("document let"))?;
        for (index, section) in self.sections.iter().enumerate() {
            let SectionDef::Section(body) = section else {
                continue;
            };
            let mut names = names.clone();
            names
                .lets(&body.lets)
                .and_then(|()| names.section(body))
                .map_err(|e| e.context(format!("section {index} ({:?})", body.route)))?;
        }
        Ok(())
    }
}

/// A section's name is 1 to [`MAX_SECTION_NAME_BYTES`] bytes. Its route is
/// `/` and then 1 or more segments of `a-z`, `0-9`, `_` and `-` separated
/// by single `/`, at most [`MAX_SECTION_NAME_BYTES`] bytes in all.
fn check_section_name(name: &str, route: &str) -> Result<(), TemplateError> {
    if name.is_empty() || name.len() > MAX_SECTION_NAME_BYTES {
        return err(format!(
            "section name {:?} is not 1 to {MAX_SECTION_NAME_BYTES} bytes",
            ex(name)
        ));
    }
    let segments_ok = route.strip_prefix('/').is_some_and(|rest| {
        rest.split('/').all(|seg| {
            !seg.is_empty()
                && seg
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
        })
    });
    if !segments_ok || route.len() > MAX_SECTION_NAME_BYTES {
        return err(format!(
            "route {:?} is not `/` and segments of a-z, 0-9, `_` and `-`, at most {MAX_SECTION_NAME_BYTES} bytes",
            ex(route)
        ));
    }
    Ok(())
}

/// A label name in a predicate or repeat is at most [`MAX_NAME_BYTES`]
/// before substitution: each series lookup compares it against the
/// catalog's label names.
fn check_label_name(label: &str) -> Result<(), TemplateError> {
    if label.len() > MAX_NAME_BYTES {
        return err(format!(
            "label name {:?} is longer than {MAX_NAME_BYTES} bytes",
            ex(label)
        ));
    }
    Ok(())
}

/// `name` is a let, parameter, repeat variable or field name: ASCII
/// letters, digits and `_`, not starting with a digit, at most
/// [`MAX_NAME_BYTES`] bytes.
fn is_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_NAME_BYTES
        && !name.starts_with(|c: char| c.is_ascii_digit())
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// The `${..}` references in `text`, or an error for one that is not
/// closed or not a name.
fn references(text: &str) -> Result<Vec<&str>, TemplateError> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("${") {
        let after = &rest[start + 2..];
        let Some(end) = after.find('}') else {
            return err(format!("unclosed `${{` in {:?}", ex(text)));
        };
        let name = &after[..end];
        let well_formed = match name.split_once('.') {
            Some((var, field)) => is_name(var) && is_name(field),
            None => is_name(name),
        };
        if !well_formed {
            return err(format!(
                "`${{` name {:?} in {:?} is not a name",
                ex(name),
                ex(text)
            ));
        }
        out.push(name);
        rest = &after[end + 1..];
    }
    Ok(out)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NameKind {
    /// A string let, parameter or `values` binding: usable as `${name}`.
    Text,
    /// A `label_values` binding: usable as `${name}`, and in a PromQL
    /// field only inside a double-quoted string.
    Catalog,
    /// A `pred` let: usable as `{"var": name}`.
    Pred,
    /// A repeat over objects: its fields are usable as `${name.field}`.
    Record,
}

/// The names in scope at a point in a template, for [`SectionTemplate::check`].
#[derive(Clone)]
struct Names<'a> {
    params: &'a BTreeSet<String>,
    names: BTreeMap<String, NameKind>,
}

impl<'a> Names<'a> {
    fn new(params: &'a BTreeSet<String>) -> Self {
        Self {
            params,
            names: BTreeMap::new(),
        }
    }

    fn in_scope(&self, name: &str) -> bool {
        self.params.contains(name) || self.names.contains_key(name)
    }

    /// Check each let with only the parameters in scope, then add the lets.
    fn lets(&mut self, lets: &BTreeMap<String, LetDef>) -> Result<(), TemplateError> {
        let params_only = Names::new(self.params);
        for (name, def) in lets {
            if !is_name(name) {
                return err(format!("let name {:?} is not a name", ex(name)));
            }
            if self.in_scope(name) {
                return err(format!("let {:?} reuses a name already in scope", ex(name)));
            }
            let checked = match def {
                LetDef::Text(text) => params_only.text(text),
                LetDef::FirstPresent(fp) => fp
                    .list
                    .iter()
                    .chain([&fp.default])
                    .try_for_each(|m| params_only.text(m)),
                LetDef::Pred(pred) => params_only.pred(pred),
            };
            checked.map_err(|e| e.context(format!("let {:?}", ex(name))))?;
        }
        for (name, def) in lets {
            let kind = match def {
                LetDef::Pred(_) => NameKind::Pred,
                _ => NameKind::Text,
            };
            self.names.insert(name.clone(), kind);
        }
        Ok(())
    }

    fn text(&self, text: &str) -> Result<(), TemplateError> {
        for name in references(text)? {
            match self.names.get(name) {
                Some(NameKind::Text | NameKind::Catalog) => {}
                Some(NameKind::Pred) => {
                    return err(format!(
                        "`${{{name}}}` in {:?} names a predicate; use {{\"var\": {:?}}}",
                        ex(text),
                        ex(name)
                    ));
                }
                Some(NameKind::Record) => {
                    return err(format!(
                        "`${{{name}}}` in {:?} names a repeat over objects; use `${{{name}.field}}`",
                        ex(text)
                    ));
                }
                None if self.params.contains(name) => {}
                None => return err(format!("unknown name `${{{name}}}` in {:?}", ex(text))),
            }
        }
        Ok(())
    }

    fn text_opt(&self, text: &Option<String>) -> Result<(), TemplateError> {
        text.as_deref().map_or(Ok(()), |t| self.text(t))
    }

    /// [`text`](Self::text) for a PromQL field, where a catalog value must
    /// sit inside a double-quoted string: there it is escaped, and anywhere
    /// else (a single-quoted or backtick string, or bare) a label value
    /// could end the string or the matcher it is in.
    fn promql(&self, text: &str) -> Result<(), TemplateError> {
        self.text(text)?;
        #[derive(PartialEq)]
        enum In {
            Bare,
            Double,
            Single,
            Backtick,
            Comment,
        }
        if text.contains("\\${") {
            return err(format!(
                "{:?} has a `${{` after a `\\`, which a PromQL field does not allow",
                ex(text)
            ));
        }
        let mut state = In::Bare;
        let mut i = 0;
        while let Some(c) = text[i..].chars().next() {
            if text[i..].starts_with("${") {
                // `text` has checked that every `${` is closed.
                let end = i + 2 + text[i + 2..].find('}').unwrap_or(0);
                let name = &text[i + 2..end];
                if self.names.get(name) == Some(&NameKind::Catalog) && state != In::Double {
                    return err(format!(
                        "`${{{name}}}` in {:?} is a label value, and goes only inside a double-quoted string there",
                        ex(text)
                    ));
                }
                i = end + 1;
                continue;
            }
            i += c.len_utf8();
            state = match (state, c) {
                (In::Bare, '"') => In::Double,
                (In::Bare, '\'') => In::Single,
                (In::Bare, '`') => In::Backtick,
                (In::Bare, '#') => In::Comment,
                (s @ (In::Double | In::Single), '\\') => {
                    // The escaped character, whatever it is.
                    i += text[i..].chars().next().map_or(0, char::len_utf8);
                    s
                }
                (In::Double, '"') | (In::Single, '\'') | (In::Backtick, '`') => In::Bare,
                (s, _) => s,
            };
        }
        Ok(())
    }

    fn promql_opt(&self, text: &Option<String>) -> Result<(), TemplateError> {
        text.as_deref().map_or(Ok(()), |t| self.promql(t))
    }

    fn pred(&self, pred: &Pred) -> Result<(), TemplateError> {
        match pred {
            Pred::Present(metric) | Pred::HasCounter(metric) => self.text(metric),
            Pred::HasLabel(HasLabel {
                metric,
                label,
                value,
            }) => {
                self.text(metric)?;
                check_label_name(label)?;
                self.text(label)?;
                self.text_opt(value)
            }
            Pred::DistinctGt(DistinctGt {
                metrics,
                by,
                each,
                int,
                ..
            }) => {
                // The length first, so the `int` search below is at most
                // MAX_BY_LABELS steps per entry.
                if by.is_empty() || by.len() > MAX_BY_LABELS {
                    return err(format!(
                        "distinct_gt names 1 to {MAX_BY_LABELS} `by` labels, not {}",
                        by.len()
                    ));
                }
                if let Some(stray) = int.iter().find(|i| !by.contains(i)) {
                    return err(format!(
                        "distinct_gt `int` label {:?} is not one of its `by` labels",
                        ex(stray)
                    ));
                }
                if *each && (by.len() != 1 || !int.is_empty()) {
                    return err("distinct_gt with `each` takes one `by` label and no `int`");
                }
                // `int` labels are `by` labels, checked above.
                by.iter().try_for_each(|l| check_label_name(l))?;
                metrics
                    .iter()
                    .chain(by)
                    .chain(int)
                    .try_for_each(|s| self.text(s))
            }
            Pred::Any(preds) | Pred::All(preds) => preds.iter().try_for_each(|p| self.pred(p)),
            Pred::Not(p) => self.pred(p),
            Pred::Var(name) => match self.names.get(name) {
                Some(NameKind::Pred) => Ok(()),
                Some(NameKind::Text | NameKind::Catalog | NameKind::Record) => {
                    err(format!("{{\"var\": {:?}}} names a value", ex(name)))
                }
                None => err(format!("unknown predicate {:?}", ex(name))),
            },
            Pred::Param(name) if self.params.contains(name) => Ok(()),
            Pred::Param(name) => err(format!(
                "parameter {:?} is not declared in `params`",
                ex(name)
            )),
        }
    }

    fn section(&mut self, body: &SectionBody) -> Result<(), TemplateError> {
        for meta in body.metadata.values() {
            if let MetaDef::GpuSelector(probe) = meta {
                probe.iter().try_for_each(|m| self.text(m))?;
            }
        }
        self.items(&body.groups, &mut |names, group| names.group(group))
    }

    fn items<T>(
        &mut self,
        items: &[Item<T>],
        leaf: &mut dyn FnMut(&mut Self, &T) -> Result<(), TemplateError>,
    ) -> Result<(), TemplateError> {
        for item in items {
            match item {
                Item::Leaf(t) => leaf(self, t)?,
                Item::When(node) => {
                    self.pred(&node.when)?;
                    self.items(&node.then, leaf)?;
                    self.items(&node.otherwise, leaf)?;
                }
                Item::Repeat(node) => {
                    let bound = self.repeat(&node.repeat)?;
                    let result = self.items(&node.body, leaf);
                    for name in bound {
                        self.names.remove(&name);
                    }
                    result?;
                }
            }
        }
        Ok(())
    }

    /// Check a repeat's head and bind what it binds; returns the names bound.
    fn repeat(&mut self, repeat: &Repeat) -> Result<Vec<String>, TemplateError> {
        let var = &repeat.var;
        if !is_name(var) {
            return err(format!("repeat variable {:?} is not a name", ex(var)));
        }
        if self.in_scope(var) {
            return err(format!(
                "repeat variable {:?} reuses a name already in scope",
                ex(var)
            ));
        }
        let bound = match &repeat.over {
            RepeatOver::LabelValues(lv) => {
                self.text(&lv.metric)?;
                check_label_name(&lv.label)?;
                self.text(&lv.label)?;
                self.names.insert(var.clone(), NameKind::Catalog);
                return Ok(vec![var.clone()]);
            }
            RepeatOver::Values(values) => match values.first() {
                None => Vec::new(),
                Some(RepeatValue::Text(_)) => {
                    if !values.iter().all(|v| matches!(v, RepeatValue::Text(_))) {
                        return err(format!("repeat {:?} mixes strings and objects", ex(var)));
                    }
                    vec![var.clone()]
                }
                Some(RepeatValue::Fields(first)) => {
                    for value in values {
                        let RepeatValue::Fields(fields) = value else {
                            return err(format!("repeat {:?} mixes strings and objects", ex(var)));
                        };
                        if !fields.keys().eq(first.keys()) {
                            return err(format!(
                                "repeat {:?}: every value needs the same fields",
                                ex(var)
                            ));
                        }
                    }
                    let mut bound = Vec::new();
                    for field in first.keys() {
                        if !is_name(field) {
                            return err(format!(
                                "repeat {:?}: field {:?} is not a name",
                                ex(var),
                                ex(field)
                            ));
                        }
                        bound.push(format!("{var}.{field}"));
                    }
                    self.names.insert(var.clone(), NameKind::Record);
                    bound.push(var.clone());
                    return self.bind_text(bound);
                }
            },
        };
        self.bind_text(bound)
    }

    /// Bind each of `names` as text; returns them.
    fn bind_text(&mut self, names: Vec<String>) -> Result<Vec<String>, TemplateError> {
        for name in &names {
            self.names.entry(name.clone()).or_insert(NameKind::Text);
        }
        Ok(names)
    }

    fn group(&mut self, group: &GroupDef) -> Result<(), TemplateError> {
        self.text(&group.name)?;
        self.text(&group.id)?;
        self.items(&group.subgroups, &mut |names, sub| {
            names.text_opt(&sub.name)?;
            names.text_opt(&sub.description)?;
            names.items(&sub.plots, &mut |names, item| names.plot_item(item))
        })
    }

    fn plot_item(&mut self, item: &PlotItem) -> Result<(), TemplateError> {
        match item {
            PlotItem::Plot(def) => self.plot(def, false),
            PlotItem::Split(split) => {
                self.pred(&split.when)?;
                self.plot(&split.plots[0], true)?;
                self.plot(&split.plots[1], true)
            }
            PlotItem::RateMean(rm) => {
                self.text(&rm.title)?;
                self.text(&rm.id)?;
                self.promql(&rm.selector)?;
                self.promql_opt(&rm.rate)
            }
        }
    }

    fn plot(&self, def: &PlotDef, in_split: bool) -> Result<(), TemplateError> {
        let fail = |msg: &str| err(format!("plot {:?}: {msg}", ex(&def.id)));
        match (def.kind, def.unit.is_some(), def.subtype.is_some()) {
            (PlotKind::Gauge | PlotKind::Counter, true, false) => {}
            (PlotKind::Gauge | PlotKind::Counter, _, _) => {
                return fail("a gauge or counter takes a `unit` and no `subtype`");
            }
            (PlotKind::Histogram, true, true) => {}
            (PlotKind::Histogram, _, _) => {
                return fail("a histogram takes a `unit` and a `subtype`");
            }
            (PlotKind::Latency, false, false) => {}
            (PlotKind::Latency, _, _) => {
                return fail("a latency plot takes no `unit` or `subtype`");
            }
        }
        if in_split && def.width.is_some() {
            return fail("a split's plots take no `width`");
        }
        if def.percentage_range && def.range.is_some() {
            return fail("`percentage_range` and `range` cannot both be given");
        }
        if let Some([min, max]) = def.range
            && min >= max
        {
            return fail("`range` is [min, max] with min below max");
        }
        self.text(&def.title)?;
        self.text(&def.id)?;
        self.promql(&def.query)?;
        self.promql_opt(&def.query_total)?;
        for text in [
            &def.unit_system,
            &def.description,
            &def.axis_label,
            &def.row_label,
            &def.style,
        ] {
            self.text_opt(text)?;
        }
        Ok(())
    }
}

/// A value a name is bound to while expanding.
#[derive(Clone)]
struct Bound {
    text: String,
    /// From the catalog (`label_values`): escaped where it lands in PromQL.
    from_catalog: bool,
}

/// The limits one section's expansion runs under.
#[derive(Debug, Clone, Copy)]
struct Limits {
    nodes: usize,
    bytes: usize,
    fuel: usize,
}

impl Limits {
    const DEFAULT: Self = Self {
        nodes: MAX_NODES,
        bytes: MAX_TEXT_BYTES,
        fuel: MAX_FUEL,
    };
}

/// What one section's expansion has spent of each limit, and what it has
/// read from the catalog.
struct Budget {
    limits: Limits,
    nodes: Cell<usize>,
    bytes: Cell<usize>,
    fuel: Cell<usize>,
    memo: CatalogMemo,
}

/// The fuel one call into the [`MetricsSource`] costs, beyond what it
/// returns. A source may scan every table it holds to answer for one
/// metric name (about 120 ns for an absent name on a 9.6-hour archive),
/// so a call is not cheap even when it returns nothing.
const SOURCE_CALL_FUEL: usize = 64;

/// The catalog as one section's expansion has read it: each metric's
/// series of each type, and the counter names, read from the source once.
#[derive(Default)]
struct CatalogMemo {
    /// Per metric: its counter, gauge and histogram series.
    series: RefCell<HashMap<String, SeriesByKind>>,
    counter_names: OnceCell<Rc<HashSet<String>>>,
}

/// A metric's series of each [`SeriesKind`], once read.
type SeriesByKind = [Option<Rc<Vec<Labels>>>; 3];

#[derive(Clone, Copy)]
enum SeriesKind {
    Counter = 0,
    Gauge = 1,
    Histogram = 2,
}

impl Default for Budget {
    fn default() -> Self {
        Self::new(Limits::DEFAULT)
    }
}

impl Budget {
    fn new(limits: Limits) -> Self {
        Self {
            limits,
            nodes: Cell::new(0),
            bytes: Cell::new(0),
            fuel: Cell::new(0),
            memo: CatalogMemo::default(),
        }
    }

    fn spend(cell: &Cell<usize>, n: usize, max: usize, what: &str) -> Result<(), TemplateError> {
        let total = cell.get().saturating_add(n);
        cell.set(total);
        if total > max {
            return Err(TemplateError::new(
                TemplateErrorKind::Limit,
                format!("expands to more than {max} {what}"),
            ));
        }
        Ok(())
    }

    /// Whether `n` more bytes of output fit, without spending them.
    fn bytes_fit(&self, n: usize) -> Result<(), TemplateError> {
        let max = self.limits.bytes;
        if self.bytes.get().saturating_add(n) > max {
            return Err(TemplateError::new(
                TemplateErrorKind::Limit,
                format!("expands to more than {max} bytes"),
            ));
        }
        Ok(())
    }
}

type Labels = BTreeMap<String, String>;

/// What strings and predicates are evaluated in: the catalog, the viewer
/// parameters, `let` values and repeat bindings, and the expansion budget.
///
/// Every loop below charges fuel for each iteration, and every copy
/// charges its bytes; the comment at each loop names its charge.
struct Scope<'a> {
    data: &'a dyn MetricsSource,
    params: &'a BTreeMap<String, String>,
    values: BTreeMap<String, Bound>,
    preds: BTreeMap<String, bool>,
    budget: &'a Budget,
}

/// Where a substituted string lands, which decides whether a catalog value
/// is escaped.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Dest {
    Text,
    Promql,
}

impl<'a> Scope<'a> {
    fn new(
        data: &'a dyn MetricsSource,
        params: &'a BTreeMap<String, String>,
        budget: &'a Budget,
    ) -> Self {
        Self {
            data,
            params,
            values: BTreeMap::new(),
            preds: BTreeMap::new(),
            budget,
        }
    }

    /// One node: one unit of fuel too.
    fn spend_node(&self) -> Result<(), TemplateError> {
        self.spend_fuel(1)?;
        Budget::spend(&self.budget.nodes, 1, self.budget.limits.nodes, "nodes")
    }

    /// `len` bytes of output, which are also `len` units of fuel to copy.
    fn spend_bytes(&self, len: usize) -> Result<(), TemplateError> {
        self.spend_fuel(len)?;
        Budget::spend(&self.budget.bytes, len, self.budget.limits.bytes, "bytes")
    }

    /// A lookup or insert of a `key`-byte key in an ordered map or set of
    /// `len` entries: one unit, plus the key's bytes once per level of the
    /// map. A level can compare the key against several keys of its node;
    /// the charge is per level.
    fn spend_lookup(&self, key: usize, len: usize) -> Result<(), TemplateError> {
        self.spend_fuel(1 + key.saturating_mul(levels(len)))
    }

    /// A lookup or insert of a `key`-byte key in a hash map or set: one
    /// unit, plus the key's bytes twice, hashed and compared.
    fn spend_hash_lookup(&self, key: usize) -> Result<(), TemplateError> {
        self.spend_fuel(1 + 2 * key)
    }

    fn spend_fuel(&self, n: usize) -> Result<(), TemplateError> {
        Budget::spend(
            &self.budget.fuel,
            n,
            self.budget.limits.fuel,
            "units of fuel",
        )
    }

    /// `metric`'s series of `kind`, from the memo. The lookup costs a hash
    /// lookup; the first read of a metric and kind costs
    /// [`SOURCE_CALL_FUEL`] plus one unit per series and its label bytes,
    /// which the memo then holds.
    fn series_of(&self, metric: &str, kind: SeriesKind) -> Result<Rc<Vec<Labels>>, TemplateError> {
        self.spend_hash_lookup(metric.len())?;
        let memo = &self.budget.memo;
        if let Some(Some(cached)) = memo
            .series
            .borrow()
            .get(metric)
            .map(|k| k[kind as usize].clone())
        {
            return Ok(cached);
        }
        let read = match kind {
            SeriesKind::Counter => self.data.counter_labels(metric),
            SeriesKind::Gauge => self.data.gauge_labels(metric),
            SeriesKind::Histogram => self.data.histogram_labels(metric),
        };
        // Loop: one unit per series read, plus its key and value bytes.
        let cost: usize = read
            .iter()
            .map(|l| 1 + l.iter().map(|(k, v)| k.len() + v.len()).sum::<usize>())
            .sum();
        self.spend_fuel(SOURCE_CALL_FUEL + metric.len() + cost)?;
        let read = Rc::new(read);
        memo.series
            .borrow_mut()
            .entry(metric.to_string())
            .or_default()[kind as usize] = Some(Rc::clone(&read));
        Ok(read)
    }

    /// `metric`'s counter and gauge series, and its histogram series when
    /// `histograms`, from the memo. A caller that walks them charges one
    /// unit per series.
    fn series(
        &self,
        metric: &str,
        histograms: bool,
    ) -> Result<Vec<Rc<Vec<Labels>>>, TemplateError> {
        let mut out = vec![
            self.series_of(metric, SeriesKind::Counter)?,
            self.series_of(metric, SeriesKind::Gauge)?,
        ];
        if histograms {
            out.push(self.series_of(metric, SeriesKind::Histogram)?);
        }
        Ok(out)
    }

    /// The values of `label` on `metric`'s series of every type, as
    /// [`MetricsSource::label_values`] gives them.
    fn label_values(&self, metric: &str, label: &str) -> Result<BTreeSet<String>, TemplateError> {
        let mut out = BTreeSet::new();
        // Loop: per series, the lookup of `label` in its labels, and for a
        // value, its insert into `out` and its bytes copied.
        for part in self.series(metric, true)? {
            for labels in part.iter() {
                self.spend_lookup(label.len(), labels.len())?;
                if let Some(v) = labels.get(label) {
                    self.spend_lookup(v.len(), out.len())?;
                    self.spend_fuel(v.len())?;
                    out.insert(v.clone());
                }
            }
        }
        Ok(out)
    }

    /// [`has_metric`], charged.
    fn present(&self, metric: &str) -> Result<bool, TemplateError> {
        Ok(self
            .series(metric, true)?
            .iter()
            .any(|part| !part.is_empty()))
    }

    /// Whether `metric` is a counter name. The names are read once, for
    /// [`SOURCE_CALL_FUEL`] plus one unit per name and its bytes; each
    /// lookup then costs one unit plus the name's bytes.
    fn is_counter(&self, metric: &str) -> Result<bool, TemplateError> {
        let memo = &self.budget.memo;
        let names = match memo.counter_names.get() {
            Some(names) => Rc::clone(names),
            None => {
                let names = self.data.counter_names();
                // Loop: one unit per counter name plus its bytes.
                self.spend_fuel(
                    SOURCE_CALL_FUEL + names.iter().map(|n| 1 + n.len()).sum::<usize>(),
                )?;
                let names: Rc<HashSet<String>> = Rc::new(names.into_iter().collect());
                Rc::clone(memo.counter_names.get_or_init(|| names))
            }
        };
        self.spend_hash_lookup(metric.len())?;
        Ok(names.contains(metric))
    }

    /// Bind each let, evaluated in a scope holding only the parameters and
    /// spending this scope's budget.
    fn bind_lets(&mut self, lets: &BTreeMap<String, LetDef>) -> Result<(), TemplateError> {
        let base = Scope::new(self.data, self.params, self.budget);
        // Loop: one unit per let, plus what `subst`, `present` and `holds`
        // charge.
        for (name, def) in lets {
            self.spend_fuel(1)?;
            match def {
                LetDef::Text(text) => {
                    let text = base.subst(text)?;
                    self.bind(name, text, false)?;
                }
                LetDef::FirstPresent(fp) => {
                    let mut chosen = None;
                    // Loop: what `subst` and `present` charge.
                    for metric in &fp.list {
                        let metric = base.subst(metric)?;
                        if base.present(&metric)? {
                            chosen = Some(metric);
                            break;
                        }
                    }
                    let chosen = match chosen {
                        Some(metric) => metric,
                        None => base.subst(&fp.default)?,
                    };
                    self.bind(name, chosen, false)?;
                }
                LetDef::Pred(pred) => {
                    let holds = base.holds(pred)?;
                    // The insert, and the name copied.
                    self.spend_lookup(name.len(), self.preds.len())?;
                    self.spend_fuel(name.len())?;
                    self.preds.insert(name.clone(), holds);
                }
            }
        }
        Ok(())
    }

    /// Bind `name` to `text`, charging the lookup and, for a new name, the
    /// insert and the name's bytes copied. A name bound before (by an
    /// earlier pass) keeps its key and takes the new value.
    fn bind(&mut self, name: &str, text: String, from_catalog: bool) -> Result<(), TemplateError> {
        self.spend_lookup(name.len(), self.values.len())?;
        match self.values.get_mut(name) {
            Some(bound) => *bound = Bound { text, from_catalog },
            None => {
                self.spend_lookup(name.len(), self.values.len())?;
                self.spend_fuel(name.len())?;
                self.values
                    .insert(name.to_string(), Bound { text, from_catalog });
            }
        }
        Ok(())
    }

    /// Whether `pred` holds: one unit, plus what its strings, lists and
    /// catalog reads charge.
    fn holds(&self, pred: &Pred) -> Result<bool, TemplateError> {
        self.spend_fuel(1)?;
        Ok(match pred {
            Pred::Present(metric) => self.present(&self.subst(metric)?)?,
            Pred::HasCounter(metric) => self.is_counter(&self.subst(metric)?)?,
            Pred::HasLabel(HasLabel {
                metric,
                label,
                value,
            }) => {
                let values = self.label_values(&self.subst(metric)?, &self.subst(label)?)?;
                match value {
                    Some(v) => {
                        let v = self.subst(v)?;
                        self.spend_lookup(v.len(), values.len())?;
                        values.contains(&v)
                    }
                    None => !values.is_empty(),
                }
            }
            Pred::DistinctGt(DistinctGt {
                metrics,
                by,
                n,
                each,
                int,
            }) => {
                // Loops: what `subst` charges per entry.
                let by: Vec<String> = by.iter().map(|b| self.subst(b)).collect::<Result<_, _>>()?;
                let int: BTreeSet<String> = int
                    .iter()
                    .map(|i| self.subst(i))
                    .collect::<Result<_, _>>()?;
                if *each {
                    // `check` refused `each` without exactly one `by` label.
                    let Some(key) = by.first() else {
                        return err("distinct_gt with `each` needs a `by` label");
                    };
                    // Loop: what `subst` and `label_values` charge.
                    for metric in metrics {
                        if self.label_values(&self.subst(metric)?, key)?.len() > *n {
                            return Ok(true);
                        }
                    }
                    false
                } else {
                    // Each tuple is kept as a 128-bit hash, so the set costs
                    // 16 bytes per distinct tuple and no copies of label
                    // values. Two tuples whose hashes collide count once,
                    // which at 128 bits does not happen in practice.
                    let mut seen: HashSet<u128> = HashSet::new();
                    // Which `by` labels are `int` labels, found once rather
                    // than per series. Loop: a lookup in `int` per label.
                    for k in &by {
                        self.spend_lookup(k.len(), int.len())?;
                    }
                    let is_int: Vec<bool> = by.iter().map(|k| int.contains(k)).collect();
                    // Loop: what `subst` and `series` charge, per metric.
                    for metric in metrics {
                        // Loop: one unit per series; each tuple adds, per
                        // `by` label, the lookup in the series' labels and
                        // the bytes of the value hashed.
                        for part in self.series(&self.subst(metric)?, false)? {
                            self.spend_fuel(part.len())?;
                            'series: for labels in part.iter() {
                                let mut low = std::hash::DefaultHasher::new();
                                let mut high = std::hash::DefaultHasher::new();
                                1u8.hash(&mut high);
                                for (key, is_int) in by.iter().zip(&is_int) {
                                    self.spend_lookup(key.len(), labels.len())?;
                                    let value = labels.get(key);
                                    self.spend_fuel(value.map_or(0, String::len))?;
                                    let normal: Option<std::borrow::Cow<str>> = if *is_int {
                                        match value.map(|v| v.parse::<i64>()) {
                                            Some(Ok(i)) => Some(i.to_string().into()),
                                            _ => continue 'series,
                                        }
                                    } else {
                                        value.map(|v| v.as_str().into())
                                    };
                                    normal.hash(&mut low);
                                    normal.hash(&mut high);
                                }
                                seen.insert(
                                    (u128::from(high.finish()) << 64) | u128::from(low.finish()),
                                );
                            }
                        }
                    }
                    seen.len() > *n
                }
            }
            Pred::Any(preds) => {
                // Loop: what `holds` charges.
                for p in preds {
                    if self.holds(p)? {
                        return Ok(true);
                    }
                }
                false
            }
            Pred::All(preds) => {
                // Loop: what `holds` charges.
                for p in preds {
                    if !self.holds(p)? {
                        return Ok(false);
                    }
                }
                true
            }
            Pred::Not(p) => !self.holds(p)?,
            Pred::Var(name) => {
                self.spend_lookup(name.len(), self.preds.len())?;
                match self.preds.get(name) {
                    Some(holds) => *holds,
                    None => return err(format!("unknown predicate {:?}", ex(name))),
                }
            }
            Pred::Param(name) => {
                self.spend_lookup(name.len(), self.params.len())?;
                self.params.contains_key(name)
            }
        })
    }

    fn subst(&self, text: &str) -> Result<String, TemplateError> {
        self.subst_to(text, Dest::Text)
    }

    fn subst_promql(&self, text: &str) -> Result<String, TemplateError> {
        self.subst_to(text, Dest::Promql)
    }

    /// `text` with every `${name}` and `${name.field}` replaced. Charged
    /// fuel for the template text scanned and each reference, and output
    /// bytes for the result, each piece checked against the byte limit
    /// before it is appended so a string over it is refused unbuilt.
    fn subst_to(&self, text: &str, dest: Dest) -> Result<String, TemplateError> {
        self.spend_fuel(text.len().max(1))?;
        let mut out = String::new();
        // The bytes counted for `out`: its length, plus five for each C0
        // control character, which JSON escapes as six bytes. Template
        // text holds none; a catalog value or a parameter can.
        let mut counted = 0usize;
        let mut rest = text;
        let mut push = |out: &mut String, piece: &str, escape: bool| -> Result<(), TemplateError> {
            let cost = piece_cost(piece, escape);
            self.budget.bytes_fit(counted.saturating_add(cost))?;
            if escape {
                push_promql_escaped(out, piece);
            } else {
                out.push_str(piece);
            }
            counted += cost;
            Ok(())
        };
        // Loop: one unit per reference (a reference to an empty value is
        // not free), plus the text scanned, charged above.
        while let Some(start) = rest.find("${") {
            self.spend_fuel(1)?;
            push(&mut out, &rest[..start], false)?;
            let after = &rest[start + 2..];
            let Some(end) = after.find('}') else {
                return err(format!("unclosed `${{` in {:?}", ex(text)));
            };
            let name = &after[..end];
            self.spend_lookup(name.len(), self.values.len())?;
            if let Some(bound) = self.values.get(name) {
                let escape = bound.from_catalog && dest == Dest::Promql;
                push(&mut out, &bound.text, escape)?;
            } else {
                self.spend_lookup(name.len(), self.params.len())?;
                let Some(value) = self.params.get(name) else {
                    // `check` refused every name that is not a binding or
                    // a declared parameter, so this is a parameter left
                    // unset.
                    return Err(TemplateError::new(
                        TemplateErrorKind::UnsetParam,
                        format!("parameter {:?} in {:?} is not set", ex(name), ex(text)),
                    ));
                };
                push(&mut out, value, false)?;
            }
            rest = &after[end + 1..];
        }
        push(&mut out, rest, false)?;
        self.spend_bytes(counted)?;
        Ok(out)
    }

    fn subst_opt(&self, text: &Option<String>) -> Result<Option<String>, TemplateError> {
        text.as_deref().map(|t| self.subst(t)).transpose()
    }

    fn meta(&self, meta: &MetaDef) -> Result<Option<Value>, TemplateError> {
        match meta {
            MetaDef::Static(value) => {
                // The copy: its in-memory bytes.
                self.spend_bytes(json_size(value))?;
                Ok(Some(value.clone()))
            }
            MetaDef::GpuSelector(probe) => {
                // Loop: what `subst` charges.
                let probe: Vec<String> = probe
                    .iter()
                    .map(|m| self.subst(m))
                    .collect::<Result<_, _>>()?;
                let selector = self.gpu_selector(&probe)?;
                if let Some(value) = &selector {
                    self.spend_bytes(json_size(value))?;
                }
                Ok(selector)
            }
        }
    }

    /// The GPU selector's metadata: the distinct `(vendor, id)` pairs among
    /// the probe metrics' counter and gauge series, when there is more than
    /// one.
    fn gpu_selector(&self, probe: &[String]) -> Result<Option<Value>, TemplateError> {
        let mut seen: BTreeSet<(Option<String>, i64)> = BTreeSet::new();
        // An empty probe still costs a unit.
        self.spend_fuel(1)?;
        // Loop: what `series` charges per metric; one unit per series, plus
        // the bytes of each vendor copied.
        for metric in probe {
            for part in self.series(metric, false)? {
                self.spend_fuel(part.len())?;
                for labels in part.iter() {
                    self.spend_lookup("id".len(), labels.len())?;
                    let Some(id) = labels.get("id").and_then(|v| v.parse::<i64>().ok()) else {
                        continue;
                    };
                    self.spend_lookup("vendor".len(), labels.len())?;
                    let vendor = labels.get("vendor").cloned();
                    let vendor_len = vendor.as_ref().map_or(0, String::len);
                    // The vendor copied, and its insert into `seen`.
                    self.spend_fuel(vendor_len)?;
                    self.spend_lookup(vendor_len + 8, seen.len())?;
                    seen.insert((vendor, id));
                }
            }
        }
        if seen.len() <= 1 {
            return Ok(None);
        }
        let mut ids: Vec<i64> = seen.iter().map(|(_, id)| *id).collect();
        // The sort: one unit per id per comparison level.
        self.spend_fuel(ids.len().saturating_mul(levels(ids.len())))?;
        ids.sort_unstable();
        ids.dedup();
        let gpus: Vec<Value> = seen
            .iter()
            .map(|(vendor, id)| match vendor {
                Some(v) => serde_json::json!({ "id": id, "vendor": v }),
                None => serde_json::json!({ "id": id }),
            })
            .collect();
        Ok(Some(
            serde_json::json!({ "enabled": true, "ids": ids, "gpus": gpus }),
        ))
    }

    fn group(&mut self, def: &GroupDef) -> Result<Option<Group>, TemplateError> {
        // The group, its subgroups and plots are output too: each is
        // charged its struct's size, so the byte limit bounds the memory
        // the view takes and not only its strings.
        self.spend_bytes(std::mem::size_of::<Group>())?;
        let mut group = Group::new(self.subst(&def.name)?, self.subst(&def.id)?);
        // Loop: each entry's copy, its key and in-memory bytes.
        for (key, value) in &def.metadata {
            self.spend_bytes(key.len() + json_size(value))?;
            self.spend_hash_lookup(key.len())?;
            group.metadata.insert(key.clone(), value.clone());
        }
        walk(&def.subgroups, self, &mut |sub, scope| {
            scope.spend_bytes(std::mem::size_of::<SubGroup>())?;
            let sg = match &sub.name {
                Some(name) => group.subgroup(scope.subst(name)?),
                None => group.subgroup_unnamed(),
            };
            if let Some(description) = &sub.description {
                sg.describe(scope.subst(description)?);
            }
            walk(&sub.plots, scope, &mut |item, scope| {
                scope.plot_item(sg, item)
            })?;
            group.drop_last_subgroup_if_empty();
            Ok(())
        })?;
        if group.is_empty() {
            return Ok(None);
        }
        Ok(Some(group))
    }

    fn plot_item(&mut self, sg: &mut SubGroup, item: &PlotItem) -> Result<(), TemplateError> {
        match item {
            PlotItem::Plot(def) => self.plot(sg, def, None),
            PlotItem::Split(split) => {
                if self.holds(&split.when)? {
                    self.plot(sg, &split.plots[0], Some(PlotWidth::Half))?;
                    self.plot(sg, &split.plots[1], Some(PlotWidth::Half))
                } else {
                    self.plot(sg, &split.plots[0], Some(PlotWidth::Full))
                }
            }
            PlotItem::RateMean(rm) => {
                let rate = match &rm.rate {
                    Some(q) => RateSource::Counter(self.subst_promql(q)?),
                    None => RateSource::FromHistogram,
                };
                let mean_unit = match rm.mean_unit {
                    MeanUnit::Time => Unit::Time,
                    MeanUnit::Bytes => Unit::Bytes,
                };
                let (title, id, selector) = (
                    self.subst(&rm.title)?,
                    self.subst(&rm.id)?,
                    self.subst_promql(&rm.selector)?,
                );
                // `histogram_rate_mean` makes two plots, copies the selector
                // into three queries and the title and id into two plots
                // each, and adds suffixes and a unit system of its own.
                self.spend_bytes(
                    2 * std::mem::size_of::<Plot>()
                        + 3 * selector.len()
                        + 2 * (title.len() + id.len())
                        + 64,
                )?;
                sg.histogram_rate_mean(&title, &id, &selector, rate, mean_unit);
                Ok(())
            }
        }
    }

    fn plot(
        &mut self,
        sg: &mut SubGroup,
        def: &PlotDef,
        width: Option<PlotWidth>,
    ) -> Result<(), TemplateError> {
        self.spend_bytes(std::mem::size_of::<Plot>())?;
        let title = self.subst(&def.title)?;
        let id = self.subst(&def.id)?;
        // `check` has refused a plot whose kind lacks the unit or subtype
        // it needs; these fall back only so expansion cannot panic.
        let unit = def.unit.unwrap_or(Unit::Count);
        let mut opts = match def.kind {
            PlotKind::Gauge => PlotOpts::gauge(title, id, unit),
            PlotKind::Counter => PlotOpts::counter(title, id, unit),
            PlotKind::Histogram => {
                let subtype = match def.subtype {
                    Some(Subtype::Buckets) => "buckets",
                    Some(Subtype::Percentiles) | None => "percentiles",
                };
                PlotOpts::histogram(title, id, unit, subtype)
            }
            PlotKind::Latency => PlotOpts::histogram_latency(title, id),
        };
        if def.percentage_range {
            opts = opts.percentage_range();
        }
        if let Some([min, max]) = def.range {
            opts = opts.range(min, max);
        }
        if let Some(system) = self.subst_opt(&def.unit_system)? {
            opts = opts.with_unit_system(system);
        }
        if let Some(description) = self.subst_opt(&def.description)? {
            opts = opts.with_description(description);
        }
        if let Some(percentiles) = &def.percentiles {
            // The copy: eight bytes per percentile.
            self.spend_bytes(8 * percentiles.len())?;
            opts = opts.with_percentiles(percentiles.clone());
        }
        if let Some(label) = self.subst_opt(&def.axis_label)? {
            opts = opts.with_axis_label(label);
        }
        if let Some(label) = self.subst_opt(&def.row_label)? {
            opts = opts.with_row_label(label);
        }
        if let Some(style) = self.subst_opt(&def.style)? {
            opts = opts.with_style(style);
        }
        if let Some(log) = def.log_scale {
            opts = opts.with_log_scale(log);
        }
        let query = self.subst_promql(&def.query)?;
        let full = match width {
            Some(w) => w == PlotWidth::Full,
            None => matches!(def.width, Some(Width::Full)),
        };
        if full {
            sg.plot_promql_full(opts, query);
        } else {
            sg.plot_promql(opts, query);
        }
        if let Some(total) = &def.query_total {
            let total = self.subst_promql(total)?;
            if let Some(plot) = sg.plots_mut_last() {
                plot.promql_query_total = Some(total);
            }
        }
        Ok(())
    }
}

/// `text` as it may appear inside a double-quoted PromQL string.
fn push_promql_escaped(out: &mut String, text: &str) {
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c => out.push(c),
        }
    }
}

/// The bytes counted for appending `piece`, escaped for PromQL when
/// `escape`: its length then, plus five for each C0 control character left
/// in it (JSON writes one as six bytes; the PromQL escape turns `\n` and
/// `\r` into two printable bytes each).
fn piece_cost(piece: &str, escape: bool) -> usize {
    if escape {
        let left = piece
            .chars()
            .filter(|c| is_c0(*c) && !matches!(c, '\n' | '\r'))
            .count();
        promql_escaped_len(piece) + 5 * left
    } else {
        piece.len() + 5 * c0_count(piece)
    }
}

/// The length of `text` once [`push_promql_escaped`] has escaped it.
fn promql_escaped_len(text: &str) -> usize {
    text.len()
        + text
            .chars()
            .filter(|c| matches!(c, '\\' | '"' | '\n' | '\r'))
            .count()
}

/// `value`'s size in memory, for the byte budget: each value's own
/// `size_of::<Value>()` plus the bytes of its strings and keys.
fn json_size(value: &Value) -> usize {
    std::mem::size_of::<Value>()
        + match value {
            Value::String(s) => s.len() + 5 * c0_count(s),
            Value::Array(items) => items.iter().map(json_size).sum(),
            // Keys come from the template, which cannot hold a control
            // character, or are fixed (the GPU selector's).
            Value::Object(map) => map.iter().map(|(k, v)| k.len() + json_size(v)).sum(),
            Value::Null | Value::Bool(_) | Value::Number(_) => 0,
        }
}

type Visit<'v, T> = dyn FnMut(&T, &mut Scope<'_>) -> Result<(), TemplateError> + 'v;

/// Visit the elements `items` expands to, in order, each with the scope it
/// was produced in: `when` picks a branch, and `repeat` visits its body once
/// per value with the variable bound for that pass.
fn walk<T>(
    items: &[Item<T>],
    scope: &mut Scope<'_>,
    visit: &mut Visit<'_, T>,
) -> Result<(), TemplateError> {
    // Loop: one node per item.
    for item in items {
        scope.spend_node()?;
        match item {
            Item::Leaf(leaf) => visit(leaf, scope)?,
            Item::When(node) => {
                let branch = if scope.holds(&node.when)? {
                    &node.then
                } else {
                    &node.otherwise
                };
                walk(branch, scope, visit)?;
            }
            // A pass's bindings are not removed after it: `check` refuses a
            // reference to a repeat variable outside its body and a
            // variable that shadows another name, so a stale binding is
            // never read, and the next pass or repeat that binds the name
            // overwrites it.
            Item::Repeat(node) => {
                let var = &node.repeat.var;
                match &node.repeat.over {
                    RepeatOver::Values(values) => {
                        // `check` refused values with differing fields, so
                        // the first value's names serve every pass.
                        let names: Vec<String> = match values.first() {
                            Some(RepeatValue::Fields(fields)) => {
                                // Loop: one unit per field plus the name's
                                // bytes.
                                let mut names = Vec::with_capacity(fields.len());
                                for f in fields.keys() {
                                    scope.spend_fuel(2 + var.len() + f.len())?;
                                    names.push(format!("{var}.{f}"));
                                }
                                names
                            }
                            _ => Vec::new(),
                        };
                        // Loop: one node per pass, plus the bytes of the
                        // value it clones and `bind`'s charge for the name.
                        for value in values {
                            scope.spend_node()?;
                            match value {
                                RepeatValue::Text(text) => {
                                    scope.spend_fuel(text.len())?;
                                    scope.bind(var, text.clone(), false)?;
                                }
                                RepeatValue::Fields(fields) => {
                                    // Loop: one node per field, plus the
                                    // value's bytes and `bind`'s charge.
                                    for (name, text) in names.iter().zip(fields.values()) {
                                        scope.spend_node()?;
                                        scope.spend_fuel(text.len())?;
                                        scope.bind(name, text.clone(), false)?;
                                    }
                                }
                            }
                            walk(&node.body, scope, visit)?;
                        }
                    }
                    RepeatOver::LabelValues(lv) => {
                        // Loop: one node per pass; the values were charged
                        // as `series` read them.
                        for text in label_values_sorted(lv, scope)? {
                            scope.spend_node()?;
                            scope.bind(var, text, true)?;
                            walk(&node.body, scope, visit)?;
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

/// The levels of an ordered map or a sort over `len` entries: the bit
/// length of `len`, at least 1.
fn levels(len: usize) -> usize {
    (usize::BITS - len.leading_zeros()).max(1) as usize
}

/// A `label_values` repeat's values, in its order.
fn label_values_sorted(lv: &LabelValues, scope: &Scope<'_>) -> Result<Vec<String>, TemplateError> {
    let mut values: Vec<String> = scope
        .label_values(&scope.subst(&lv.metric)?, &scope.subst(&lv.label)?)?
        .into_iter()
        .collect();
    // The sort: per comparison level (log n of them), one unit per value
    // plus the bytes the comparisons read; an `int` sort is a second pass
    // that also parses each value per comparison.
    let n = values.len();
    let bytes: usize = values.iter().map(String::len).sum();
    let levels = levels(n);
    let passes = match lv.sort {
        LabelSort::Str => 1,
        LabelSort::Int => 2,
    };
    scope.spend_fuel((n + bytes).saturating_mul(levels).saturating_mul(passes))?;
    sort_label_values(&mut values, lv.sort);
    Ok(values)
}

/// Sort label values as `sort` says. `int` is a stable sort of the
/// string-sorted values, so values that parse to the same integer (`"01"`
/// and `"1"`) and values that do not parse keep string order.
fn sort_label_values(values: &mut [String], sort: LabelSort) {
    values.sort();
    if let LabelSort::Int = sort {
        values.sort_by_key(|v| match v.parse::<i64>() {
            Ok(i) => (0, i),
            Err(_) => (1, 0),
        });
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::section_template_tests::Catalog;

    /// A schema 1 document with `throughput` declared and these sections.
    fn doc(sections: Value) -> String {
        json!({ "schema": 1, "params": ["throughput"], "sections": sections }).to_string()
    }

    /// One section at `/t` with these groups and lets.
    fn section(lets: Value, groups: Value) -> String {
        doc(json!([{ "name": "T", "route": "/t", "let": lets, "groups": groups }]))
    }

    /// One group of one subgroup holding these plot items.
    fn plots(lets: Value, items: Value) -> String {
        section(
            lets,
            json!([{ "name": "G", "id": "g", "subgroups": [{ "name": "S", "plots": items }] }]),
        )
    }

    fn gauge(id: &str) -> Value {
        json!({ "kind": "gauge", "title": id, "id": id, "unit": "count", "query": id })
    }

    /// A plot per `label_values` value `${v}` of `id` on `m`.
    fn per_value() -> Value {
        json!({ "kind": "gauge", "title": "${v}", "id": "${v}", "unit": "count",
                "query": "m{id=\"${v}\"}" })
    }

    fn expand_with(template: &str, data: &Catalog, params: &[(&str, &str)]) -> Value {
        let t = SectionTemplate::parse(template).unwrap_or_else(|e| panic!("{e}"));
        let params = params
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let view = t
            .expand(data, "/t", Vec::new(), &params)
            .unwrap_or_else(|e| panic!("{e}"))
            .unwrap();
        serde_json::to_value(&view).unwrap()
    }

    fn expand(template: &str, data: &Catalog) -> Value {
        expand_with(template, data, &[])
    }

    /// The groups the view holds.
    fn groups(view: &Value) -> Value {
        view["groups"].clone()
    }

    /// Every plot id in the view, in order.
    fn ids(view: &Value) -> Vec<String> {
        let mut out = Vec::new();
        for group in view["groups"].as_array().unwrap() {
            for sub in group["subgroups"].as_array().unwrap() {
                for plot in sub["plots"].as_array().unwrap() {
                    out.push(plot["opts"]["id"].as_str().unwrap().to_string());
                }
            }
        }
        out
    }

    /// The single group `build` fills, as the generators would render it.
    fn group_json(build: impl FnOnce(&mut SubGroup)) -> Value {
        let mut group = Group::new("G", "g");
        build(group.subgroup("S"));
        json!([serde_json::to_value(&group).unwrap()])
    }

    fn parse_err(template: &str) -> String {
        let e = SectionTemplate::parse(template).unwrap_err();
        assert_eq!(e.kind(), TemplateErrorKind::Invalid, "{e}");
        e.message().to_string()
    }

    fn assert_refused(template: &str, needle: &str) {
        let e = parse_err(template);
        assert!(e.contains(needle), "{needle:?} not in {e:?}");
    }

    /// Each predicate arm, as a `when` gating a plot named for the case.
    #[test]
    fn predicates() {
        let data = Catalog::default()
            .counter("c", &[&[("id", "0"), ("op", "read")]])
            .gauge(
                "g",
                &[
                    &[("id", "01"), ("vendor", "a")],
                    &[("id", "1"), ("vendor", "a")],
                ],
            )
            .gauge("v", &[&[("id", "0")], &[("id", "0"), ("vendor", "a")]])
            .gauge("u", &[&[("id", "0")], &[("id", "x")], &[("vendor", "a")]])
            .histogram("h", &[&[("id", "0")]]);
        let cases = [
            ("present-counter", json!({ "present": "c" }), true),
            ("present-gauge", json!({ "present": "g" }), true),
            ("present-histogram", json!({ "present": "h" }), true),
            ("present-missing", json!({ "present": "nope" }), false),
            ("has-counter", json!({ "has_counter": "c" }), true),
            ("has-counter-gauge", json!({ "has_counter": "g" }), false),
            (
                "has-label",
                json!({ "has_label": { "metric": "c", "label": "op" } }),
                true,
            ),
            (
                "has-label-missing",
                json!({ "has_label": { "metric": "c", "label": "nope" } }),
                false,
            ),
            (
                "has-label-value",
                json!({ "has_label": { "metric": "c", "label": "op", "value": "read" } }),
                true,
            ),
            (
                "has-label-other-value",
                json!({ "has_label": { "metric": "c", "label": "op", "value": "write" } }),
                false,
            ),
            // `each` counts strings: "01" and "1" are two values.
            (
                "each",
                json!({ "distinct_gt": { "metrics": ["c", "g"], "by": ["id"], "n": 1, "each": true } }),
                true,
            ),
            (
                "each-one",
                json!({ "distinct_gt": { "metrics": ["c"], "by": ["id"], "n": 1, "each": true } }),
                false,
            ),
            // Across metrics: the same as two strings, one as integers.
            (
                "tuples-str",
                json!({ "distinct_gt": { "metrics": ["g"], "by": ["id"], "n": 1 } }),
                true,
            ),
            (
                "tuples-int",
                json!({ "distinct_gt": { "metrics": ["g"], "by": ["id"], "n": 1, "int": ["id"] } }),
                false,
            ),
            // A missing non-`int` label counts as absent: two tuples.
            (
                "tuples-absent-label",
                json!({ "distinct_gt": { "metrics": ["v"], "by": ["vendor", "id"], "n": 1, "int": ["id"] } }),
                true,
            ),
            // Unparsed or missing `int` labels are skipped: one tuple.
            (
                "tuples-skip-unparsed",
                json!({ "distinct_gt": { "metrics": ["u"], "by": ["id"], "n": 0, "int": ["id"] } }),
                true,
            ),
            (
                "tuples-skip-unparsed-n1",
                json!({ "distinct_gt": { "metrics": ["u"], "by": ["id"], "n": 1, "int": ["id"] } }),
                false,
            ),
            // Histogram series are not counted.
            (
                "tuples-no-histograms",
                json!({ "distinct_gt": { "metrics": ["h"], "by": ["id"], "n": 0 } }),
                false,
            ),
            (
                "any-tf",
                json!({ "any": [{ "present": "c" }, { "present": "x" }] }),
                true,
            ),
            (
                "any-ff",
                json!({ "any": [{ "present": "x" }, { "present": "y" }] }),
                false,
            ),
            ("any-empty", json!({ "any": [] }), false),
            (
                "all-tt",
                json!({ "all": [{ "present": "c" }, { "present": "g" }] }),
                true,
            ),
            (
                "all-tf",
                json!({ "all": [{ "present": "c" }, { "present": "x" }] }),
                false,
            ),
            (
                "all-ft",
                json!({ "all": [{ "present": "x" }, { "present": "c" }] }),
                false,
            ),
            ("all-empty", json!({ "all": [] }), true),
            ("not-t", json!({ "not": { "present": "c" } }), false),
            ("not-f", json!({ "not": { "present": "x" } }), true),
            ("var-t", json!({ "var": "yes" }), true),
            ("var-f", json!({ "var": "no" }), false),
            ("param-unset", json!({ "param": "throughput" }), false),
        ];
        let items: Vec<Value> = cases
            .iter()
            .map(|(id, pred, _)| json!({ "when": pred, "then": [gauge(id)] }))
            .collect();
        let lets = json!({
            "yes": { "pred": { "present": "c" } },
            "no": { "pred": { "present": "x" } },
        });
        let got = ids(&expand(&plots(lets.clone(), json!(items)), &data));
        let want: Vec<String> = cases
            .iter()
            .filter(|(_, _, holds)| *holds)
            .map(|(id, _, _)| id.to_string())
            .collect();
        assert_eq!(got, want);

        let param = plots(
            lets,
            json!([{ "when": { "param": "throughput" }, "then": [gauge("p")] }]),
        );
        let set = expand_with(&param, &data, &[("throughput", "x")]);
        assert_eq!(ids(&set), ["p"]);
    }

    #[test]
    fn when_else() {
        let t = plots(
            json!({}),
            json!([
                { "when": { "present": "c" }, "then": [gauge("then")], "else": [gauge("else")] },
                { "when": { "present": "x" }, "then": [gauge("then2")], "else": [gauge("else2")] },
            ]),
        );
        let data = Catalog::default().counter("c", &[&[]]);
        assert_eq!(ids(&expand(&t, &data)), ["then", "else2"]);
    }

    #[test]
    fn split_is_a_pair_or_one_full_width_plot() {
        let split = |pred: Value| {
            plots(
                json!({}),
                json!([{ "split": { "when": pred, "plots": [gauge("a"), gauge("b")] } }]),
            )
        };
        let data = Catalog::default().counter("c", &[&[]]);
        let pair = expand(&split(json!({ "present": "c" })), &data);
        let want = group_json(|sg| {
            sg.plot_promql(PlotOpts::gauge("a", "a", Unit::Count), "a".into());
            sg.plot_promql(PlotOpts::gauge("b", "b", Unit::Count), "b".into());
        });
        assert_eq!(groups(&pair), want);
        let one = expand(&split(json!({ "present": "x" })), &data);
        let want = group_json(|sg| {
            sg.plot_promql_full(PlotOpts::gauge("a", "a", Unit::Count), "a".into());
        });
        assert_eq!(groups(&one), want);
    }

    #[test]
    fn rate_mean_is_histogram_rate_mean() {
        let t = plots(
            json!({}),
            json!([
                { "rate_mean": { "title": "Read", "id": "read", "selector": "lat{op=\"read\"}",
                                 "rate": "sum(irate(ops[5m]))", "mean_unit": "time" } },
                { "rate_mean": { "title": "Size", "id": "size", "selector": "size",
                                 "mean_unit": "bytes" } },
            ]),
        );
        let want = group_json(|sg| {
            sg.histogram_rate_mean(
                "Read",
                "read",
                "lat{op=\"read\"}",
                RateSource::Counter("sum(irate(ops[5m]))".into()),
                Unit::Time,
            );
            sg.histogram_rate_mean(
                "Size",
                "size",
                "size",
                RateSource::FromHistogram,
                Unit::Bytes,
            );
        });
        let got = groups(&expand(&t, &Catalog::default()));
        assert_eq!(got, want);
        assert_eq!(
            got[0]["subgroups"][0]["plots"][1]["promql_query_total"],
            "histogram_sum(lat{op=\"read\"})"
        );
    }

    /// Every plot option, against the builder calls it names.
    #[test]
    fn plot_options() {
        let t = plots(
            json!({}),
            json!([
                { "kind": "counter", "title": "T", "id": "a", "unit": "bytes", "query": "q",
                  "width": "full", "range": [1.0, 5.0], "unit_system": "bytes",
                  "description": "D", "axis_label": "Y", "row_label": "R", "style": "heatmap",
                  "log_scale": true, "query_total": "qt" },
                { "kind": "histogram", "title": "H", "id": "b", "unit": "time",
                  "subtype": "buckets", "query": "h", "percentiles": [0.5, 0.99] },
                { "kind": "histogram", "title": "P", "id": "p", "unit": "bytes",
                  "subtype": "percentiles", "query": "h" },
                { "kind": "latency", "title": "L", "id": "c", "query": "l",
                  "percentage_range": true },
                { "kind": "gauge", "title": "G", "id": "d", "unit": "power", "query": "g",
                  "log_scale": false },
            ]),
        );
        let want = group_json(|sg| {
            sg.plot_promql_full(
                PlotOpts::counter("T", "a", Unit::Bytes)
                    .range(1.0, 5.0)
                    .with_unit_system("bytes")
                    .with_description("D")
                    .with_axis_label("Y")
                    .with_row_label("R")
                    .with_style("heatmap")
                    .with_log_scale(true),
                "q".into(),
            );
            sg.plots_mut_last().unwrap().promql_query_total = Some("qt".into());
            sg.plot_promql(
                PlotOpts::histogram("H", "b", Unit::Time, "buckets")
                    .with_percentiles(vec![0.5, 0.99]),
                "h".into(),
            );
            sg.plot_promql(
                PlotOpts::histogram("P", "p", Unit::Bytes, "percentiles"),
                "h".into(),
            );
            sg.plot_promql(
                PlotOpts::histogram_latency("L", "c").percentage_range(),
                "l".into(),
            );
            sg.plot_promql(
                PlotOpts::gauge("G", "d", Unit::Power).with_log_scale(false),
                "g".into(),
            );
        });
        assert_eq!(groups(&expand(&t, &Catalog::default())), want);
    }

    #[test]
    fn every_unit_name() {
        let names = [
            ("count", Unit::Count),
            ("rate", Unit::Rate),
            ("time", Unit::Time),
            ("bytes", Unit::Bytes),
            ("datarate", Unit::Datarate),
            ("bitrate", Unit::Bitrate),
            ("percentage", Unit::Percentage),
            ("frequency", Unit::Frequency),
            ("power", Unit::Power),
        ];
        let items: Vec<Value> = names
            .iter()
            .map(|(name, _)| json!({ "kind": "gauge", "title": name, "id": name, "unit": name, "query": "q" }))
            .collect();
        let want = group_json(|sg| {
            for (name, unit) in names {
                sg.plot_promql(PlotOpts::gauge(name, name, unit), "q".into());
            }
        });
        assert_eq!(
            groups(&expand(
                &plots(json!({}), json!(items)),
                &Catalog::default()
            )),
            want
        );
    }

    /// Groups and subgroups left empty are omitted; group metadata is kept.
    #[test]
    fn empty_groups_and_subgroups_are_omitted() {
        let t = section(
            json!({}),
            json!([
                { "name": "Empty", "id": "empty", "subgroups": [
                    { "name": "E", "plots": [{ "when": { "present": "x" }, "then": [gauge("x")] }] },
                ] },
                { "name": "G", "id": "g", "metadata": { "side": "left" }, "subgroups": [
                    { "name": "E", "plots": [] },
                    { "name": "S", "description": "D", "plots": [gauge("a")] },
                    { "plots": [gauge("b")] },
                ] },
            ]),
        );
        let mut group = Group::new("G", "g");
        group.metadata.insert("side".into(), json!("left"));
        let sg = group.subgroup("S");
        sg.describe("D");
        sg.plot_promql(PlotOpts::gauge("a", "a", Unit::Count), "a".into());
        group
            .subgroup_unnamed()
            .plot_promql(PlotOpts::gauge("b", "b", Unit::Count), "b".into());
        let want = json!([serde_json::to_value(&group).unwrap()]);
        assert_eq!(groups(&expand(&t, &Catalog::default())), want);
    }

    #[test]
    fn lets() {
        let t = plots(
            json!({
                "text": "rate(${throughput})",
                "net": { "first_present": { "list": ["a", "b"], "default": "z" } },
                "none": { "first_present": { "list": ["x"], "default": "z" } },
                "has_b": { "pred": { "present": "b" } },
            }),
            json!([
                { "when": { "var": "has_b" }, "then": [
                    { "kind": "gauge", "title": "${net} ${none}", "id": "i", "unit": "count",
                      "query": "${text}" },
                ] },
            ]),
        );
        let data = Catalog::default().counter("b", &[&[]]);
        let view = expand_with(&t, &data, &[("throughput", "q")]);
        let plot = &view["groups"][0]["subgroups"][0]["plots"][0];
        assert_eq!(plot["opts"]["title"], "b z");
        assert_eq!(plot["promql_query"], "rate(q)");
    }

    /// Document lets are visible in every section, beside the section's own.
    #[test]
    fn document_lets() {
        let t = json!({
            "schema": 1,
            "let": { "d": "doc" },
            "sections": [{ "name": "T", "route": "/t", "let": { "s": "sec" }, "groups": [
                { "name": "G", "id": "g", "subgroups": [{ "plots": [
                    { "kind": "gauge", "title": "${d} ${s}", "id": "i", "unit": "count", "query": "q" },
                ] }] },
            ] }],
        })
        .to_string();
        let view = expand(&t, &Catalog::default());
        assert_eq!(
            view["groups"][0]["subgroups"][0]["plots"][0]["opts"]["title"],
            "doc sec"
        );
    }

    /// A parameter the viewer sets but the document does not declare is not
    /// visible to it.
    #[test]
    fn undeclared_params_are_not_visible() {
        let t = json!({
            "schema": 1,
            "sections": [{ "name": "T", "route": "/t", "groups": [
                { "name": "G", "id": "g", "subgroups": [{ "plots": [gauge("a")] }] },
            ] }],
        })
        .to_string();
        assert_refused(
            &t.replace(
                "\"groups\"",
                "\"let\": {\"x\": \"${throughput}\"}, \"groups\"",
            ),
            "unknown name `${throughput}`",
        );
        let declared = plots(
            json!({}),
            json!([{ "kind": "gauge", "title": "${throughput}",
            "id": "a", "unit": "count", "query": "q" }]),
        );
        let t = SectionTemplate::parse(&declared).unwrap();
        let mut params = BTreeMap::new();
        let e = t
            .expand(&Catalog::default(), "/t", Vec::new(), &params)
            .err()
            .unwrap();
        assert!(e.message().contains("parameter \"throughput\""), "{e}");
        assert_eq!(e.kind(), TemplateErrorKind::UnsetParam);
        params.insert("throughput".to_string(), "x".to_string());
        assert!(
            t.expand(&Catalog::default(), "/t", Vec::new(), &params)
                .is_ok()
        );
    }

    #[test]
    fn repeat_over_values() {
        let t = plots(
            json!({}),
            json!([
                { "repeat": { "var": "s", "in": { "values": ["a", "b"] } }, "body": [gauge("s-${s}")] },
                { "repeat": { "var": "o", "in": { "values": [
                    { "id": "x", "q": "1" }, { "id": "y", "q": "2" },
                ] } }, "body": [
                    { "kind": "gauge", "title": "${o.id}", "id": "o-${o.id}", "unit": "count",
                      "query": "${o.q}" },
                ] },
                { "repeat": { "var": "e", "in": { "values": [] } }, "body": [gauge("never")] },
            ]),
        );
        let view = expand(&t, &Catalog::default());
        assert_eq!(ids(&view), ["s-a", "s-b", "o-x", "o-y"]);
        assert_eq!(
            view["groups"][0]["subgroups"][0]["plots"][3]["promql_query"],
            "2"
        );
    }

    /// Nested repeats see the outer binding; a binding ends with its pass.
    #[test]
    fn repeat_scoping() {
        let t = plots(
            json!({}),
            json!([
                { "repeat": { "var": "a", "in": { "values": ["1", "2"] } }, "body": [
                    { "repeat": { "var": "b", "in": { "values": ["x", "y"] } }, "body": [gauge("${a}${b}")] },
                ] },
                { "repeat": { "var": "b", "in": { "values": ["z"] } }, "body": [gauge("${b}")] },
            ]),
        );
        assert_eq!(
            ids(&expand(&t, &Catalog::default())),
            ["1x", "1y", "2x", "2y", "z"]
        );
        assert_refused(
            &plots(
                json!({}),
                json!([
                    { "repeat": { "var": "a", "in": { "values": ["1"] } }, "body": [] },
                    gauge("${a}"),
                ]),
            ),
            "unknown name `${a}`",
        );
    }

    #[test]
    fn repeat_over_label_values() {
        let data = Catalog::default().gauge(
            "m",
            &[
                &[("id", "10")],
                &[("id", "2")],
                &[("id", "1")],
                &[("id", "01")],
                &[("id", "b")],
                &[("id", "a")],
                &[("id", "-1")],
            ],
        );
        let rep = |sort: &str| {
            plots(
                json!({}),
                json!([{ "repeat": { "var": "v", "in": { "label_values":
                    { "metric": "m", "label": "id", "sort": sort } } }, "body": [per_value()] }]),
            )
        };
        assert_eq!(
            ids(&expand(&rep("str"), &data)),
            ["-1", "01", "1", "10", "2", "a", "b"]
        );
        assert_eq!(
            ids(&expand(&rep("int"), &data)),
            ["-1", "01", "1", "2", "10", "a", "b"]
        );
        let default = plots(
            json!({}),
            json!([{ "repeat": { "var": "v", "in": { "label_values":
                { "metric": "m", "label": "id" } } }, "body": [per_value()] }]),
        );
        assert_eq!(
            ids(&expand(&default, &data)),
            ids(&expand(&rep("str"), &data))
        );
    }

    /// The `int` sort is the same whatever order the catalog lists values in.
    #[test]
    fn int_sort_is_deterministic() {
        let mut a: Vec<String> = ["x", "1", "01", "1a", "9223372036854775807", "2", "001"]
            .map(String::from)
            .to_vec();
        let mut b = a.clone();
        b.reverse();
        sort_label_values(&mut a, LabelSort::Int);
        sort_label_values(&mut b, LabelSort::Int);
        // Integers first, even the largest; then the rest as strings.
        assert_eq!(a, ["001", "01", "1", "2", "9223372036854775807", "1a", "x"]);
        assert_eq!(a, b);
    }

    /// A catalog value is escaped where it lands in PromQL, and only there.
    #[test]
    fn catalog_values_are_escaped_in_queries() {
        let data = Catalog::default().gauge("m", &[&[("k", "a\"b\\c")]]);
        let t = plots(
            json!({}),
            json!([{ "repeat": { "var": "v", "in": { "label_values": { "metric": "m", "label": "k" } } },
                     "body": [
                { "kind": "gauge", "title": "${v}", "id": "i", "unit": "count",
                  "query": "m{k=\"${v}\"}", "query_total": "t{k=\"${v}\"}" },
                { "rate_mean": { "title": "${v}", "id": "r", "selector": "h{k=\"${v}\"}",
                                 "rate": "c{k=\"${v}\"}", "mean_unit": "time" } },
            ] }]),
        );
        let view = expand(&t, &data);
        let got = &view["groups"][0]["subgroups"][0]["plots"];
        assert_eq!(got[0]["opts"]["title"], "a\"b\\c");
        assert_eq!(got[0]["promql_query"], r#"m{k="a\"b\\c"}"#);
        assert_eq!(got[0]["promql_query_total"], r#"t{k="a\"b\\c"}"#);
        assert_eq!(got[1]["promql_query"], r#"c{k="a\"b\\c"}"#);
        assert_eq!(got[2]["promql_query"], r#"histogram_mean(h{k="a\"b\\c"})"#);
        // A `values` binding is inserted as given.
        let t = plots(
            json!({}),
            json!([{ "repeat": { "var": "v", "in": { "values": ["a\"b"] } },
                     "body": [{ "kind": "gauge", "title": "t", "id": "i", "unit": "count",
                                "query": "${v}" }] }]),
        );
        let view = expand(&t, &data);
        assert_eq!(
            view["groups"][0]["subgroups"][0]["plots"][0]["promql_query"],
            "a\"b"
        );
    }

    #[test]
    fn section_metadata() {
        let data = Catalog::default().gauge(
            "gpu",
            &[
                &[("id", "0"), ("vendor", "a")],
                &[("id", "1"), ("vendor", "a")],
            ],
        );
        let t = doc(json!([{ "name": "T", "route": "/t", "metadata": {
            "s": { "value": { "x": [1, 2] } },
            "gpu_selector": { "computed": "gpu_selector", "probe": ["gpu"] },
            "absent": { "computed": "gpu_selector", "probe": ["none"] },
        } }]));
        let view = expand(&t, &data);
        assert_eq!(view["metadata"]["s"], json!({ "x": [1, 2] }));
        assert_eq!(
            view["metadata"]["gpu_selector"],
            json!({ "enabled": true, "ids": [0, 1], "gpus": [
                { "id": 0, "vendor": "a" }, { "id": 1, "vendor": "a" } ] })
        );
        assert!(view["metadata"].get("absent").is_none());
    }

    #[test]
    fn builtin_query_and_sections() {
        let t = doc(json!([
            { "name": "A", "route": "/a" },
            { "builtin": "query" },
        ]));
        let t = SectionTemplate::parse(&t).unwrap();
        let routes: Vec<_> = t
            .sections()
            .into_iter()
            .map(|s| (s.name, s.route))
            .collect();
        assert_eq!(
            routes,
            [
                ("A".into(), "/a".into()),
                ("Query Explorer".into(), "/query".into())
            ]
        );
        let data = Catalog::default();
        assert!(
            t.expand(&data, "/query", Vec::new(), &BTreeMap::new())
                .unwrap()
                .is_some()
        );
        assert!(
            t.expand(&data, "/nope", Vec::new(), &BTreeMap::new())
                .unwrap()
                .is_none()
        );
        assert_eq!(t.schema(), 1);
    }

    /// The reviewer's examples: a misspelled or extra field is refused
    /// rather than read as another node or dropped.
    #[test]
    fn unknown_fields_are_refused() {
        let cases = [
            (
                plots(
                    json!({}),
                    json!([{ "when": { "present": "a" }, "then": [gauge("a")], "otherwise": [] }]),
                ),
                "unknown field \"otherwise\"",
            ),
            (
                doc(json!([{ "builtin": "query", "name": "Y", "route": "/x" }])),
                "unknown field \"name\"",
            ),
            (
                doc(
                    json!([{ "name": "T", "route": "/t", "metadata": { "k": { "computd": "gpu_selector" } } }]),
                ),
                "section metadata is",
            ),
            (
                doc(
                    json!([{ "name": "T", "route": "/t", "metadata": { "k": { "value": 1, "x": 2 } } }]),
                ),
                "unknown field \"x\"",
            ),
            (
                doc(
                    json!([{ "name": "T", "route": "/t", "metadata": { "k": { "computed": "cpu_selector", "probe": [] } } }]),
                ),
                "unknown variant `cpu_selector`",
            ),
            (
                plots(
                    json!({ "l": { "pred": { "present": "a" }, "x": 1 } }),
                    json!([]),
                ),
                "unknown field \"x\"",
            ),
            (
                plots(json!({ "l": { "frist_present": {} } }), json!([])),
                "a let is",
            ),
            (
                plots(
                    json!({}),
                    json!([{ "repeat": { "var": "v", "in": { "values": [] } }, "body": [], "x": 1 }]),
                ),
                "unknown field \"x\"",
            ),
            (
                plots(
                    json!({}),
                    json!([{ "split": { "when": { "present": "a" }, "plots": [gauge("a"), gauge("b")] }, "x": 1 }]),
                ),
                "unknown field \"x\"",
            ),
            (
                plots(
                    json!({}),
                    json!([{ "rate_mean": { "title": "t", "id": "i", "selector": "s", "mean_unit": "time" }, "x": 1 }]),
                ),
                "unknown field \"x\"",
            ),
            (
                plots(
                    json!({}),
                    json!([{ "when": { "present": "a", "has_counter": "b" }, "then": [] }]),
                ),
                "",
            ),
            (
                plots(
                    json!({}),
                    json!([{ "kind": "gauge", "title": "t", "id": "i", "unit": "count", "query": "q", "keep_empty": true }]),
                ),
                "unknown field `keep_empty`",
            ),
            (
                section(
                    json!({}),
                    json!([{ "name": "G", "id": "g", "keep_empty": true }]),
                ),
                "unknown field \"keep_empty\"",
            ),
        ];
        for (template, needle) in cases {
            assert_refused(&template, needle);
        }
    }

    #[test]
    fn errors_name_the_section() {
        let t = doc(json!([
            { "name": "A", "route": "/a" },
            { "name": "B", "route": "/b", "groups": [{ "name": "G", "id": "g", "x": 1 }] },
        ]));
        assert_refused(&t, "section 1 (\"/b\"): groups[0]: unknown field \"x\"");
    }

    /// Enumerated values are checked at parse, in branches that would not
    /// be expanded.
    #[test]
    fn enumerated_values_are_checked_at_parse() {
        let never = |plot: Value| {
            plots(
                json!({}),
                json!([{ "when": { "present": "x" }, "then": [], "else": [
                { "when": { "present": "y" }, "then": [plot] }
            ] }]),
            )
        };
        let plot = |extra: Value| {
            let mut p = gauge("p");
            p.as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            p
        };
        let cases = [
            (plot(json!({ "kind": "gauj" })), "unknown variant `gauj`"),
            (plot(json!({ "unit": "watts" })), "unknown variant `watts`"),
            (plot(json!({ "width": "half" })), "unknown variant `half`"),
            (
                plot(json!({ "kind": "histogram", "subtype": "heat" })),
                "unknown variant `heat`",
            ),
            (
                json!({ "rate_mean": { "title": "t", "id": "i", "selector": "s", "mean_unit": "count" } }),
                "unknown variant `count`",
            ),
            (plot(json!({ "title": "${" })), "unclosed `${`"),
            (plot(json!({ "title": "${a b}" })), "is not a name"),
            (plot(json!({ "title": "${}" })), "is not a name"),
            (
                plot(json!({ "query": "${nope}" })),
                "unknown name `${nope}`",
            ),
            (plot(json!({ "kind": "latency" })), "takes no `unit`"),
            (
                plot(json!({ "kind": "histogram" })),
                "takes a `unit` and a `subtype`",
            ),
            (plot(json!({ "subtype": "buckets" })), "no `subtype`"),
            (
                json!({ "kind": "gauge", "title": "t", "id": "i", "query": "q" }),
                "takes a `unit`",
            ),
            (
                plot(json!({ "percentage_range": true, "range": [0.0, 2.0] })),
                "cannot both be given",
            ),
            (
                json!({ "split": { "when": { "present": "a" }, "plots": [plot(json!({ "width": "full" })), gauge("b")] } }),
                "take no `width`",
            ),
            (
                json!({ "when": { "var": "nope" }, "then": [] }),
                "unknown predicate \"nope\"",
            ),
            (
                json!({ "when": { "param": "nope" }, "then": [] }),
                "not declared in `params`",
            ),
            (
                json!({ "when": { "distinct_gt": { "metrics": ["a"], "by": ["id", "x"], "n": 1, "each": true } }, "then": [] }),
                "takes one `by` label",
            ),
            (
                json!({ "when": { "distinct_gt": { "metrics": ["a"], "by": [], "n": 1 } }, "then": [] }),
                "1 to 16 `by` labels, not 0",
            ),
            (
                json!({ "when": { "has_label": { "metric": "${q}", "label": "l" } }, "then": [] }),
                "unknown name `${q}`",
            ),
        ];
        for (item, needle) in cases {
            assert_refused(&never(item), needle);
        }
    }

    #[test]
    fn names_are_checked_at_parse() {
        // A predicate let used as a value, and a value let as a predicate.
        let lets = json!({ "p": { "pred": { "present": "a" } }, "t": "x" });
        assert_refused(
            &plots(lets.clone(), json!([gauge("${p}")])),
            "names a predicate",
        );
        assert_refused(
            &plots(lets, json!([{ "when": { "var": "t" }, "then": [] }])),
            "names a value",
        );
        // A let cannot use another let, either way round alphabetically.
        for (a, b) in [("a", "b"), ("b", "a")] {
            let lets = json!({ a: "x", b: format!("${{{a}}}") });
            assert_refused(&plots(lets, json!([])), "unknown name");
            let lets = json!({ a: { "pred": { "present": "x" } }, b: { "pred": { "var": a } } });
            assert_refused(&plots(lets, json!([])), "unknown predicate");
            let lets = json!({ a: "x", b: { "first_present": { "list": [format!("${{{a}}}")], "default": "d" } } });
            assert_refused(&plots(lets, json!([])), "unknown name");
        }
        // Nor can a section let use a document let.
        let t = json!({ "schema": 1, "let": { "a": "x", "p": { "pred": { "present": "x" } } },
            "sections": [{ "name": "T", "route": "/t", "let": { "b": "${a}" } }] })
        .to_string();
        assert_refused(&t, "unknown name `${a}`");
        let t = t.replace(r#""b":"${a}""#, r#""b":{"pred":{"var":"p"}}"#);
        assert_refused(&t, "unknown predicate \"p\"");
        // A section let cannot reuse a document let's name or a parameter's.
        let t = json!({ "schema": 1, "let": { "a": "x" }, "sections": [
            { "name": "T", "route": "/t", "let": { "a": "y" } } ] })
        .to_string();
        assert_refused(&t, "reuses a name");
        assert_refused(
            &plots(json!({ "throughput": "y" }), json!([])),
            "reuses a name",
        );
        // A repeat cannot shadow a let, a parameter or an enclosing repeat.
        for var in ["t", "throughput", "o"] {
            let t = plots(
                json!({ "t": "x" }),
                json!([{ "repeat": { "var": "o", "in": { "values": [{ "f": "1" }] } }, "body": [
                    { "repeat": { "var": var, "in": { "values": ["1"] } }, "body": [] },
                ] }]),
            );
            assert_refused(&t, "reuses a name");
        }
        // A repeat's fields are known: `${o.g}` is not one of them.
        let t = plots(
            json!({}),
            json!([{ "repeat": { "var": "o", "in": { "values": [{ "f": "1" }] } },
                     "body": [gauge("${o.g}")] }]),
        );
        assert_refused(&t, "unknown name `${o.g}`");
        let t = plots(
            json!({}),
            json!([{ "repeat": { "var": "o", "in": { "values": [{ "f": "1" }, { "g": "2" }] } },
                     "body": [] }]),
        );
        assert_refused(&t, "the same fields");
        let t = plots(
            json!({}),
            json!([{ "repeat": { "var": "o", "in": { "values": ["a", { "g": "2" }] } },
                     "body": [] }]),
        );
        assert_refused(&t, "mixes strings and objects");
        let t = plots(
            json!({}),
            json!([{ "repeat": { "var": "o", "in": { "values": [1] } }, "body": [] }]),
        );
        assert_refused(&t, "not a string or an object of strings");
    }

    #[test]
    fn routes_are_unique() {
        assert_refused(
            &doc(json!([{ "name": "A", "route": "/a" }, { "name": "B", "route": "/a" }])),
            "two sections have route \"/a\"",
        );
        assert_refused(
            &doc(json!([{ "builtin": "query" }, { "name": "Q", "route": "/query" }])),
            "two sections have route \"/query\"",
        );
        assert_refused(
            &doc(json!([{ "builtin": "graphs" }])),
            "unknown variant `graphs`",
        );
    }

    /// `schema` is read first: a newer document is refused for its schema,
    /// whatever else it holds.
    #[test]
    fn schema_is_checked_first() {
        let e = parse_err(r#"{"schema": 2, "sections": [], "newfield": 1}"#);
        assert!(e.contains("schema 2 is not one this viewer expands"), "{e}");
        let e = parse_err(r#"{"schema": 0, "sections": []}"#);
        assert!(e.contains("schema 0"), "{e}");
        let e = parse_err(r#"{"sections": []}"#);
        assert!(e.contains("no `schema`"), "{e}");
        let e = parse_err(r#"{"schema": "1", "sections": []}"#);
        assert!(e.contains("not an integer"), "{e}");
        let e = parse_err(r#"{"schema": 1, "sections": [], "newfield": 1}"#);
        assert!(e.contains("unknown field \"newfield\""), "{e}");
        assert!(
            parse_err(
                &doc(json!([])).replace("\"params\":[\"throughput\"]", "\"params\":[\"a\",\"a\"]")
            )
            .contains("declared twice")
        );
    }

    /// Five nested repeats of ten would be 111,110 repeat passes.
    #[test]
    fn expansion_is_bounded_by_nodes() {
        let ten: Vec<String> = (0..10).map(|i| i.to_string()).collect();
        let mut body = json!([]);
        for var in ["a", "b", "c", "d", "e"] {
            body = json!([{ "repeat": { "var": var, "in": { "values": ten } }, "body": body }]);
        }
        let t = SectionTemplate::parse(&plots(json!({}), body)).unwrap();
        let e = t
            .expand(&Catalog::default(), "/t", Vec::new(), &BTreeMap::new())
            .err()
            .unwrap();
        assert_eq!(
            e.message(),
            format!("/t: expands to more than {MAX_NODES} nodes")
        );

        // A catalog with more label values than the budget.
        let ids: Vec<String> = (0..MAX_NODES).map(|i| i.to_string()).collect();
        let series: Vec<Vec<(&str, &str)>> = ids.iter().map(|i| vec![("id", i.as_str())]).collect();
        let series: Vec<&[(&str, &str)]> = series.iter().map(Vec::as_slice).collect();
        let data = Catalog::default().gauge("m", &series);
        let t = plots(
            json!({}),
            json!([{ "repeat": { "var": "v", "in": { "label_values": { "metric": "m", "label": "id" } } },
                     "body": [] }]),
        );
        let t = SectionTemplate::parse(&t).unwrap();
        let e = t
            .expand(&data, "/t", Vec::new(), &BTreeMap::new())
            .err()
            .unwrap();
        assert!(e.message().contains("nodes"), "{e}");
    }

    #[test]
    fn expansion_is_bounded_by_bytes() {
        let big = "x".repeat(MAX_STRING_BYTES);
        let passes: Vec<String> = (0..MAX_TEXT_BYTES / MAX_STRING_BYTES + 1)
            .map(|i| i.to_string())
            .collect();
        let t = plots(
            json!({}),
            json!([{ "repeat": { "var": "v", "in": { "values": passes } },
                     "body": [{ "kind": "gauge", "title": big, "id": "i", "unit": "count", "query": "q" }] }]),
        );
        let t = SectionTemplate::parse(&t).unwrap();
        let e = t
            .expand(&Catalog::default(), "/t", Vec::new(), &BTreeMap::new())
            .err()
            .unwrap();
        assert_eq!(
            e.message(),
            format!("/t: expands to more than {MAX_TEXT_BYTES} bytes")
        );
    }

    /// The Rezolus template stays well inside both limits on the largest
    /// catalog, so the limits refuse only a runaway document.
    #[test]
    fn the_rezolus_template_is_well_inside_the_limits() {
        let t = SectionTemplate::parse(&crate::rezolus_template::json()).unwrap();
        let data = crate::section_template_tests::sink_catalog();
        for section in &t.sections {
            let SectionDef::Section(body) = section else {
                continue;
            };
            let budget = Budget::default();
            t.expand_body(&data, body, Vec::new(), &BTreeMap::new(), &budget)
                .unwrap();
            assert!(budget.nodes.get() < MAX_NODES / 10, "{}", body.route);
            assert!(budget.bytes.get() < MAX_TEXT_BYTES / 10, "{}", body.route);
            assert!(budget.fuel.get() < MAX_FUEL / 10, "{}", body.route);
        }
    }

    /// Expand `/t` of `template` and return what it spent.
    fn spent(template: &str, data: &Catalog) -> Budget {
        let t = SectionTemplate::parse(template).unwrap_or_else(|e| panic!("{e}"));
        let SectionDef::Section(body) = &t.sections[0] else {
            panic!("not a section");
        };
        let budget = Budget::default();
        t.expand_body(data, body, Vec::new(), &BTreeMap::new(), &budget)
            .unwrap_or_else(|e| panic!("{e}"));
        budget
    }

    /// The message of the limit `/t` of `template` passes on `data`.
    fn expand_err(template: &str, data: &Catalog, params: &[(&str, &str)]) -> String {
        let t = SectionTemplate::parse(template).unwrap_or_else(|e| panic!("{e}"));
        let params = params
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let e = t.expand(data, "/t", Vec::new(), &params).err().unwrap();
        assert_eq!(e.kind(), TemplateErrorKind::Limit, "{e}");
        e.message().to_string()
    }

    /// The largest allocation this test binary's threads request while
    /// [`peak_alloc`] is measuring them.
    struct PeakAlloc;

    thread_local! {
        static MEASURING: Cell<bool> = const { Cell::new(false) };
        static PEAK: Cell<usize> = const { Cell::new(0) };
        static TOTAL: Cell<usize> = const { Cell::new(0) };
    }

    fn note(size: usize) {
        let _ = MEASURING.try_with(|on| {
            if on.get() {
                let _ = PEAK.try_with(|p| p.set(p.get().max(size)));
                let _ = TOTAL.try_with(|t| t.set(t.get().saturating_add(size)));
            }
        });
    }

    // SAFETY: every call forwards to `System` with the caller's arguments;
    // `note` only reads and writes two const-initialized thread-locals.
    unsafe impl std::alloc::GlobalAlloc for PeakAlloc {
        unsafe fn alloc(&self, layout: std::alloc::Layout) -> *mut u8 {
            note(layout.size());
            unsafe { std::alloc::System.alloc(layout) }
        }
        unsafe fn alloc_zeroed(&self, layout: std::alloc::Layout) -> *mut u8 {
            note(layout.size());
            unsafe { std::alloc::System.alloc_zeroed(layout) }
        }
        unsafe fn dealloc(&self, ptr: *mut u8, layout: std::alloc::Layout) {
            unsafe { std::alloc::System.dealloc(ptr, layout) }
        }
        unsafe fn realloc(
            &self,
            ptr: *mut u8,
            layout: std::alloc::Layout,
            new_size: usize,
        ) -> *mut u8 {
            note(new_size);
            unsafe { std::alloc::System.realloc(ptr, layout, new_size) }
        }
    }

    #[global_allocator]
    static ALLOC: PeakAlloc = PeakAlloc;

    /// `f`'s result and the largest single allocation it made on this thread.
    fn peak_alloc<R>(f: impl FnOnce() -> R) -> (R, usize) {
        let (out, peak, _) = measure_alloc(f);
        (out, peak)
    }

    /// `f`'s result, its largest single allocation and the bytes of all
    /// its allocations, on this thread.
    fn measure_alloc<R>(f: impl FnOnce() -> R) -> (R, usize, usize) {
        PEAK.with(|p| p.set(0));
        TOTAL.with(|t| t.set(0));
        MEASURING.with(|on| on.set(true));
        let out = f();
        MEASURING.with(|on| on.set(false));
        (out, PEAK.with(Cell::get), TOTAL.with(Cell::get))
    }

    /// A string that would be far over the byte budget is refused before
    /// it is built, whatever its pieces are bound from: a let, a parameter,
    /// a `values` field or a catalog value. Each would be 19 MB; no
    /// allocation during the refused expansion reaches 4 times the budget.
    #[test]
    fn a_string_over_the_byte_budget_is_refused_before_it_is_built() {
        // At the string limit, referenced 1200 times: 19 MB each if built.
        let big = "x".repeat(16_000);
        let refs = |name: &str| format!("${{{name}}}").repeat(1200);
        let title_plot = |name: &str| json!({ "kind": "gauge", "title": refs(name), "id": "i", "unit": "count", "query": "q" });
        let data = Catalog::default().gauge("m", &[&[("k", big.as_str())]]);
        let cases = [
            (
                plots(json!({ "big": big }), json!([title_plot("big")])),
                vec![],
            ),
            (
                plots(json!({}), json!([title_plot("throughput")])),
                vec![("throughput", big.as_str())],
            ),
            (
                plots(
                    json!({}),
                    json!([{ "repeat": { "var": "o", "in": { "values": [{ "f": big }] } },
                             "body": [title_plot("o.f")] }]),
                ),
                vec![],
            ),
            (
                plots(
                    json!({}),
                    json!([{ "repeat": { "var": "v", "in": { "label_values": { "metric": "m", "label": "k" } } },
                             "body": [title_plot("v")] }]),
                ),
                vec![],
            ),
        ];
        for (template, params) in cases {
            let t = SectionTemplate::parse(&template).unwrap();
            let params = params
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
            let (e, peak) = peak_alloc(|| {
                t.expand(&data, "/t", Vec::new(), &params)
                    .err()
                    .unwrap()
                    .message()
                    .to_string()
            });
            assert_eq!(
                e,
                format!("/t: expands to more than {MAX_TEXT_BYTES} bytes")
            );
            assert!(peak < 4 * MAX_TEXT_BYTES, "allocated {peak} bytes at once");
        }
        // An escaped catalog value is charged at its escaped length.
        let quotes = "\"".repeat(MAX_TEXT_BYTES / 2 + 1);
        let data = Catalog::default().gauge("m", &[&[("k", quotes.as_str())]]);
        let t = plots(
            json!({}),
            json!([{ "repeat": { "var": "v", "in": { "label_values": { "metric": "m", "label": "k" } } },
                     "body": [{ "kind": "gauge", "title": "t", "id": "i", "unit": "count",
                                "query": "m{k=\"${v}\"}" }] }]),
        );
        let t = SectionTemplate::parse(&t).unwrap();
        let (e, peak) = peak_alloc(|| {
            t.expand(&data, "/t", Vec::new(), &BTreeMap::new())
                .err()
                .unwrap()
                .message()
                .to_string()
        });
        assert_eq!(
            e,
            format!("/t: expands to more than {MAX_TEXT_BYTES} bytes")
        );
        // The label value itself is copied at half the budget; the escaped
        // copy, at the whole budget, is never started.
        assert!(peak < MAX_TEXT_BYTES, "allocated {peak} bytes at once");
    }

    /// Each catalog read is charged per series and per label byte: the
    /// same document costs at least that much more fuel on a catalog where
    /// `m` has 100 series of 3 labels than on one where it has none.
    #[test]
    fn catalog_reads_are_charged_per_series_and_label() {
        // Ids that do not parse, so the GPU selector reads the series but
        // produces nothing.
        let ids: Vec<String> = (0..100).map(|i| format!("a{i:02}")).collect();
        let series: Vec<Vec<(&str, &str)>> = ids
            .iter()
            .map(|i| vec![("id", i.as_str()), ("aa", "xx"), ("bb", "yy")])
            .collect();
        let series: Vec<&[(&str, &str)]> = series.iter().map(Vec::as_slice).collect();
        // One read of `m`'s series costs 100 × (1 + 13 label bytes).
        let read = 100 * (1 + 13);
        let full = Catalog::default().counter("m", &series);
        let empty = Catalog::default();
        let when = |pred: Value| plots(json!({}), json!([{ "when": pred, "then": [] }]));
        let cases = [
            (when(json!({ "present": "m" })), read),
            (
                when(json!({ "has_label": { "metric": "m", "label": "id" } })),
                read,
            ),
            (
                when(json!({ "distinct_gt": { "metrics": ["m", "m"], "by": ["id"], "n": 999 } })),
                read,
            ),
            (
                when(
                    json!({ "distinct_gt": { "metrics": ["m", "m"], "by": ["id"], "n": 999, "each": true } }),
                ),
                read,
            ),
            (
                plots(json!({ "p": { "pred": { "present": "m" } } }), json!([])),
                read,
            ),
            (
                plots(
                    json!({ "f": { "first_present": { "list": ["m"], "default": "d" } } }),
                    json!([]),
                ),
                read,
            ),
            (
                plots(
                    json!({}),
                    json!([{ "repeat": { "var": "v", "in": { "label_values": { "metric": "m", "label": "id" } } },
                             "body": [] }]),
                ),
                read,
            ),
            (
                doc(json!([{ "name": "T", "route": "/t", "metadata": {
                    "s": { "computed": "gpu_selector", "probe": ["m", "m"] } } }])),
                read,
            ),
            (
                plots(
                    json!({}),
                    json!([{ "split": { "when": { "present": "m" }, "plots": [gauge("a"), gauge("b")] } }]),
                ),
                read,
            ),
        ];
        for (template, at_least) in cases {
            let more = spent(&template, &full).fuel.get() - spent(&template, &empty).fuel.get();
            // An absent metric's read costs one unit, so each read adds one
            // less than its series cost; the rest of the document can differ
            // by a few units (`first_present` binds `m` or the default).
            let at_least = at_least - at_least / read - 8;
            assert!(more >= at_least, "{more} < {at_least}: {template}");
        }
        // A metric read twice is read from the source once: the second
        // `m` costs its lookups and walk, not another read.
        let twice =
            when(json!({ "distinct_gt": { "metrics": ["m", "m"], "by": ["id"], "n": 999 } }));
        let once = when(json!({ "distinct_gt": { "metrics": ["m"], "by": ["id"], "n": 999 } }));
        let second = spent(&twice, &full).fuel.get() - spent(&once, &full).fuel.get();
        // The walk and the hashed values cost about half a read here; a
        // second source read would cost a whole one more.
        assert!(second < read, "{second}");
        // `has_counter` scans the counter names: 100 more names of 4 bytes.
        let names: Vec<String> = (0..100).map(|i| format!("c{i:03}")).collect();
        let mut many = Catalog::default();
        for name in &names {
            many = many.counter(name, &[&[]]);
        }
        let t = when(json!({ "has_counter": "m" }));
        let more = spent(&t, &many).fuel.get() - spent(&t, &empty).fuel.get();
        assert!(more >= 100 * 5, "{more}");
        // A predicate costs fuel even when its metric is absent: one unit,
        // `x` scanned and copied (two), and three memo lookups (one plus
        // `x` each); the first also reads each type from the source once.
        let any = |n: usize| when(json!({ "any": vec![json!({ "present": "x" }); n] }));
        assert_eq!(
            spent(&any(50), &empty).fuel.get() - spent(&any(0), &empty).fuel.get(),
            50 * (1 + 2 + 3 * 3) + 3 * (64 + 1)
        );
    }

    /// The reviewer's case: a 5000-series metric listed 200 times in a
    /// `distinct_gt`, evaluated on each of 20 repeat passes, is 20 million
    /// series reads. It is refused at the fuel limit.
    #[test]
    fn expansion_is_bounded_by_catalog_work() {
        let ids: Vec<String> = (0..5000).map(|i| i.to_string()).collect();
        let series: Vec<Vec<(&str, &str)>> = ids.iter().map(|i| vec![("id", i.as_str())]).collect();
        let series: Vec<&[(&str, &str)]> = series.iter().map(Vec::as_slice).collect();
        let data = Catalog::default().counter("m", &series);
        let passes: Vec<String> = (0..20).map(|i| i.to_string()).collect();
        let t = plots(
            json!({}),
            json!([{ "repeat": { "var": "r", "in": { "values": passes } }, "body": [
                { "when": { "distinct_gt": { "metrics": vec!["m"; 200], "by": ["id"], "n": 1_000_000 } },
                  "then": [] },
            ] }]),
        );
        let e = expand_err(&t, &data, &[]);
        assert_eq!(
            e,
            format!("/t: expands to more than {MAX_FUEL} units of fuel")
        );
    }

    /// Strings, lets, section and group metadata, the GPU selector and
    /// percentiles are each charged to the byte budget.
    #[test]
    fn bytes_are_charged_for_each_kind_of_output() {
        let data = Catalog::default().gauge(
            "gpu",
            &[
                &[("id", "0"), ("vendor", "a")],
                &[("id", "1"), ("vendor", "a")],
            ],
        );
        let k = "x".repeat(1000);
        let with_group_meta = |v: &str| {
            section(
                json!({}),
                json!([{ "name": "G", "id": "g", "metadata": { "m": v },
                         "subgroups": [{ "plots": [gauge("a")] }] }]),
            )
        };
        let with_section_meta =
            |meta: Value| doc(json!([{ "name": "T", "route": "/t", "metadata": { "m": meta } }]));
        let with_percentiles = |n: usize| {
            plots(
                json!({}),
                json!([{ "kind": "histogram", "title": "h", "id": "h", "unit": "time",
                         "subtype": "percentiles", "query": "q", "percentiles": vec![0.5; n] }]),
            )
        };
        let with_let = |v: &str| plots(json!({ "l": v }), json!([]));
        let bytes = |t: &str| spent(t, &data).bytes.get();
        assert!(bytes(&with_group_meta(&k)) >= bytes(&with_group_meta("")) + 1000);
        assert!(
            bytes(&with_section_meta(json!({ "value": k })))
                >= bytes(&with_section_meta(json!({ "value": "" }))) + 1000
        );
        assert!(
            bytes(&with_section_meta(
                json!({ "computed": "gpu_selector", "probe": ["gpu"] })
            )) > bytes(&with_section_meta(
                json!({ "computed": "gpu_selector", "probe": ["none"] })
            ))
        );
        assert!(bytes(&with_percentiles(1000)) >= bytes(&with_percentiles(0)) + 8000);
        assert!(bytes(&with_let(&k)) >= bytes(&with_let("")) + 1000);
    }

    /// Serde's derived impls read a struct from a JSON array of its fields
    /// in order; every struct position refuses one.
    #[test]
    fn arrays_in_place_of_objects_are_refused() {
        let plot = json!(["gauge", "Title", "pid", "count", null, "q"]);
        let cases = [
            (
                plots(json!({}), json!([plot])),
                "expected an object, found an array",
            ),
            (
                section(json!({}), json!([["G", "g", {}, []]])),
                "expected an object",
            ),
            (
                section(
                    json!({}),
                    json!([{ "name": "G", "id": "g", "subgroups": [["S", null, []]] }]),
                ),
                "expected an object",
            ),
            (doc(json!([["T", "/t"]])), "expected an object"),
            (
                plots(
                    json!({}),
                    json!([{ "when": { "has_label": ["m", "l", null] }, "then": [] }]),
                ),
                "expected an object",
            ),
            (
                plots(
                    json!({}),
                    json!([{ "when": { "distinct_gt": [["m"], ["id"], 1, false, []] }, "then": [] }]),
                ),
                "expected an object",
            ),
            (
                plots(
                    json!({}),
                    json!([{ "repeat": ["v", { "values": [] }], "body": [] }]),
                ),
                "expected an object",
            ),
            (
                plots(
                    json!({}),
                    json!([{ "repeat": { "var": "v", "in": { "label_values": ["m", "l", "str"] } }, "body": [] }]),
                ),
                "expected an object",
            ),
            (
                plots(
                    json!({}),
                    json!([{ "split": [{ "present": "a" }, [gauge("a"), gauge("b")]] }]),
                ),
                "expected an object",
            ),
            (
                plots(
                    json!({}),
                    json!([{ "split": { "when": { "present": "a" }, "plots": [plot, gauge("b")] } }]),
                ),
                "expected an object",
            ),
            (
                plots(
                    json!({}),
                    json!([{ "split": { "when": { "present": "a" }, "plots": [gauge("b")] } }]),
                ),
                "a split has 2 plots, not 1",
            ),
            (
                plots(
                    json!({}),
                    json!([{ "rate_mean": ["t", "i", "s", null, "time"] }]),
                ),
                "expected an object",
            ),
            (
                plots(json!({ "f": { "first_present": [["m"], "d"] } }), json!([])),
                "expected an object",
            ),
            (
                doc(
                    json!([{ "name": "T", "route": "/t", "metadata": { "k": ["gpu_selector", []] } }]),
                ),
                "section metadata is",
            ),
            (
                r#"[1, [], []]"#.to_string(),
                "expected an object, found an array",
            ),
        ];
        for (template, needle) in cases {
            assert_refused(&template, needle);
        }
    }

    #[test]
    fn repeated_keys_are_refused() {
        let cases = [
            r#"{"schema": 1, "schema": 1, "sections": []}"#,
            r#"{"schema": 1, "let": {"a": "x", "a": "y"}, "sections": []}"#,
            r#"{"schema": 1, "sections": [{"name": "T", "route": "/t", "route": "/u"}]}"#,
            r#"{"schema": 1, "sections": [{"name": "T", "route": "/t", "metadata": {"m": {"value": {"k": 1, "k": 2}}}}]}"#,
            r#"{"schema": 1, "sections": [{"name": "T", "route": "/t", "groups": [{"name": "G", "id": "g", "metadata": {"s": 1, "s": 2}}]}]}"#,
        ];
        for template in cases {
            assert_refused(template, "is repeated");
        }
    }

    #[test]
    fn sections_are_capped_and_routes_checked() {
        let many: Vec<Value> = (0..=MAX_SECTIONS)
            .map(|i| json!({ "name": "S", "route": format!("/s{i}") }))
            .collect();
        assert_refused(&doc(json!(many)), &format!("at most {MAX_SECTIONS}"));
        let fine: Vec<Value> = (0..MAX_SECTIONS - 1)
            .map(|i| json!({ "name": "S", "route": format!("/s{i}") }))
            .chain([json!({ "builtin": "query" })])
            .collect();
        assert!(SectionTemplate::parse(&doc(json!(fine))).is_ok());
        let long = format!("/{}", "a".repeat(MAX_SECTION_NAME_BYTES));
        for route in [
            "",
            "x",
            "/",
            "/a//b",
            "/a/",
            "/A",
            "/a/../b",
            "/a.b",
            "//a",
            long.as_str(),
        ] {
            assert_refused(
                &doc(json!([{ "name": "S", "route": route }])),
                &format!("route {route:?} is not"),
            );
        }
        for route in ["/hw_sensors", "/a-b/c_1", "/0"] {
            assert!(SectionTemplate::parse(&doc(json!([{ "name": "S", "route": route }]))).is_ok());
        }
        let long_name = "n".repeat(MAX_SECTION_NAME_BYTES + 1);
        for name in ["", long_name.as_str()] {
            assert_refused(
                &doc(json!([{ "name": name, "route": "/a" }])),
                "section name",
            );
        }
    }

    /// A catalog value in a PromQL field goes only inside a double-quoted
    /// string, where it is escaped.
    #[test]
    fn catalog_values_in_promql_fields_must_be_double_quoted() {
        let rep = |plot: Value| {
            plots(
                json!({}),
                json!([{ "repeat": { "var": "v", "in": { "label_values": { "metric": "m", "label": "k" } } },
                         "body": [plot] }]),
            )
        };
        let query = |q: &str| json!({ "kind": "gauge", "title": "t", "id": "i", "unit": "count", "query": q });
        let refused = [
            query("m{k='${v}'}"),
            query("m{k=`${v}`}"),
            query("${v}"),
            query(r#"m{k="a"} + ${v}"#),
            query(r#"m{k="a\"b"} + ${v}"#),
            json!({ "kind": "gauge", "title": "t", "id": "i", "unit": "count", "query": "q",
                    "query_total": "m{k='${v}'}" }),
            json!({ "rate_mean": { "title": "t", "id": "i", "selector": "h{k='${v}'}", "mean_unit": "time" } }),
            json!({ "rate_mean": { "title": "t", "id": "i", "selector": "h", "rate": "c{k='${v}'}", "mean_unit": "time" } }),
        ];
        for plot in refused {
            assert_refused(&rep(plot), "goes only inside a double-quoted string");
        }
        let accepted = [
            query(r#"m{k="${v}"}"#),
            query(r#"m{k="pre-${v}-post"}"#),
            query(r#"m{k="a\"${v}"}"#),
            query(r#"m{j='x"y', k="${v}"}"#),
            json!({ "kind": "gauge", "title": "'${v}' ${v}", "id": "${v}", "unit": "count", "query": "q" }),
        ];
        for plot in accepted {
            assert!(SectionTemplate::parse(&rep(plot.clone())).is_ok(), "{plot}");
        }
        // A newline in a label value is escaped too.
        let data = Catalog::default().gauge("m", &[&[("k", "a\nb")]]);
        let view = expand(&rep(query(r#"m{k="${v}"}"#)), &data);
        assert_eq!(
            view["groups"][0]["subgroups"][0]["plots"][0]["promql_query"],
            "m{k=\"a\\nb\"}"
        );
    }

    /// Every string field is checked for its names, in every position.
    #[test]
    fn every_string_field_is_checked() {
        let bad = "${nope}";
        let mut plot_fields = Vec::new();
        for field in [
            "title",
            "id",
            "query",
            "unit_system",
            "description",
            "axis_label",
            "row_label",
            "style",
            "query_total",
        ] {
            let mut p = gauge("p");
            p[field] = json!(bad);
            plot_fields.push(p);
        }
        let mut items: Vec<Value> = plot_fields.clone();
        items.push(json!({ "split": { "when": { "present": "a" }, "plots": [gauge("a"), plot_fields[0].clone()] } }));
        items.push(
            json!({ "split": { "when": { "present": bad }, "plots": [gauge("a"), gauge("b")] } }),
        );
        for field in ["title", "id", "selector", "rate"] {
            let mut rm = json!({ "title": "t", "id": "i", "selector": "s", "rate": "r", "mean_unit": "time" });
            rm[field] = json!(bad);
            items.push(json!({ "rate_mean": rm }));
        }
        for pred in [
            json!({ "present": bad }),
            json!({ "has_counter": bad }),
            json!({ "has_label": { "metric": bad, "label": "l" } }),
            json!({ "has_label": { "metric": "m", "label": bad } }),
            json!({ "has_label": { "metric": "m", "label": "l", "value": bad } }),
            json!({ "distinct_gt": { "metrics": [bad], "by": ["id"], "n": 1 } }),
            json!({ "distinct_gt": { "metrics": ["m"], "by": [bad], "n": 1 } }),
            json!({ "distinct_gt": { "metrics": ["m"], "by": ["id", bad], "int": [bad], "n": 1 } }),
            json!({ "not": { "any": [{ "all": [{ "present": bad }] }] } }),
        ] {
            items.push(json!({ "when": pred, "then": [] }));
        }
        items.push(json!({ "repeat": { "var": "v", "in": { "label_values": { "metric": bad, "label": "l" } } }, "body": [] }));
        items.push(json!({ "repeat": { "var": "v", "in": { "label_values": { "metric": "m", "label": bad } } }, "body": [] }));
        for item in items {
            assert_refused(
                &plots(json!({}), json!([item.clone()])),
                "unknown name `${nope}`",
            );
        }
        let subgroups = [
            json!({ "name": bad, "plots": [] }),
            json!({ "description": bad, "plots": [] }),
        ];
        for sub in subgroups {
            assert_refused(
                &section(
                    json!({}),
                    json!([{ "name": "G", "id": "g", "subgroups": [sub] }]),
                ),
                "unknown name",
            );
        }
        for (name, id) in [(bad, "g"), ("G", bad)] {
            assert_refused(
                &section(json!({}), json!([{ "name": name, "id": id }])),
                "unknown name",
            );
        }
        for lets in [
            json!({ "l": bad }),
            json!({ "l": { "first_present": { "list": [bad], "default": "d" } } }),
            json!({ "l": { "first_present": { "list": [], "default": bad } } }),
            json!({ "l": { "pred": { "present": bad } } }),
        ] {
            assert_refused(&plots(lets, json!([])), "unknown name");
        }
        assert_refused(
            &doc(json!([{ "name": "T", "route": "/t", "metadata": {
                "s": { "computed": "gpu_selector", "probe": [bad] } } }])),
            "unknown name",
        );
        // A repeat over objects is used through its fields.
        assert_refused(
            &plots(
                json!({}),
                json!([{ "repeat": { "var": "o", "in": { "values": [{ "f": "1" }] } }, "body": [gauge("${o}")] }]),
            ),
            "names a repeat over objects",
        );
    }

    #[test]
    fn names_must_be_names() {
        for param in ["a b", "1x", "", "a.b"] {
            let t = json!({ "schema": 1, "params": [param], "sections": [] }).to_string();
            assert_refused(&t, "is not a name");
        }
        assert_refused(&plots(json!({ "a-b": "x" }), json!([])), "is not a name");
        for var in ["a-b", "a.b", "1"] {
            assert_refused(
                &plots(
                    json!({}),
                    json!([{ "repeat": { "var": var, "in": { "values": [] } }, "body": [] }]),
                ),
                "is not a name",
            );
        }
        assert_refused(
            &plots(
                json!({}),
                json!([{ "repeat": { "var": "o", "in": { "values": [{ "a-b": "1" }] } }, "body": [] }]),
            ),
            "is not a name",
        );
        assert_refused(
            &plots(
                json!({}),
                json!([{ "when": { "distinct_gt": { "metrics": ["a"], "by": ["id"], "int": ["id"], "n": 1, "each": true } }, "then": [] }]),
            ),
            "takes one `by` label and no `int`",
        );
    }

    #[test]
    fn range_is_min_below_max() {
        for range in [[5.0, 1.0], [1.0, 1.0]] {
            let mut p = gauge("p");
            p["range"] = json!(range);
            assert_refused(&plots(json!({}), json!([p])), "min below max");
        }
    }

    #[test]
    fn params_lists_the_declared_parameters() {
        let t = json!({ "schema": 1, "params": ["b", "a"], "sections": [] }).to_string();
        let t = SectionTemplate::parse(&t).unwrap();
        assert_eq!(t.params().collect::<Vec<_>>(), ["a", "b"]);
    }

    #[test]
    fn json_size_counts_each_value_in_memory() {
        let v = std::mem::size_of::<Value>();
        assert_eq!(json_size(&json!({})), v);
        assert_eq!(json_size(&json!([])), v);
        assert_eq!(json_size(&json!(null)), v);
        assert_eq!(json_size(&json!([{}, 1])), 3 * v);
        assert_eq!(json_size(&json!({ "ab": "cd" })), 2 * v + 4);
    }

    /// Group metadata is charged in every group it is copied into, key
    /// bytes included: empty objects in many groups pass the byte limit.
    #[test]
    fn group_metadata_is_charged_per_copy() {
        let meta = |key: &str| {
            section(
                json!({}),
                json!([{ "name": "G", "id": "g", "metadata": { key: {} },
                         "subgroups": [{ "plots": [gauge("a")] }] }]),
            )
        };
        let bytes = |t: &str| spent(t, &Catalog::default()).bytes.get();
        let long = "k".repeat(1000);
        assert!(bytes(&meta(&long)) >= bytes(&meta("k")) + 999);
        let empties: serde_json::Map<String, Value> =
            (0..1000).map(|i| (format!("k{i}"), json!({}))).collect();
        let passes: Vec<String> = (0..1000).map(|i| i.to_string()).collect();
        let t = section(
            json!({}),
            json!([{ "repeat": { "var": "r", "in": { "values": passes } }, "body": [
                { "name": "G", "id": "g${r}", "metadata": empties,
                  "subgroups": [{ "plots": [gauge("a")] }] },
            ] }]),
        );
        assert_eq!(
            expand_err(&t, &Catalog::default(), &[]),
            format!("/t: expands to more than {MAX_TEXT_BYTES} bytes")
        );
    }

    /// Each field a pass over objects binds is a node.
    #[test]
    fn repeat_fields_are_charged_as_nodes() {
        let fields: serde_json::Map<String, Value> =
            (0..10).map(|i| (format!("f{i}"), json!("x"))).collect();
        let t = plots(
            json!({}),
            json!([{ "repeat": { "var": "o", "in": { "values": [fields.clone(), fields] } },
                     "body": [] }]),
        );
        // The group and subgroup, the repeat node, two passes, ten fields
        // each.
        assert_eq!(spent(&t, &Catalog::default()).nodes.get(), 2 + 1 + 2 + 20);
        let fields: serde_json::Map<String, Value> =
            (0..300).map(|i| (format!("f{i}"), json!("x"))).collect();
        let values = vec![Value::Object(fields); 100];
        let t = plots(
            json!({}),
            json!([{ "repeat": { "var": "o", "in": { "values": values } }, "body": [] }]),
        );
        assert_eq!(
            expand_err(&t, &Catalog::default(), &[]),
            format!("/t: expands to more than {MAX_NODES} nodes")
        );
    }

    /// Counting tuples costs a lookup per series per `by` label, and `by`
    /// names at most 16 labels.
    #[test]
    fn distinct_gt_is_charged_per_label() {
        let data = Catalog::default()
            .counter("m", &[&[("a", "0")], &[("a", "1")], &[("a", "2")]])
            .gauge("m", &[&[("a", "0")], &[("a", "1")]]);
        let t = plots(
            json!({}),
            json!([{ "when": { "distinct_gt": { "metrics": ["m"], "by": ["a", "b", "c"], "n": 99 } },
                     "then": [] }]),
        );
        let by1 = plots(
            json!({}),
            json!([{ "when": { "distinct_gt": { "metrics": ["m"], "by": ["a"], "n": 99 } },
                     "then": [] }]),
        );
        // Two more `by` labels: each scanned and copied (2 + 2), and one
        // unit plus a byte per series per label (5 series); `b` and `c` are
        // absent, so no value bytes.
        let fuel = |t: &str| spent(t, &data).fuel.get();
        // Per series, one unit plus one byte per extra label (5 series);
        // finding the `int` labels once costs the same again.
        assert_eq!(fuel(&t) - fuel(&by1), 4 + 5 * 4 + 4);
        // The same label three times costs three lookups and hashes the
        // value three times: one byte per series per extra label more.
        let dup = plots(
            json!({}),
            json!([{ "when": { "distinct_gt": { "metrics": ["m"], "by": ["a", "a", "a"], "n": 99 } },
                     "then": [] }]),
        );
        assert_eq!(fuel(&dup) - fuel(&by1), 4 + 5 * 4 + 5 * 2 + 4);
        let by: Vec<String> = (0..=MAX_BY_LABELS).map(|i| format!("l{i}")).collect();
        assert_refused(
            &plots(
                json!({}),
                json!([{ "when": { "distinct_gt": { "metrics": ["m"], "by": by, "n": 1 } }, "then": [] }]),
            ),
            "1 to 16 `by` labels, not 17",
        );
        assert_eq!(MAX_BY_LABELS, 16);
    }

    #[test]
    fn the_section_limit_is_64() {
        assert_eq!(MAX_SECTIONS, 64);
    }

    /// The scanner's other states: a backslash before `${`, a backtick
    /// string holding a `"`, and a `#` comment.
    #[test]
    fn catalog_values_cannot_leave_the_string_another_way() {
        let rep = |q: &str| {
            plots(
                json!({}),
                json!([{ "repeat": { "var": "v", "in": { "label_values": { "metric": "m", "label": "k" } } },
                         "body": [{ "kind": "gauge", "title": "t", "id": "i", "unit": "count", "query": q }] }]),
            )
        };
        for q in [r#"m{k="\${v}"}"#, r"m{k='\${v}'}", r"\${v}"] {
            assert_refused(&rep(q), "after a `\\`");
        }
        assert_refused(
            &plots(
                json!({}),
                json!([{ "kind": "gauge", "title": "t", "id": "i", "unit": "count",
                                        "query": r"x\${throughput}" }]),
            ),
            "after a `\\`",
        );
        // A template string holds no newline, so a `#` comment runs to the
        // end of the field.
        assert_refused(&rep("sum(m)\n"), "holds the control character U+000A");
        for q in [
            r#"m{k=`"${v}"`}"#,
            r#"sum(m) # "${v}""#,
            r#"sum(m) # x m{k="${v}"}"#,
        ] {
            assert_refused(&rep(q), "goes only inside a double-quoted string");
        }
        for q in [r#"m{j=`a"b`, k="${v}"}"#, r##"m{j="#", k="${v}"}"##] {
            assert!(SectionTemplate::parse(&rep(q)).is_ok(), "{q:?}");
        }
        // A carriage return in a label value is escaped.
        let data = Catalog::default().gauge("m", &[&[("k", "a\rb")]]);
        let view = expand(&rep(r#"m{k="${v}"}"#), &data);
        assert_eq!(
            view["groups"][0]["subgroups"][0]["plots"][0]["promql_query"],
            "m{k=\"a\\rb\"}"
        );
    }

    #[test]
    fn errors_have_a_kind() {
        let e = SectionTemplate::parse("{}").unwrap_err();
        assert_eq!(e.kind(), TemplateErrorKind::Invalid);
        assert_eq!(e.to_string(), e.message());
    }

    /// The byte budget's escaped length is the escaped string's length.
    #[test]
    fn promql_escaped_len_matches_the_escape() {
        let text = "a\\b\"c\nd\re";
        let mut out = String::new();
        push_promql_escaped(&mut out, text);
        assert_eq!(out, "a\\\\b\\\"c\\nd\\re");
        assert_eq!(promql_escaped_len(text), out.len());
    }

    #[test]
    fn documents_are_capped_in_entries_strings_and_names() {
        let wide: Vec<String> = (0..=MAX_ENTRIES).map(|i| i.to_string()).collect();
        assert_refused(
            &plots(
                json!({}),
                json!([{ "repeat": { "var": "v", "in": { "values": wide } }, "body": [] }]),
            ),
            "an array of more than 4096 entries",
        );
        let many: serde_json::Map<String, Value> = (0..=MAX_ENTRIES)
            .map(|i| (format!("l{i}"), json!("x")))
            .collect();
        assert_refused(
            &plots(Value::Object(many), json!([])),
            "an object of more than 4096 entries",
        );
        let long = "x".repeat(MAX_STRING_BYTES + 1);
        assert_refused(
            &plots(json!({}), json!([gauge(&long)])),
            "a string of 16385 bytes",
        );
        assert_refused(
            &plots(json!({ long.clone(): "x" }), json!([])),
            "a string of 16385 bytes",
        );
        let fine = "x".repeat(MAX_STRING_BYTES);
        assert!(SectionTemplate::parse(&plots(json!({}), json!([gauge(&fine)]))).is_ok());
        assert_eq!(
            (MAX_ENTRIES, MAX_STRING_BYTES, MAX_NAME_BYTES),
            (4096, 16384, 64)
        );
        let name = "n".repeat(MAX_NAME_BYTES + 1);
        assert_refused(
            &plots(json!({ name.clone(): "x" }), json!([])),
            "is not a name",
        );
        assert_refused(
            &json!({ "schema": 1, "params": [name.clone()], "sections": [] }).to_string(),
            "is not a name",
        );
        assert_refused(
            &plots(
                json!({}),
                json!([{ "repeat": { "var": name.clone(), "in": { "values": [] } }, "body": [] }]),
            ),
            "is not a name",
        );
        assert_refused(
            &plots(
                json!({}),
                json!([{ "repeat": { "var": "o", "in": { "values": [{ name.clone(): "x" }] } }, "body": [] }]),
            ),
            "is not a name",
        );
        let ok = "n".repeat(MAX_NAME_BYTES);
        assert!(SectionTemplate::parse(&plots(json!({ ok: "x" }), json!([]))).is_ok());
    }

    #[test]
    fn distinct_gt_int_labels_are_by_labels() {
        assert_refused(
            &plots(
                json!({}),
                json!([{ "when": { "distinct_gt": { "metrics": ["m"], "by": ["id"], "int": ["x"], "n": 1 } }, "then": [] }]),
            ),
            "`int` label \"x\" is not one of its `by` labels",
        );
    }

    /// A reference to an empty value still costs fuel, and so does the
    /// text scanned.
    #[test]
    fn references_to_empty_values_cost_fuel() {
        let title = |n: usize| {
            plots(
                json!({ "e": "" }),
                json!([{ "kind": "gauge", "title": "${e}".repeat(n), "id": "i", "unit": "count", "query": "q" }]),
            )
        };
        let more = spent(&title(1000), &Catalog::default()).fuel.get()
            - spent(&title(0), &Catalog::default()).fuel.get();
        // Each reference: one unit, plus its four bytes scanned.
        assert!(more >= 1000 * 5 - 1, "{more}");
        let t = plots(
            json!({ "e": "" }),
            json!([{ "repeat": { "var": "r", "in": { "values": (0..4000).map(|i| i.to_string()).collect::<Vec<_>>() } },
                     "body": [{ "kind": "gauge", "title": "${e}".repeat(4000), "id": "i", "unit": "count", "query": "q" }] }]),
        );
        assert_eq!(
            expand_err(&t, &Catalog::default(), &[]),
            format!("/t: expands to more than {MAX_FUEL} units of fuel")
        );
    }

    /// Parsing moves each part of the document out of its parent: the
    /// bytes allocated to parse a document do not grow with how deeply its
    /// content is nested.
    #[test]
    fn parse_is_linear_in_nesting_depth() {
        let leaf: Vec<String> = (0..4000).map(|i| format!("value-{i}")).collect();
        let nested = |depth: usize| {
            let mut body =
                json!([{ "repeat": { "var": "v", "in": { "values": leaf } }, "body": [] }]);
            for _ in 0..depth {
                body = json!([{ "when": { "present": "m" }, "then": body }]);
            }
            plots(json!({}), body)
        };
        let alloc = |depth: usize| {
            let text = nested(depth);
            let (t, _, total) = measure_alloc(|| SectionTemplate::parse(&text));
            assert!(t.is_ok());
            total
        };
        let (shallow, deep) = (alloc(1), alloc(30));
        assert!(
            deep < shallow * 2,
            "depth 30 allocated {deep} bytes, depth 1 {shallow}"
        );
    }

    #[test]
    fn more_refusals() {
        // An empty `values` list binds nothing.
        assert_refused(
            &plots(
                json!({}),
                json!([{ "repeat": { "var": "v", "in": { "values": [] } }, "body": [gauge("${v}")] }]),
            ),
            "unknown name `${v}`",
        );
        // An escaped quote in a single-quoted string does not end it, and
        // the `"` after it does not open a double-quoted one.
        let t = plots(
            json!({}),
            json!([{ "repeat": { "var": "v", "in": { "label_values": { "metric": "m", "label": "k" } } },
                     "body": [{ "kind": "gauge", "title": "t", "id": "i", "unit": "count",
                                "query": r#"m{k='\'"${v}"'}"# }] }]),
        );
        assert_refused(&t, "goes only inside a double-quoted string");
    }

    /// An error quotes at most 200 characters of the text it names.
    #[test]
    fn errors_cut_long_text() {
        let long = format!("{}${{", "x".repeat(MAX_STRING_BYTES - 2));
        let e = parse_err(&plots(json!({}), json!([gauge(&long)])));
        assert!(e.len() < 600, "{} bytes", e.len());
        assert!(e.contains(&format!("({} bytes)", long.len())), "{e}");
    }

    /// A small deterministic generator of hostile documents: deep repeats,
    /// long lists, empty bindings, many predicates.
    struct Gen {
        state: u64,
        /// Items left to generate in this document, so a document stays
        /// small while its expansion need not.
        items: usize,
    }

    impl Gen {
        fn next(&mut self) -> u64 {
            // xorshift64
            self.state ^= self.state << 13;
            self.state ^= self.state >> 7;
            self.state ^= self.state << 17;
            self.state
        }
        fn below(&mut self, n: usize) -> usize {
            (self.next() % n as u64) as usize
        }
        fn chance(&mut self, percent: usize) -> bool {
            self.below(100) < percent
        }
    }

    #[derive(Clone)]
    enum Bind {
        Text(String),
        Catalog(String),
    }

    fn gen_pred(g: &mut Gen, depth: usize) -> Value {
        let metric = ["m", "g", "h", "absent"][g.below(4)];
        match g.below(if depth >= 2 { 5 } else { 8 }) {
            0 => json!({ "present": metric }),
            1 => json!({ "has_counter": metric }),
            2 => {
                json!({ "has_label": { "metric": metric, "label": "id", "value": g.below(30).to_string() } })
            }
            3 => {
                json!({ "distinct_gt": { "metrics": vec![metric; 1 + g.below(64)], "by": ["id", "k"],
                                          "int": ["id"], "n": g.below(20) } })
            }
            4 => json!({ "distinct_gt": { "metrics": vec![metric; 1 + g.below(64)], "by": ["id"],
                                          "each": true, "n": g.below(20) } }),
            5 => {
                json!({ "any": (0..g.below(8)).map(|_| gen_pred(g, depth + 1)).collect::<Vec<_>>() })
            }
            6 => {
                json!({ "all": (0..g.below(8)).map(|_| gen_pred(g, depth + 1)).collect::<Vec<_>>() })
            }
            _ => json!({ "not": gen_pred(g, depth + 1) }),
        }
    }

    fn gen_text(g: &mut Gen, scope: &[Bind], promql: bool) -> String {
        let mut out = String::from("t");
        for _ in 0..g.below(40) {
            if scope.is_empty() || g.chance(30) {
                out.push_str("${e}");
                continue;
            }
            match &scope[g.below(scope.len())] {
                Bind::Text(name) => out.push_str(&format!("${{{name}}}")),
                Bind::Catalog(name) if promql => out.push_str(&format!("{{k=\"${{{name}}}\"}}")),
                Bind::Catalog(name) => out.push_str(&format!("${{{name}}}")),
            }
        }
        out
    }

    fn gen_items(
        g: &mut Gen,
        depth: usize,
        scope: &mut Vec<Bind>,
        leaf: &mut dyn FnMut(&mut Gen, &[Bind]) -> Value,
    ) -> Value {
        let mut items = Vec::new();
        for _ in 0..1 + g.below(6) {
            if g.items == 0 && !items.is_empty() {
                break;
            }
            g.items = g.items.saturating_sub(1);
            let roll = if depth >= 6 || g.items == 0 {
                9
            } else {
                g.below(10)
            };
            let item = match roll {
                0..=2 => json!({ "when": gen_pred(g, 0),
                                 "then": gen_items(g, depth + 1, scope, leaf),
                                 "else": gen_items(g, depth + 1, scope, leaf) }),
                3..=5 => {
                    let var = format!("v{}", scope.len());
                    let (over, binds) = match g.below(3) {
                        0 => (
                            json!({ "values": (0..g.below(64)).map(|i| "x".repeat(i)).collect::<Vec<_>>() }),
                            vec![Bind::Text(var.clone())],
                        ),
                        1 => {
                            let n = g.below(64);
                            let fields = 1 + g.below(8);
                            let values: Vec<Value> = (0..n)
                                .map(|i| {
                                    Value::Object(
                                        (0..fields)
                                            .map(|f| (format!("f{f}"), json!(i.to_string())))
                                            .collect(),
                                    )
                                })
                                .collect();
                            let binds = if n == 0 {
                                Vec::new()
                            } else {
                                (0..fields)
                                    .map(|f| Bind::Text(format!("{var}.f{f}")))
                                    .collect()
                            };
                            (json!({ "values": values }), binds)
                        }
                        _ => (
                            json!({ "label_values": { "metric": "m", "label": "id", "sort": "int" } }),
                            vec![Bind::Catalog(var.clone())],
                        ),
                    };
                    // A list of strings that is empty binds nothing either.
                    let binds = if over["values"].as_array().is_some_and(|v| v.is_empty()) {
                        Vec::new()
                    } else {
                        binds
                    };
                    let mark = scope.len();
                    scope.extend(binds);
                    // Keep the variable names unique by padding the scope.
                    if scope.len() == mark {
                        scope.push(Bind::Text("e".into()));
                    }
                    let body = gen_items(g, depth + 1, scope, leaf);
                    scope.truncate(mark);
                    json!({ "repeat": { "var": var, "in": over }, "body": body })
                }
                _ => leaf(g, scope),
            };
            items.push(item);
        }
        Value::Array(items)
    }

    fn gen_document(g: &mut Gen) -> String {
        let mut scope = Vec::new();
        let groups = gen_items(g, 0, &mut scope, &mut |g, scope| {
            let mut scope = scope.to_vec();
            let subgroups = gen_items(g, 2, &mut scope, &mut |g, scope| {
                let mut scope = scope.to_vec();
                let plots = gen_items(g, 4, &mut scope, &mut |g, scope| {
                    let title = gen_text(g, scope, false);
                    let query = gen_text(g, scope, true);
                    match g.below(4) {
                        0 => json!({ "split": { "when": gen_pred(g, 0), "plots": [
                            { "kind": "gauge", "title": title, "id": "a", "unit": "count", "query": query },
                            { "kind": "counter", "title": "b", "id": "b", "unit": "rate", "query": "q" } ] } }),
                        1 => {
                            json!({ "rate_mean": { "title": title, "id": "r", "selector": query, "mean_unit": "time" } })
                        }
                        _ => {
                            json!({ "kind": "histogram", "title": title, "id": "p", "unit": "time",
                                     "subtype": "percentiles", "query": query,
                                     "percentiles": vec![0.5; g.below(500)] })
                        }
                    }
                });
                json!({ "name": gen_text(g, &scope, false), "plots": plots })
            });
            json!({ "name": "G", "id": "g", "metadata": { "m": vec![json!({}); g.below(200)] },
                    "subgroups": subgroups })
        });
        json!({ "schema": 1, "let": { "e": "",
                                     "p": { "pred": gen_pred(g, 0) } },
                "sections": [{ "name": "T", "route": "/t", "groups": groups }] })
        .to_string()
    }

    /// Hostile documents under the parse caps: each section either expands
    /// or is refused at a limit, with what it spent within the limit plus
    /// one charge, and no single allocation above 4 times the byte limit
    /// plus 1 MiB.
    #[test]
    fn hostile_documents_expand_within_the_limits() {
        let limits = Limits {
            nodes: 1_000,
            bytes: 128 << 10,
            fuel: 512 << 10,
        };
        let ids: Vec<String> = (0..20).map(|i| i.to_string()).collect();
        let series: Vec<Vec<(&str, &str)>> = ids
            .iter()
            .map(|i| vec![("id", i.as_str()), ("k", "a\"b")])
            .collect();
        let series: Vec<&[(&str, &str)]> = series.iter().map(Vec::as_slice).collect();
        let data = Catalog::default()
            .counter("m", &series)
            .gauge("g", &series)
            .histogram("h", &series);
        let mut g = Gen {
            state: 0x5eed_5eed_5eed_5eed,
            items: 0,
        };
        let (mut expanded, mut limited) = (0, 0);
        for _ in 0..48 {
            g.items = 150;
            let text = gen_document(&mut g);
            let t = SectionTemplate::parse(&text).unwrap_or_else(|e| panic!("{e}\n{text}"));
            let SectionDef::Section(body) = &t.sections[0] else {
                unreachable!()
            };
            let budget = Budget::new(limits);
            let (result, peak) = peak_alloc(|| {
                t.expand_body(&data, body, Vec::new(), &BTreeMap::new(), &budget)
                    .map(|_| ())
            });
            match result {
                Ok(()) => expanded += 1,
                Err(e) => {
                    assert_eq!(e.kind(), TemplateErrorKind::Limit, "{e}\n{text}");
                    limited += 1;
                }
            }
            assert!(budget.nodes.get() <= limits.nodes + 1);
            // And from below: each node and each byte of output also spent
            // a unit of fuel, and so did each series read from the catalog.
            let read: usize = budget
                .memo
                .series
                .borrow()
                .values()
                .flatten()
                .flatten()
                .map(|part| part.len())
                .sum();
            assert!(budget.fuel.get() >= budget.nodes.get() + budget.bytes.get() + read);
            assert!(budget.bytes.get() <= limits.bytes + MAX_STRING_BYTES);
            assert!(budget.fuel.get() <= limits.fuel + limits.bytes + MAX_STRING_BYTES);
            assert!(
                peak <= 4 * limits.bytes + (1 << 20),
                "allocated {peak} bytes at once"
            );
        }
        // Both outcomes are exercised.
        assert!(
            expanded > 10 && limited > 10,
            "{expanded} expanded, {limited} limited"
        );
    }

    // Release-mode timing of hostile constructions, run by hand:
    // `cargo test --release -p dashboard --lib adversarial -- --ignored --nocapture`.
    fn adversarial_run(name: &str, text: String, data: &Catalog) {
        let start = std::time::Instant::now();
        let parsed = SectionTemplate::parse(&text);
        let parse_time = start.elapsed();
        let outcome = match parsed {
            Err(e) => format!(
                "parse refused: {}",
                e.message().chars().take(120).collect::<String>()
            ),
            Ok(t) => match t.expand(data, "/t", Vec::new(), &BTreeMap::new()) {
                Ok(_) => "expanded".to_string(),
                Err(e) => format!(
                    "expand refused: {}",
                    e.message().chars().take(120).collect::<String>()
                ),
            },
        };
        eprintln!(
            "ADVERSARIAL {name}: {} bytes, parse {:?}, total {:?}, {outcome}",
            text.len(),
            parse_time,
            start.elapsed()
        );
    }

    fn adversarial_doc(groups: Value, lets: Value) -> String {
        json!({ "schema": 1, "let": lets, "sections": [{ "name": "T", "route": "/t", "groups": groups }] })
            .to_string()
    }

    fn adversarial_plots(items: Value, lets: Value) -> String {
        adversarial_doc(
            json!([{ "name": "G", "id": "g", "subgroups": [{ "name": "S", "plots": items }] }]),
            lets,
        )
    }

    /// A `distinct_gt` whose `int` list has 200,000 entries, evaluated on
    /// each of 20 repeat passes.
    #[test]
    #[ignore]
    fn adversarial_distinct_gt_int() {
        let passes: Vec<String> = (0..20).map(|i| i.to_string()).collect();
        let t = adversarial_plots(
            json!([{ "repeat": { "var": "r", "in": { "values": passes } }, "body": [
                { "when": { "distinct_gt": { "metrics": ["m"], "by": ["id"], "int": vec!["id"; 200_000], "n": 1 } },
                  "then": [] } ] }]),
            json!({}),
        );
        let data = Catalog::default().counter("m", &[&[("id", "1")]]);
        adversarial_run("distinct_gt int x200000 x20", t, &data);
    }

    /// A title of 200,000 references to an empty let, on each of 1000
    /// repeat passes; and the same within the string cap (4000 references).
    #[test]
    #[ignore]
    fn adversarial_empty_references() {
        let passes: Vec<String> = (0..1000).map(|i| i.to_string()).collect();
        for refs in [200_000, 4000] {
            let t = adversarial_plots(
                json!([{ "repeat": { "var": "r", "in": { "values": passes } }, "body": [
                    { "kind": "gauge", "title": "${e}".repeat(refs), "id": "i", "unit": "count", "query": "q" } ] }]),
                json!({ "e": "" }),
            );
            adversarial_run(&format!("empty refs x{refs} x1000"), t, &Catalog::default());
        }
    }

    /// `present` on a metric of 2000 series of 200 labels, on each of
    /// 20,000 repeat passes.
    #[test]
    #[ignore]
    fn adversarial_wide_series() {
        let keys: Vec<String> = (0..200).map(|i| format!("label{i}")).collect();
        let ids: Vec<String> = (0..2000).map(|i| i.to_string()).collect();
        let series: Vec<Vec<(&str, &str)>> = ids
            .iter()
            .map(|id| keys.iter().map(|k| (k.as_str(), id.as_str())).collect())
            .collect();
        let series: Vec<&[(&str, &str)]> = series.iter().map(Vec::as_slice).collect();
        let data = Catalog::default().counter("m", &series);
        let passes: Vec<String> = (0..4000).map(|i| i.to_string()).collect();
        let inner: Vec<String> = (0..5).map(|i| i.to_string()).collect();
        let t = adversarial_plots(
            json!([{ "repeat": { "var": "r", "in": { "values": passes } }, "body": [
                { "repeat": { "var": "s", "in": { "values": inner } }, "body": [
                    { "when": { "present": "m" }, "then": [] } ] } ] }]),
            json!({}),
        );
        adversarial_run("present m (2000x200 labels) x20000", t, &data);
    }

    /// A 1 MB document: one 520,000-entry array under 30 nested `when`s.
    #[test]
    #[ignore]
    fn adversarial_deep_wide_parse() {
        let mut body = json!([{ "repeat": { "var": "v", "in": { "values": vec!["a"; 520_000] } }, "body": [] }]);
        for _ in 0..30 {
            body = json!([{ "when": { "present": "m" }, "then": body }]);
        }
        let t = adversarial_plots(body, json!({}));
        adversarial_run("520000-entry array at depth 30", t, &Catalog::default());
    }

    /// The fuel each binding costs, exactly: a pass is a node (one unit),
    /// its value's bytes, and the binding (one unit plus the name's bytes).
    #[test]
    fn bindings_cost_fuel_exactly() {
        let fuel = |t: &str| spent(t, &Catalog::default()).fuel.get();
        let rep = |values: Value| {
            plots(
                json!({}),
                json!([{ "repeat": { "var": "vv", "in": { "values": values } }, "body": [] }]),
            )
        };
        // Strings: node 1 + value 1 + the binding's lookup in a map of at
        // most one entry (1 + "vv") = 5 per pass; the first pass also
        // inserts the name (1 + "vv") and copies it (2).
        assert_eq!(
            fuel(&rep(json!(vec!["x"; 100]))) - fuel(&rep(json!([]))),
            100 * 5 + 3 + 2
        );
        // Objects of one field: node 1, then per field node 1 + value 1 +
        // lookup (1 + "vv.f") = 8 per pass; the first pass inserts and
        // copies the name (5 + 4); the field's name is formatted once per
        // repeat (2 + "vv" + "f").
        assert_eq!(
            fuel(&rep(json!(vec![json!({ "f": "x" }); 100]))) - fuel(&rep(json!([]))),
            100 * 8 + 5 + 4 + 5
        );
        // A let: 1, its text scanned (1), a lookup and an insert in a map
        // of `i` entries (each 1 + 3 bytes per level), and the name copied.
        let lets: serde_json::Map<String, Value> =
            (0..100).map(|i| (format!("l{i:02}"), json!(""))).collect();
        let want: usize = (0..100).map(|i| 1 + 1 + 2 * (1 + 3 * levels(i)) + 3).sum();
        assert_eq!(
            fuel(&plots(Value::Object(lets), json!([]))) - fuel(&plots(json!({}), json!([]))),
            want
        );
    }

    /// Sorting a `label_values` repeat's values costs n times the bits of n.
    #[test]
    fn sorting_label_values_costs_fuel() {
        let ids: Vec<String> = (0..100).map(|i| i.to_string()).collect();
        let series: Vec<Vec<(&str, &str)>> = ids.iter().map(|i| vec![("id", i.as_str())]).collect();
        let series: Vec<&[(&str, &str)]> = series.iter().map(Vec::as_slice).collect();
        let data = Catalog::default().counter("m", &series);
        let params = BTreeMap::new();
        let read = Budget::default();
        Scope::new(&data, &params, &read).series("m", true).unwrap();
        let sorted = Budget::default();
        let lv = LabelValues {
            metric: "m".into(),
            label: "id".into(),
            sort: LabelSort::Int,
        };
        label_values_sorted(&lv, &Scope::new(&data, &params, &sorted)).unwrap();
        // Beyond the read: `m` and `id` scanned and copied (6), the lookup
        // of `id` in each series' one label (100 × 3), each value's insert
        // into a set of `i` values and its bytes copied ("0" to "99", 190),
        // and the sort (two passes of 7 levels over 100 values and their
        // 190 bytes).
        let inserts: usize = (0..100usize)
            .map(|i| 1 + i.to_string().len() * levels(i))
            .sum();
        assert_eq!(
            sorted.fuel.get() - read.fuel.get(),
            6 + 100 * 3 + inserts + 190 + 2 * 7 * (100 + 190)
        );
        // A string sort is one pass.
        let str_sorted = Budget::default();
        let lv = LabelValues {
            sort: LabelSort::Str,
            ..lv
        };
        label_values_sorted(&lv, &Scope::new(&data, &params, &str_sorted)).unwrap();
        assert_eq!(sorted.fuel.get() - str_sorted.fuel.get(), 7 * (100 + 190));
    }

    /// Each group, subgroup and plot is charged its struct's size in bytes.
    #[test]
    fn output_structs_cost_bytes() {
        let bytes = |t: &str| spent(t, &Catalog::default()).bytes.get();
        let one = plots(json!({}), json!([gauge("a")]));
        let two = plots(json!({}), json!([gauge("a"), gauge("b")]));
        // The plot struct plus "b" three times (title, id, query).
        assert_eq!(bytes(&two) - bytes(&one), std::mem::size_of::<Plot>() + 3);
        let rate_mean = plots(
            json!({}),
            json!([gauge("a"), { "rate_mean": { "title": "t", "id": "i", "selector": "s", "mean_unit": "time" } }]),
        );
        assert_eq!(
            bytes(&rate_mean) - bytes(&one),
            // Two plots, "t", "i" and "s" from the template, and the copies
            // `histogram_rate_mean` makes: 3 selectors, 2 titles and ids, 64.
            2 * std::mem::size_of::<Plot>() + 3 + 3 + 2 * 2 + 64
        );
        let groups = |extra: Value| {
            let mut groups =
                vec![json!({ "name": "G", "id": "g", "subgroups": [{ "plots": [gauge("a")] }] })];
            groups.push(extra);
            section(json!({}), Value::Array(groups))
        };
        let base = section(
            json!({}),
            json!([{ "name": "G", "id": "g", "subgroups": [{ "plots": [gauge("a")] }] }]),
        );
        assert_eq!(
            bytes(&groups(json!({ "name": "H", "id": "h" }))) - bytes(&base),
            std::mem::size_of::<Group>() + 2
        );
        let with_sub = section(
            json!({}),
            json!([{ "name": "G", "id": "g", "subgroups": [{ "plots": [gauge("a")] }, { "plots": [] }] }]),
        );
        assert_eq!(
            bytes(&with_sub) - bytes(&base),
            std::mem::size_of::<SubGroup>()
        );
    }

    /// The Rezolus template on a host with 1024 CPUs: the section that
    /// spends the most fuel stays under half of [`MAX_FUEL`].
    #[test]
    fn the_rezolus_template_fits_a_1024_cpu_host() {
        let t = SectionTemplate::parse(&crate::rezolus_template::json()).unwrap();
        let data = crate::section_template_tests::bench_with_ids(1024);
        let mut most = (0, String::new());
        for section in &t.sections {
            let SectionDef::Section(body) = section else {
                continue;
            };
            let budget = Budget::default();
            t.expand_body(&data, body, Vec::new(), &BTreeMap::new(), &budget)
                .unwrap_or_else(|e| panic!("{}: {e}", body.route));
            if budget.fuel.get() > most.0 {
                most = (budget.fuel.get(), body.route.clone());
            }
        }
        eprintln!("FUEL1024 {} {}", most.1, most.0);
        assert!(most.0 < MAX_FUEL / 2, "{} spent {}", most.1, most.0);
    }

    /// A control character from the catalog counts at the six bytes JSON
    /// writes it as; in a query, `\n` and `\r` become two printable bytes.
    #[test]
    fn control_characters_count_as_json_escapes() {
        let value = "\u{1}".repeat(100);
        let data = Catalog::default().gauge("m", &[&[("k", value.as_str())]]);
        let rep = |plot: Value| {
            plots(
                json!({}),
                json!([{ "repeat": { "var": "v", "in": { "label_values": { "metric": "m", "label": "k" } } },
                         "body": [plot] }]),
            )
        };
        let bytes = |t: &str| spent(t, &data).bytes.get();
        let titled = |title: &str| {
            rep(
                json!({ "kind": "gauge", "title": title, "id": "i", "unit": "count", "query": "q" }),
            )
        };
        assert_eq!(bytes(&titled("${v}")) - bytes(&titled("")), 100 * 6);
        assert_eq!(piece_cost("a\u{1}\n", false), 3 + 2 * 5);
        assert_eq!(piece_cost("a\u{1}\n", true), 4 + 5);
        assert_eq!(json_size(&json!("\u{1}")), std::mem::size_of::<Value>() + 6);
        // Template strings cannot hold one at all.
        assert_refused(&titled("a\u{1}"), "holds the control character U+0001");
    }

    /// Tuples are counted by hash; `int` labels are normalized before
    /// hashing, so `"01"` and `"1"` are one tuple and `"1"` and `"2"` two.
    #[test]
    fn distinct_gt_tuples_hash_normalized_values() {
        let data = Catalog::default().counter(
            "m",
            &[
                &[("id", "01"), ("v", "a")],
                &[("id", "1"), ("v", "a")],
                &[("id", "2"), ("v", "a")],
            ],
        );
        let t = |int: Value, n: usize| {
            plots(
                json!({}),
                json!([{ "when": { "distinct_gt": { "metrics": ["m"], "by": ["v", "id"], "int": int, "n": n } },
                         "then": [gauge("yes")] }]),
            )
        };
        assert_eq!(ids(&expand(&t(json!(["id"]), 1), &data)), ["yes"]);
        assert!(ids(&expand(&t(json!(["id"]), 2), &data)).is_empty());
        assert_eq!(ids(&expand(&t(json!([]), 2), &data)), ["yes"]);
    }

    /// A refused plot's id is quoted at most 200 characters long.
    #[test]
    fn plot_errors_cut_the_id() {
        let mut p = gauge("p");
        p["id"] = json!("i".repeat(MAX_STRING_BYTES));
        p["kind"] = json!("latency");
        let e = parse_err(&plots(json!({}), json!([p])));
        assert!(e.len() < 600 && e.contains("takes no `unit`"), "{e}");
    }

    /// Counter names are read from the source once per expansion.
    #[test]
    fn counter_names_are_read_once() {
        let names: Vec<String> = (0..100).map(|i| format!("c{i:03}")).collect();
        let mut data = Catalog::default();
        for name in &names {
            data = data.counter(name, &[&[]]);
        }
        let n = |k: usize| {
            plots(
                json!({}),
                json!([{ "when": { "any": vec![json!({ "has_counter": "m" }); k] }, "then": [] }]),
            )
        };
        let fuel = |k: usize| spent(&n(k), &data).fuel.get();
        // Each further `has_counter`: one unit, `m` scanned and copied, and
        // a hash lookup (one plus `m` hashed and compared).
        assert_eq!(fuel(3) - fuel(2), 1 + 2 + 3);
    }

    /// The GPU selector's charges: a unit for an empty probe, one per
    /// series walked, and the bytes of each vendor copied.
    #[test]
    fn the_gpu_selector_is_charged_per_series_and_vendor() {
        let meta = |probe: Value| {
            doc(json!([{ "name": "T", "route": "/t", "metadata": {
                "s": { "computed": "gpu_selector", "probe": probe } } }]))
        };
        let none = doc(json!([{ "name": "T", "route": "/t" }]));
        let fuel = |t: &str, data: &Catalog| spent(t, data).fuel.get();
        let empty = Catalog::default();
        assert_eq!(fuel(&meta(json!([])), &empty) - fuel(&none, &empty), 1);
        // Ten series without an `id`: read (1 + "k" + "x" each), walked (1)
        // and looked up for `id` in their one label (1 + 2).
        let ten: Vec<&[(&str, &str)]> = vec![&[("k", "x")]; 10];
        let with = Catalog::default().counter("m", &ten);
        assert_eq!(
            fuel(&meta(json!(["m"])), &with) - fuel(&meta(json!(["m"])), &empty),
            10 * 3 + 10 * (1 + 3)
        );
        // Ten series of one GPU: a vendor three bytes longer is read,
        // copied and compared on its insert into a set of at most one entry
        // ten times, and the selector stays absent (one GPU).
        let short: Vec<&[(&str, &str)]> = vec![&[("id", "0"), ("vendor", "v")]; 10];
        let long: Vec<&[(&str, &str)]> = vec![&[("id", "0"), ("vendor", "vvvv")]; 10];
        let (short, long) = (
            Catalog::default().counter("m", &short),
            Catalog::default().counter("m", &long),
        );
        assert_eq!(
            fuel(&meta(json!(["m"])), &long) - fuel(&meta(json!(["m"])), &short),
            10 * 3 * 3
        );
    }

    /// The base charges of a tuple count and a first `has_counter`.
    #[test]
    fn first_reads_cost_their_base_units() {
        let data = Catalog::default()
            .counter("m", &[&[("a", "0")], &[("a", "1")], &[("a", "2")]])
            .gauge("m", &[&[("a", "0")], &[("a", "1")]]);
        let empty = Catalog::default();
        let by1 = plots(
            json!({}),
            json!([{ "when": { "distinct_gt": { "metrics": ["m"], "by": ["a"], "n": 99 } }, "then": [] }]),
        );
        // Against an absent `m`: the reads' series (5 × 3: one unit plus
        // "a" and a value), the walk (5 × (1 + 1 + "a")) and the values
        // hashed (5).
        assert_eq!(
            spent(&by1, &data).fuel.get() - spent(&by1, &empty).fuel.get(),
            15 + 15 + 5
        );
        // The first `has_counter` reads the names: 64, then one unit plus
        // the bytes of each of the 100 four-byte names.
        let names: Vec<String> = (0..100).map(|i| format!("c{i:03}")).collect();
        let mut many = Catalog::default();
        for name in &names {
            many = many.counter(name, &[&[]]);
        }
        let n = |k: usize| {
            plots(
                json!({}),
                json!([{ "when": { "any": vec![json!({ "has_counter": "m" }); k] }, "then": [] }]),
            )
        };
        // One unit for the predicate, `m` scanned and copied, the read, and
        // the lookup (one plus `m`).
        assert_eq!(
            spent(&n(1), &many).fuel.get() - spent(&n(0), &many).fuel.get(),
            1 + 2 + 64 + 100 * 5 + 3
        );
        // Label names in predicates are at most 64 bytes.
        let long = "l".repeat(MAX_NAME_BYTES + 1);
        for pred in [
            json!({ "has_label": { "metric": "m", "label": long } }),
            json!({ "distinct_gt": { "metrics": ["m"], "by": [long], "n": 1 } }),
        ] {
            assert_refused(
                &plots(json!({}), json!([{ "when": pred, "then": [] }])),
                "longer than 64 bytes",
            );
        }
        assert_refused(
            &plots(
                json!({}),
                json!([{ "repeat": { "var": "v", "in": { "label_values": { "metric": "m", "label": long } } }, "body": [] }]),
            ),
            "longer than 64 bytes",
        );
        // U+001F is the last control character refused.
        assert_refused(&plots(json!({}), json!([gauge("a\u{1f}")])), "U+001F");
        assert!(SectionTemplate::parse(&plots(json!({}), json!([gauge("a ")]))).is_ok());
        assert_eq!(piece_cost("\u{1f}", false), 6);
    }

    /// A `var` costs a lookup in the map of predicate lets: one unit plus
    /// its name's bytes per level, on top of the predicate's own unit.
    #[test]
    fn var_lookups_are_charged_per_level() {
        let lets: serde_json::Map<String, Value> = (0..100)
            .map(|i| (format!("p{i:02}"), json!({ "pred": { "present": "x" } })))
            .collect();
        let all = |k: usize| {
            let vars: Vec<Value> = (0..k)
                .map(|i| json!({ "var": format!("p{i:02}") }))
                .collect();
            plots(
                Value::Object(lets.clone()),
                json!([{ "when": { "any": vars }, "then": [] }]),
            )
        };
        let fuel = |t: &str| spent(t, &Catalog::default()).fuel.get();
        assert_eq!(
            fuel(&all(100)) - fuel(&all(0)),
            100 * (1 + 1 + 3 * levels(100))
        );
        assert_eq!(levels(100), 7);
        assert_eq!((levels(0), levels(1), levels(2), levels(4)), (1, 1, 2, 3));
    }

    /// `int` labels are found once per `distinct_gt`, each a lookup in the
    /// `int` set.
    #[test]
    fn int_labels_are_charged_once() {
        let data =
            Catalog::default().counter("m", &[&[("a", "1"), ("b", "1"), ("c", "1"), ("d", "1")]]);
        let t = |int: Value| {
            plots(
                json!({}),
                json!([{ "when": { "distinct_gt": { "metrics": ["m"], "by": ["a", "b", "c", "d"], "int": int, "n": 9 } },
                         "then": [] }]),
            )
        };
        let fuel = |t: &str| spent(t, &data).fuel.get();
        // Four `int` labels scanned and copied (4 × 2), and each `by` label's
        // lookup reads its byte three times in a set of four, not once.
        assert_eq!(
            fuel(&t(json!(["a", "b", "c", "d"]))) - fuel(&t(json!([]))),
            8 + 4 * 2
        );
    }

    /// A label name of exactly 64 bytes is accepted.
    #[test]
    fn a_64_byte_label_name_is_accepted() {
        let label = "l".repeat(MAX_NAME_BYTES);
        let t = plots(
            json!({}),
            json!([{ "when": { "has_label": { "metric": "m", "label": label } }, "then": [] }]),
        );
        assert!(SectionTemplate::parse(&t).is_ok());
    }

    /// Expand `/t` of `template` with `params` and return its fuel.
    fn fuel_with(template: &str, data: &Catalog, params: &[(&str, &str)]) -> usize {
        let t = SectionTemplate::parse(template).unwrap_or_else(|e| panic!("{e}"));
        let SectionDef::Section(body) = &t.sections[0] else {
            panic!("not a section");
        };
        let params: BTreeMap<String, String> = params
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let budget = Budget::default();
        t.expand_body(data, body, Vec::new(), &params, &budget)
            .unwrap_or_else(|e| panic!("{e}"));
        budget.fuel.get()
    }

    /// Every lookup in expansion's own maps is charged per level: the
    /// bindings and parameters a reference looks in, `param`, a pred let's
    /// insert, a `has_label` value, and the metadata maps.
    #[test]
    fn map_lookups_are_charged() {
        let empty = Catalog::default();
        let title = |refs: &str| {
            plots(
                json!({ "e": "" }),
                json!([{ "kind": "gauge", "title": format!("t{refs}"), "id": "i", "unit": "count", "query": "q" }]),
            )
        };
        // `${e}`: one unit, four bytes scanned, a lookup in a map of one
        // binding (1 + 1).
        assert_eq!(
            fuel_with(&title(&"${e}".repeat(10)), &empty, &[]) - fuel_with(&title(""), &empty, &[]),
            10 * (1 + 4 + 2)
        );
        // `${throughput}` set to `x`: one unit, 13 bytes scanned, a missed
        // lookup in the bindings (1 + 10 × 1) and one in the parameters
        // (1 + 10 × 1), and the byte of output.
        let p = [("throughput", "x")];
        assert_eq!(
            fuel_with(&title(&"${throughput}".repeat(10)), &empty, &p)
                - fuel_with(&title(""), &empty, &p),
            10 * (1 + 13 + 11 + 11 + 1)
        );
        // `param`: the predicate's unit and its lookup (1 + 10 × 1).
        let param = |k: usize| {
            plots(
                json!({}),
                json!([{ "when": { "any": vec![json!({ "param": "throughput" }); k] }, "then": [] }]),
            )
        };
        assert_eq!(
            fuel_with(&param(10), &empty, &[]) - fuel_with(&param(0), &empty, &[]),
            10 * 12
        );
        // A pred let's insert: three more bytes of name cost three bytes
        // per level of the map, and three more copied.
        let lets = |suffix: &str| {
            let lets: serde_json::Map<String, Value> = (0..100)
                .map(|i| {
                    (
                        format!("p{i:02}{suffix}"),
                        json!({ "pred": { "present": "x" } }),
                    )
                })
                .collect();
            plots(Value::Object(lets), json!([]))
        };
        let want: usize = (0..100).map(|i| 3 * levels(i) + 3).sum();
        assert_eq!(
            fuel_with(&lets("xxx"), &empty, &[]) - fuel_with(&lets(""), &empty, &[]),
            want
        );
        // A `has_label` value: two more bytes, scanned and copied (4), and
        // compared once per level of a set of four values (2 × 3).
        let data = Catalog::default().counter(
            "m",
            &[&[("k", "a")], &[("k", "b")], &[("k", "c")], &[("k", "d")]],
        );
        let has = |v: &str| {
            plots(
                json!({}),
                json!([{ "when": { "has_label": { "metric": "m", "label": "k", "value": v } }, "then": [] }]),
            )
        };
        assert_eq!(
            fuel_with(&has("abc"), &data, &[]) - fuel_with(&has("a"), &data, &[]),
            4 + 2 * 3
        );
        // Metadata keys: three more bytes hashed and compared (6), and for a
        // group's, copied into the view (3).
        let group = |k: &str| {
            section(
                json!({}),
                json!([{ "name": "G", "id": "g", "metadata": { k: 1 }, "subgroups": [{ "plots": [gauge("a")] }] }]),
            )
        };
        assert_eq!(
            fuel_with(&group("kkkk"), &empty, &[]) - fuel_with(&group("k"), &empty, &[]),
            6 + 3
        );
        let view = |k: &str| {
            doc(json!([{ "name": "T", "route": "/t", "metadata": { k: { "value": 1 } } }]))
        };
        assert_eq!(
            fuel_with(&view("kkkk"), &empty, &[]) - fuel_with(&view("k"), &empty, &[]),
            6
        );
    }

    /// The GPU selector's whole charge on four GPUs of one vendor.
    #[test]
    fn the_gpu_selector_charge_is_exact() {
        let data = Catalog::default().counter(
            "m",
            &[
                &[("id", "0"), ("vendor", "v")],
                &[("id", "1"), ("vendor", "v")],
                &[("id", "2"), ("vendor", "v")],
                &[("id", "3"), ("vendor", "v")],
            ],
        );
        let params = BTreeMap::new();
        let budget = Budget::default();
        let scope = Scope::new(&data, &params, &budget);
        assert!(scope.gpu_selector(&["m".to_string()]).unwrap().is_some());
        // The probe's unit; the counter and gauge lookups (3 each) and
        // reads (64 + 1, plus 4 × 11 for the counters); the walk (4); per
        // series the `id` and `vendor` lookups in two labels (5 and 13) and
        // the vendor copied (1); the inserts into `seen` (9 bytes per
        // level of a set of 0, 1, 2 and 3 entries); and the sort of four
        // ids (4 × 3).
        let want = 1
            + (3 + 64 + 1 + 4 * 11)
            + (3 + 64 + 1)
            + 4
            + 4 * (5 + 13 + 1)
            + (0..4).map(|i| 1 + 9 * levels(i)).sum::<usize>()
            + 4 * 3;
        assert_eq!(budget.fuel.get(), want);
    }
}
