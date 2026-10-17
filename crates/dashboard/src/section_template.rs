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
//!   cannot reuse a name already in scope.
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
//!   every metric, with the labels listed in `int` compared as integers
//!   (`"01"` and `"1"` are one value), skipping a series that lacks an
//!   `int` label or whose `int` label does not parse; a missing non-`int`
//!   label counts as absent.
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
//! `query_total`, or a `rate_mean`'s `selector` or `rate`, a reference to
//! one must sit inside a double-quoted string (`{k="${v}"}`), and `parse`
//! refuses one anywhere else; there its `\`, `"` and newlines are escaped
//! as in a PromQL string literal. In other fields, and for every other
//! binding and parameter, the value is inserted as given.
//!
//! # Limits
//!
//! Expanding one section, its lets included:
//!
//! - visits at most [`MAX_NODES`] nodes, counting elements, `when` and
//!   `repeat` nodes, and repeat passes;
//! - produces at most [`MAX_TEXT_BYTES`] bytes of strings, metadata and
//!   percentiles, checked before each piece of a string is appended;
//! - does at most [`MAX_WORK`] units of catalog work: one per predicate
//!   evaluated, plus one per series that a predicate, a `label_values`
//!   repeat or the GPU selector reads, and one per counter name a
//!   `has_counter` scans.
//!
//! A section that would exceed any of these is refused with an error; the
//! other sections are unaffected. `parse` reports the first problem it
//! finds, not all of them.
//!
//! # Versioning
//!
//! `schema` versions the language. An expander handles every schema up to
//! [`MAX_SCHEMA`]. [`SectionTemplate::parse`] reads `schema` before
//! anything else, so a document with a larger schema is refused as such
//! whatever else it holds. A document with an unknown field or node
//! anywhere, or that fails any other check `parse` makes in any branch, is
//! refused whole, and the caller uses another template for that source.

use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};

use serde::de::{DeserializeOwned, Error as _};
use serde::{Deserialize, Deserializer};
use serde_json::Value;

use crate::MetricsSource;
use crate::plot::*;

/// The highest schema this expander handles.
pub const MAX_SCHEMA: u32 = 1;

/// The most nodes one section's expansion visits.
pub const MAX_NODES: usize = 20_000;

/// The most bytes of strings and metadata one section's expansion produces.
pub const MAX_TEXT_BYTES: usize = 4 << 20;

/// The most catalog work one section's expansion does: one unit per
/// predicate evaluated plus one per series a predicate, `label_values`
/// repeat or computed metadata reads (and per counter name `has_counter`
/// scans).
pub const MAX_WORK: usize = 2_000_000;

/// The most sections a document holds.
pub const MAX_SECTIONS: usize = 64;

/// The longest section name or route, in bytes.
pub const MAX_SECTION_NAME_BYTES: usize = 64;

/// A parsed and checked section template document.
#[derive(Debug, Clone)]
pub struct SectionTemplate {
    schema: u32,
    revision: Option<String>,
    params: BTreeSet<String>,
    lets: BTreeMap<String, LetDef>,
    sections: Vec<SectionDef>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    #[allow(dead_code)]
    schema: u32,
    #[serde(default)]
    revision: Option<String>,
    #[serde(default)]
    params: Vec<String>,
    #[serde(default, rename = "let")]
    lets: BTreeMap<String, LetDef>,
    /// Parsed one at a time, so an error names the section.
    sections: Vec<Value>,
}

#[derive(Debug, Clone)]
enum SectionDef {
    /// `{"builtin": "query"}`: the query explorer.
    Builtin,
    Section(Box<SectionBody>),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BuiltinDef {
    #[allow(dead_code)]
    builtin: Builtin,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum Builtin {
    Query,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct SectionBody {
    name: String,
    route: String,
    #[serde(default, rename = "let")]
    lets: BTreeMap<String, LetDef>,
    #[serde(default)]
    metadata: BTreeMap<String, MetaDef>,
    #[serde(default)]
    groups: Vec<Item<GroupDef>>,
}

#[derive(Debug, Clone)]
enum LetDef {
    Text(String),
    FirstPresent(FirstPresent),
    Pred(Pred),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FirstPresentLet {
    #[serde(deserialize_with = "object")]
    first_present: FirstPresent,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct FirstPresent {
    list: Vec<String>,
    default: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PredLet {
    pred: Pred,
}

#[derive(Debug, Clone)]
enum MetaDef {
    Static(Value),
    GpuSelector(Vec<String>),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StaticMeta {
    value: Value,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ComputedMeta {
    computed: Computed,
    probe: Vec<String>,
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

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, bound = "T: DeserializeOwned")]
struct WhenNode<T> {
    when: Pred,
    then: Vec<Item<T>>,
    #[serde(default, rename = "else")]
    otherwise: Vec<Item<T>>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, bound = "T: DeserializeOwned")]
struct RepeatNode<T> {
    #[serde(deserialize_with = "object")]
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

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct GroupDef {
    name: String,
    id: String,
    #[serde(default)]
    metadata: BTreeMap<String, Value>,
    #[serde(default)]
    subgroups: Vec<Item<SubGroupDef>>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct SubGroupDef {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    plots: Vec<Item<PlotItem>>,
}

#[derive(Debug, Clone)]
enum PlotItem {
    Split(Box<SplitDef>),
    RateMean(RateMeanDef),
    Plot(Box<PlotDef>),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SplitNode {
    #[serde(deserialize_with = "object")]
    split: Box<SplitDef>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct SplitDef {
    when: Pred,
    #[serde(deserialize_with = "two_plots")]
    plots: [PlotDef; 2],
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RateMeanNode {
    #[serde(deserialize_with = "object")]
    rate_mean: RateMeanDef,
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

// The nodes below are told apart by the key that names them (`when`,
// `repeat`, `split`, ...), then read as a struct that refuses unknown
// fields. An untagged enum would try each variant in turn, accept a
// misspelled field into whichever variant ignores it, and report a real
// error only as "did not match any variant".
//
// Every struct is read through `read` or `object`, which refuse anything
// but a JSON object: serde's derived struct impls also accept a JSON array
// of the fields in order, which would let `["gauge", "T", "id", ...]` be a
// plot.

fn has_key(value: &Value, key: &str) -> bool {
    value.get(key).is_some()
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

/// A JSON value whose objects have no repeated key. `serde_json::Value`
/// keeps the last of a repeated key and drops the others silently.
struct UniqueKeys(Value);

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
            fn visit_str<E>(self, v: &str) -> Result<UniqueKeys, E> {
                Ok(UniqueKeys(Value::String(v.to_string())))
            }
            fn visit_string<E>(self, v: String) -> Result<UniqueKeys, E> {
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
                    let UniqueKeys(value) = map.next_value()?;
                    if out.contains_key(&key) {
                        return Err(A::Error::custom(format!("key {key:?} is repeated")));
                    }
                    out.insert(key, value);
                }
                Ok(UniqueKeys(Value::Object(out)))
            }
        }
        d.deserialize_any(Visit)
    }
}

impl<'de> Deserialize<'de> for SectionDef {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(d)?;
        if has_key(&value, "builtin") {
            read::<BuiltinDef, _>(value)?;
            Ok(Self::Builtin)
        } else {
            Ok(Self::Section(read(value)?))
        }
    }
}

impl<'de> Deserialize<'de> for LetDef {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(d)?;
        if let Value::String(text) = value {
            Ok(Self::Text(text))
        } else if has_key(&value, "first_present") {
            Ok(Self::FirstPresent(
                read::<FirstPresentLet, _>(value)?.first_present,
            ))
        } else if has_key(&value, "pred") {
            Ok(Self::Pred(read::<PredLet, _>(value)?.pred))
        } else {
            Err(D::Error::custom(
                "a let is a string, {\"first_present\": ..} or {\"pred\": ..}",
            ))
        }
    }
}

impl<'de> Deserialize<'de> for MetaDef {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(d)?;
        if has_key(&value, "computed") {
            let ComputedMeta { computed, probe } = read(value)?;
            match computed {
                Computed::GpuSelector => Ok(Self::GpuSelector(probe)),
            }
        } else if has_key(&value, "value") {
            Ok(Self::Static(read::<StaticMeta, _>(value)?.value))
        } else {
            Err(D::Error::custom(
                "section metadata is {\"value\": ..} or {\"computed\": ..}",
            ))
        }
    }
}

impl<'de, T: DeserializeOwned> Deserialize<'de> for Item<T> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(d)?;
        if has_key(&value, "when") {
            Ok(Self::When(read(value)?))
        } else if has_key(&value, "repeat") {
            Ok(Self::Repeat(read(value)?))
        } else {
            Ok(Self::Leaf(read(value)?))
        }
    }
}

impl<'de> Deserialize<'de> for PlotItem {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(d)?;
        if has_key(&value, "split") {
            Ok(Self::Split(read::<SplitNode, _>(value)?.split))
        } else if has_key(&value, "rate_mean") {
            Ok(Self::RateMean(read::<RateMeanNode, _>(value)?.rate_mean))
        } else {
            Ok(Self::Plot(read(value)?))
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
                            "repeat value field {field:?} is not a string"
                        )));
                    };
                    fields.insert(field, text);
                }
                Ok(Self::Fields(fields))
            }
            other => Err(D::Error::custom(format!(
                "repeat value {other} is not a string or an object of strings"
            ))),
        }
    }
}

/// Why a template was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemplateError(pub String);

impl std::fmt::Display for TemplateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for TemplateError {}

fn err<T>(msg: impl Into<String>) -> Result<T, TemplateError> {
    Err(TemplateError(msg.into()))
}

/// How a section is named in an error: its index and route.
fn section_label(index: usize, value: &Value) -> String {
    if let Some(route) = value.get("route").and_then(Value::as_str) {
        format!("section {index} ({route:?})")
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
            .map_err(|e| TemplateError(format!("not a section template: {e}")))?;
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
                TemplateError(format!(
                    "section template schema {schema} is not an integer"
                ))
            })?,
        };
        if schema == 0 || schema > u64::from(MAX_SCHEMA) {
            return err(format!(
                "section template schema {schema} is not one this viewer expands (1 to {MAX_SCHEMA})"
            ));
        }
        let doc: Document = read::<_, serde_json::Error>(value)
            .map_err(|e| TemplateError(format!("not a section template: {e}")))?;
        if doc.sections.len() > MAX_SECTIONS {
            return err(format!(
                "{} sections: at most {MAX_SECTIONS}",
                doc.sections.len()
            ));
        }
        let mut sections = Vec::with_capacity(doc.sections.len());
        for (index, value) in doc.sections.into_iter().enumerate() {
            let label = section_label(index, &value);
            let section = SectionDef::deserialize(value)
                .map_err(|e| TemplateError(format!("{label}: {e}")))?;
            sections.push(section);
        }
        let mut params = BTreeSet::new();
        for param in doc.params {
            if !is_name(&param) {
                return err(format!("parameter name {param:?} is not a name"));
            }
            if !params.insert(param.clone()) {
                return err(format!("parameter {param:?} is declared twice"));
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
                        .map_err(|e| TemplateError(format!("{route}: {e}")));
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
                        .map_err(|e| TemplateError(format!("section {index}: {e}")))?;
                    &body.route
                }
            };
            if !routes.insert(route) {
                return err(format!("two sections have route {route:?}"));
            }
        }
        let mut names = Names::new(&self.params);
        names
            .lets(&self.lets)
            .map_err(|e| TemplateError(format!("document let: {e}")))?;
        for (index, section) in self.sections.iter().enumerate() {
            let SectionDef::Section(body) = section else {
                continue;
            };
            let mut names = names.clone();
            names
                .lets(&body.lets)
                .and_then(|()| names.section(body))
                .map_err(|e| TemplateError(format!("section {index} ({:?}): {e}", body.route)))?;
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
            "section name {name:?} is not 1 to {MAX_SECTION_NAME_BYTES} bytes"
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
            "route {route:?} is not `/` and segments of a-z, 0-9, `_` and `-`, at most {MAX_SECTION_NAME_BYTES} bytes"
        ));
    }
    Ok(())
}

/// `name` is a let, parameter, repeat variable or field name: ASCII
/// letters, digits and `_`, not starting with a digit.
fn is_name(name: &str) -> bool {
    !name.is_empty()
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
            return err(format!("unclosed `${{` in {text:?}"));
        };
        let name = &after[..end];
        let well_formed = match name.split_once('.') {
            Some((var, field)) => is_name(var) && is_name(field),
            None => is_name(name),
        };
        if !well_formed {
            return err(format!("`${{{name}}}` in {text:?} is not a name"));
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
                return err(format!("let name {name:?} is not a name"));
            }
            if self.in_scope(name) {
                return err(format!("let {name:?} reuses a name already in scope"));
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
            checked.map_err(|e| TemplateError(format!("let {name:?}: {e}")))?;
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
                        "`${{{name}}}` in {text:?} names a predicate; use {{\"var\": {name:?}}}"
                    ));
                }
                Some(NameKind::Record) => {
                    return err(format!(
                        "`${{{name}}}` in {text:?} names a repeat over objects; use `${{{name}.field}}`"
                    ));
                }
                None if self.params.contains(name) => {}
                None => return err(format!("unknown name `${{{name}}}` in {text:?}")),
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
                        "`${{{name}}}` in {text:?} is a label value, and goes only inside a double-quoted string there"
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
                if *each && (by.len() != 1 || !int.is_empty()) {
                    return err("distinct_gt with `each` takes one `by` label and no `int`");
                }
                if by.is_empty() {
                    return err("distinct_gt needs a `by` label");
                }
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
                    err(format!("{{\"var\": {name:?}}} names a value"))
                }
                None => err(format!("unknown predicate {name:?}")),
            },
            Pred::Param(name) if self.params.contains(name) => Ok(()),
            Pred::Param(name) => err(format!("parameter {name:?} is not declared in `params`")),
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
            return err(format!("repeat variable {var:?} is not a name"));
        }
        if self.in_scope(var) {
            return err(format!(
                "repeat variable {var:?} reuses a name already in scope"
            ));
        }
        let bound = match &repeat.over {
            RepeatOver::LabelValues(lv) => {
                self.text(&lv.metric)?;
                self.text(&lv.label)?;
                self.names.insert(var.clone(), NameKind::Catalog);
                return Ok(vec![var.clone()]);
            }
            RepeatOver::Values(values) => match values.first() {
                None => Vec::new(),
                Some(RepeatValue::Text(_)) => {
                    if !values.iter().all(|v| matches!(v, RepeatValue::Text(_))) {
                        return err(format!("repeat {var:?} mixes strings and objects"));
                    }
                    vec![var.clone()]
                }
                Some(RepeatValue::Fields(first)) => {
                    for value in values {
                        let RepeatValue::Fields(fields) = value else {
                            return err(format!("repeat {var:?} mixes strings and objects"));
                        };
                        if !fields.keys().eq(first.keys()) {
                            return err(format!(
                                "repeat {var:?}: every value needs the same fields"
                            ));
                        }
                    }
                    let mut bound = Vec::new();
                    for field in first.keys() {
                        if !is_name(field) {
                            return err(format!("repeat {var:?}: field {field:?} is not a name"));
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
        let fail = |msg: &str| err(format!("plot {:?}: {msg}", def.id));
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

/// What one section's expansion has spent of each limit.
#[derive(Default)]
struct Budget {
    nodes: Cell<usize>,
    bytes: Cell<usize>,
    work: Cell<usize>,
}

impl Budget {
    fn spend(cell: &Cell<usize>, n: usize, max: usize, what: &str) -> Result<(), TemplateError> {
        let total = cell.get().saturating_add(n);
        cell.set(total);
        if total > max {
            return err(format!("expands to more than {max} {what}"));
        }
        Ok(())
    }

    /// Whether `n` more bytes fit, without spending them.
    fn bytes_fit(&self, n: usize) -> Result<(), TemplateError> {
        if self.bytes.get().saturating_add(n) > MAX_TEXT_BYTES {
            return err(format!("expands to more than {MAX_TEXT_BYTES} bytes"));
        }
        Ok(())
    }
}

type Labels = BTreeMap<String, String>;

/// What strings and predicates are evaluated in: the catalog, the viewer
/// parameters, `let` values and repeat bindings, and the expansion budget.
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

    fn spend_node(&self) -> Result<(), TemplateError> {
        Budget::spend(&self.budget.nodes, 1, MAX_NODES, "nodes")
    }

    fn spend_bytes(&self, len: usize) -> Result<(), TemplateError> {
        Budget::spend(&self.budget.bytes, len, MAX_TEXT_BYTES, "bytes")
    }

    fn spend_work(&self, n: usize) -> Result<(), TemplateError> {
        Budget::spend(&self.budget.work, n, MAX_WORK, "units of catalog work")
    }

    /// `metric`'s counter and gauge series, and its histogram series when
    /// `histograms`, charged to the work budget one unit per series.
    fn series(&self, metric: &str, histograms: bool) -> Result<Vec<Labels>, TemplateError> {
        let mut out = self.data.counter_labels(metric);
        out.extend(self.data.gauge_labels(metric));
        if histograms {
            out.extend(self.data.histogram_labels(metric));
        }
        self.spend_work(out.len())?;
        Ok(out)
    }

    /// The values of `label` on `metric`'s series of every type, as
    /// [`MetricsSource::label_values`] gives them.
    fn label_values(&self, metric: &str, label: &str) -> Result<BTreeSet<String>, TemplateError> {
        Ok(self
            .series(metric, true)?
            .into_iter()
            .filter_map(|mut labels| labels.remove(label))
            .collect())
    }

    /// [`has_metric`], charged.
    fn present(&self, metric: &str) -> Result<bool, TemplateError> {
        Ok(!self.series(metric, true)?.is_empty())
    }

    /// Bind each let, evaluated in a scope holding only the parameters and
    /// spending this scope's budget.
    fn bind_lets(&mut self, lets: &BTreeMap<String, LetDef>) -> Result<(), TemplateError> {
        let base = Scope::new(self.data, self.params, self.budget);
        for (name, def) in lets {
            match def {
                LetDef::Text(text) => {
                    let text = base.subst(text)?;
                    self.bind(name, text, false);
                }
                LetDef::FirstPresent(fp) => {
                    let mut chosen = None;
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
                    self.bind(name, chosen, false);
                }
                LetDef::Pred(pred) => {
                    let holds = base.holds(pred)?;
                    self.preds.insert(name.clone(), holds);
                }
            }
        }
        Ok(())
    }

    fn bind(&mut self, name: &str, text: String, from_catalog: bool) {
        self.values
            .insert(name.to_string(), Bound { text, from_catalog });
    }

    /// Whether `pred` holds: one unit of work, plus one per series read.
    fn holds(&self, pred: &Pred) -> Result<bool, TemplateError> {
        self.spend_work(1)?;
        Ok(match pred {
            Pred::Present(metric) => self.present(&self.subst(metric)?)?,
            Pred::HasCounter(metric) => {
                let metric = self.subst(metric)?;
                let names = self.data.counter_names();
                self.spend_work(names.len())?;
                names.contains(&metric)
            }
            Pred::HasLabel(HasLabel {
                metric,
                label,
                value,
            }) => {
                let values = self.label_values(&self.subst(metric)?, &self.subst(label)?)?;
                match value {
                    Some(v) => values.contains(&self.subst(v)?),
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
                    for metric in metrics {
                        if self.label_values(&self.subst(metric)?, key)?.len() > *n {
                            return Ok(true);
                        }
                    }
                    false
                } else {
                    let mut seen: BTreeSet<Vec<Option<String>>> = BTreeSet::new();
                    for metric in metrics {
                        for labels in self.series(&self.subst(metric)?, false)? {
                            let mut tuple = Vec::with_capacity(by.len());
                            let mut keep = true;
                            for key in &by {
                                let value = labels.get(key).cloned();
                                if int.contains(key) {
                                    match value.as_deref().map(str::parse::<i64>) {
                                        Some(Ok(i)) => tuple.push(Some(i.to_string())),
                                        _ => {
                                            keep = false;
                                            break;
                                        }
                                    }
                                } else {
                                    tuple.push(value);
                                }
                            }
                            if keep {
                                seen.insert(tuple);
                            }
                        }
                    }
                    seen.len() > *n
                }
            }
            Pred::Any(preds) => {
                for p in preds {
                    if self.holds(p)? {
                        return Ok(true);
                    }
                }
                false
            }
            Pred::All(preds) => {
                for p in preds {
                    if !self.holds(p)? {
                        return Ok(false);
                    }
                }
                true
            }
            Pred::Not(p) => !self.holds(p)?,
            Pred::Var(name) => match self.preds.get(name) {
                Some(holds) => *holds,
                None => return err(format!("unknown predicate {name:?}")),
            },
            Pred::Param(name) => self.params.contains_key(name),
        })
    }

    fn subst(&self, text: &str) -> Result<String, TemplateError> {
        self.subst_to(text, Dest::Text)
    }

    fn subst_promql(&self, text: &str) -> Result<String, TemplateError> {
        self.subst_to(text, Dest::Promql)
    }

    /// `text` with every `${name}` and `${name.field}` replaced, counted
    /// against the byte budget. Each piece is checked against the budget
    /// before it is appended, so a string that would exceed it is refused
    /// before it is built.
    fn subst_to(&self, text: &str, dest: Dest) -> Result<String, TemplateError> {
        let mut out = String::new();
        let mut rest = text;
        let push = |out: &mut String, piece: &str, escape: bool| -> Result<(), TemplateError> {
            let len = if escape {
                promql_escaped_len(piece)
            } else {
                piece.len()
            };
            self.budget.bytes_fit(out.len().saturating_add(len))?;
            if escape {
                push_promql_escaped(out, piece);
            } else {
                out.push_str(piece);
            }
            Ok(())
        };
        while let Some(start) = rest.find("${") {
            push(&mut out, &rest[..start], false)?;
            let after = &rest[start + 2..];
            let Some(end) = after.find('}') else {
                return err(format!("unclosed `${{` in {text:?}"));
            };
            let name = &after[..end];
            if let Some(bound) = self.values.get(name) {
                let escape = bound.from_catalog && dest == Dest::Promql;
                push(&mut out, &bound.text, escape)?;
            } else if let Some(value) = self.params.get(name) {
                push(&mut out, value, false)?;
            } else {
                return err(format!("unknown or unset name {name:?} in {text:?}"));
            }
            rest = &after[end + 1..];
        }
        push(&mut out, rest, false)?;
        self.spend_bytes(out.len())?;
        Ok(out)
    }

    fn subst_opt(&self, text: &Option<String>) -> Result<Option<String>, TemplateError> {
        text.as_deref().map(|t| self.subst(t)).transpose()
    }

    fn meta(&self, meta: &MetaDef) -> Result<Option<Value>, TemplateError> {
        match meta {
            MetaDef::Static(value) => {
                self.spend_bytes(json_size(value))?;
                Ok(Some(value.clone()))
            }
            MetaDef::GpuSelector(probe) => {
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
        for metric in probe {
            for labels in self.series(metric, false)? {
                let Some(id) = labels.get("id").and_then(|v| v.parse::<i64>().ok()) else {
                    continue;
                };
                seen.insert((labels.get("vendor").cloned(), id));
            }
        }
        if seen.len() <= 1 {
            return Ok(None);
        }
        let mut ids: Vec<i64> = seen.iter().map(|(_, id)| *id).collect();
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
        let mut group = Group::new(self.subst(&def.name)?, self.subst(&def.id)?);
        for (key, value) in &def.metadata {
            self.spend_bytes(key.len() + json_size(value))?;
            group.metadata.insert(key.clone(), value.clone());
        }
        walk(&def.subgroups, self, &mut |sub, scope| {
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
                sg.histogram_rate_mean(
                    &self.subst(&rm.title)?,
                    &self.subst(&rm.id)?,
                    &self.subst_promql(&rm.selector)?,
                    rate,
                    mean_unit,
                );
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
            c => out.push(c),
        }
    }
}

/// The length of `text` once [`push_promql_escaped`] has escaped it.
fn promql_escaped_len(text: &str) -> usize {
    text.len()
        + text
            .chars()
            .filter(|c| matches!(c, '\\' | '"' | '\n'))
            .count()
}

/// An estimate of `value`'s size as JSON, for the byte budget.
fn json_size(value: &Value) -> usize {
    match value {
        Value::Null | Value::Bool(_) | Value::Number(_) => 8,
        Value::String(s) => s.len() + 2,
        Value::Array(items) => items.iter().map(json_size).sum::<usize>() + 2,
        Value::Object(map) => map.iter().map(|(k, v)| k.len() + 3 + json_size(v)).sum(),
    }
}

type Visit<'v, T> = dyn FnMut(&T, &mut Scope<'_>) -> Result<(), TemplateError> + 'v;

/// Visit the elements `items` expands to, in order, each with the scope it
/// was produced in: `when` picks a branch, and `repeat` visits its body once
/// per value with the variable bound for that pass only.
fn walk<T>(
    items: &[Item<T>],
    scope: &mut Scope<'_>,
    visit: &mut Visit<'_, T>,
) -> Result<(), TemplateError> {
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
            Item::Repeat(node) => {
                let var = &node.repeat.var;
                let (values, from_catalog) = repeat_values(&node.repeat.over, scope)?;
                for value in values {
                    scope.spend_node()?;
                    // A pass's bindings are not removed after it: `check`
                    // refuses a reference to a repeat variable outside its
                    // body and a variable that shadows another name, so a
                    // stale binding is never read, and the next pass or
                    // repeat that binds the name overwrites it.
                    match value {
                        RepeatValue::Text(text) => scope.bind(var, text, from_catalog),
                        RepeatValue::Fields(fields) => {
                            for (field, text) in fields {
                                scope.bind(&format!("{var}.{field}"), text, from_catalog);
                            }
                        }
                    }
                    walk(&node.body, scope, visit)?;
                }
            }
        }
    }
    Ok(())
}

/// A repeat's values, and whether they came from the catalog.
fn repeat_values(
    over: &RepeatOver,
    scope: &Scope<'_>,
) -> Result<(Vec<RepeatValue>, bool), TemplateError> {
    Ok(match over {
        RepeatOver::Values(values) => (values.clone(), false),
        RepeatOver::LabelValues(lv) => {
            let mut values: Vec<String> = scope
                .label_values(&scope.subst(&lv.metric)?, &scope.subst(&lv.label)?)?
                .into_iter()
                .collect();
            sort_label_values(&mut values, lv.sort);
            (values.into_iter().map(RepeatValue::Text).collect(), true)
        }
    })
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
        SectionTemplate::parse(template).unwrap_err().0
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
        assert!(e.0.contains("unset name \"throughput\""), "{e}");
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
                "unknown field `otherwise`",
            ),
            (
                doc(json!([{ "builtin": "query", "name": "Y", "route": "/x" }])),
                "unknown field `name`",
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
                "unknown field `x`",
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
                "unknown field `x`",
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
                "unknown field `x`",
            ),
            (
                plots(
                    json!({}),
                    json!([{ "split": { "when": { "present": "a" }, "plots": [gauge("a"), gauge("b")] }, "x": 1 }]),
                ),
                "unknown field `x`",
            ),
            (
                plots(
                    json!({}),
                    json!([{ "rate_mean": { "title": "t", "id": "i", "selector": "s", "mean_unit": "time" }, "x": 1 }]),
                ),
                "unknown field `x`",
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
                "unknown field `keep_empty`",
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
        assert_refused(&t, "section 1 (\"/b\"): unknown field `x`");
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
                "needs a `by` label",
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
        assert!(e.contains("unknown field `newfield`"), "{e}");
        assert!(
            parse_err(
                &doc(json!([])).replace("\"params\":[\"throughput\"]", "\"params\":[\"a\",\"a\"]")
            )
            .contains("declared twice")
        );
    }

    /// Five nested repeats of ten would be 100,000 plots.
    #[test]
    fn expansion_is_bounded_by_nodes() {
        let ten: Vec<String> = (0..10).map(|i| i.to_string()).collect();
        let mut body = json!([gauge("p")]);
        for var in ["a", "b", "c", "d", "e"] {
            body = json!([{ "repeat": { "var": var, "in": { "values": ten } }, "body": body }]);
        }
        let t = SectionTemplate::parse(&plots(json!({}), body)).unwrap();
        let e = t
            .expand(&Catalog::default(), "/t", Vec::new(), &BTreeMap::new())
            .err()
            .unwrap();
        assert_eq!(e.0, format!("/t: expands to more than {MAX_NODES} nodes"));

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
        assert!(e.0.contains("nodes"), "{e}");
    }

    #[test]
    fn expansion_is_bounded_by_bytes() {
        let big = "x".repeat(MAX_TEXT_BYTES / 4 + 1);
        let t = plots(
            json!({}),
            json!([{ "repeat": { "var": "v", "in": { "values": ["1", "2", "3", "4"] } },
                     "body": [{ "kind": "gauge", "title": big, "id": "i", "unit": "count", "query": "q" }] }]),
        );
        let t = SectionTemplate::parse(&t).unwrap();
        let e = t
            .expand(&Catalog::default(), "/t", Vec::new(), &BTreeMap::new())
            .err()
            .unwrap();
        assert_eq!(
            e.0,
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
            assert!(budget.work.get() < MAX_WORK / 10, "{}", body.route);
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

    fn expand_err(template: &str, data: &Catalog, params: &[(&str, &str)]) -> String {
        let t = SectionTemplate::parse(template).unwrap_or_else(|e| panic!("{e}"));
        let params = params
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        t.expand(data, "/t", Vec::new(), &params).err().unwrap().0
    }

    /// The largest allocation this test binary's threads request while
    /// [`peak_alloc`] is measuring them.
    struct PeakAlloc;

    thread_local! {
        static MEASURING: Cell<bool> = const { Cell::new(false) };
        static PEAK: Cell<usize> = const { Cell::new(0) };
    }

    fn note(size: usize) {
        let _ = MEASURING.try_with(|on| {
            if on.get() {
                let _ = PEAK.try_with(|p| p.set(p.get().max(size)));
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
        PEAK.with(|p| p.set(0));
        MEASURING.with(|on| on.set(true));
        let out = f();
        MEASURING.with(|on| on.set(false));
        (out, PEAK.with(Cell::get))
    }

    /// A string that would be far over the byte budget is refused before
    /// it is built, whatever its pieces are bound from: a let, a parameter,
    /// a `values` field or a catalog value. Each would be about 2 GB; no
    /// allocation during the refused expansion reaches 4 times the budget.
    #[test]
    fn a_string_over_the_byte_budget_is_refused_before_it_is_built() {
        let big = "x".repeat(100_000);
        let refs = |name: &str| format!("${{{name}}}").repeat(20_000);
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
            let (e, peak) =
                peak_alloc(|| t.expand(&data, "/t", Vec::new(), &params).err().unwrap().0);
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
                .0
        });
        assert_eq!(
            e,
            format!("/t: expands to more than {MAX_TEXT_BYTES} bytes")
        );
        // The label value itself is copied at half the budget; the escaped
        // copy, at the whole budget, is never started.
        assert!(peak < MAX_TEXT_BYTES, "allocated {peak} bytes at once");
    }

    /// The work each predicate, repeat and computed value is charged: one
    /// per predicate, one per series (or counter name) it reads.
    #[test]
    fn catalog_work_is_charged_per_series() {
        // 3 counter, 2 gauge and 1 histogram series of `m`; 2 counter names.
        let data = Catalog::default()
            .counter("m", &[&[("id", "0")], &[("id", "1")], &[("id", "2")]])
            .counter("other", &[&[]])
            .gauge("m", &[&[("id", "0")], &[("id", "1")]])
            .histogram("m", &[&[("id", "0")]]);
        let when = |pred: Value| plots(json!({}), json!([{ "when": pred, "then": [] }]));
        let cases = [
            (when(json!({ "present": "m" })), 1 + 6),
            (when(json!({ "has_counter": "m" })), 1 + 2),
            (
                when(json!({ "has_label": { "metric": "m", "label": "id" } })),
                1 + 6,
            ),
            (
                when(json!({ "distinct_gt": { "metrics": ["m", "m"], "by": ["id"], "n": 99 } })),
                1 + 5 + 5,
            ),
            (
                when(
                    json!({ "distinct_gt": { "metrics": ["m", "m"], "by": ["id"], "n": 99, "each": true } }),
                ),
                1 + 6 + 6,
            ),
            (when(json!({ "not": { "all": [{ "any": [] }] } })), 3),
            (
                plots(json!({ "p": { "pred": { "present": "m" } } }), json!([])),
                1 + 6,
            ),
            (
                plots(
                    json!({ "f": { "first_present": { "list": ["x", "m"], "default": "d" } } }),
                    json!([]),
                ),
                6,
            ),
            (
                plots(
                    json!({}),
                    json!([{ "repeat": { "var": "v", "in": { "label_values": { "metric": "m", "label": "id" } } },
                             "body": [] }]),
                ),
                6,
            ),
            (
                doc(json!([{ "name": "T", "route": "/t", "metadata": {
                    "s": { "computed": "gpu_selector", "probe": ["m", "m"] } } }])),
                5 + 5,
            ),
            (
                plots(
                    json!({}),
                    json!([{ "split": { "when": { "present": "m" }, "plots": [gauge("a"), gauge("b")] } }]),
                ),
                1 + 6,
            ),
        ];
        for (template, work) in cases {
            assert_eq!(spent(&template, &data).work.get(), work, "{template}");
        }
    }

    /// The reviewer's case: a 5000-series metric listed 200 times in a
    /// `distinct_gt`, evaluated on each of 20 repeat passes, is 20 million
    /// series reads. It is refused at the work limit.
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
        let start = std::time::Instant::now();
        let e = expand_err(&t, &data, &[]);
        assert_eq!(
            e,
            format!("/t: expands to more than {MAX_WORK} units of catalog work")
        );
        assert!(start.elapsed() < std::time::Duration::from_secs(10));
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
            json!({ "distinct_gt": { "metrics": ["m"], "by": ["id"], "int": [bad], "n": 1 } }),
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
}
