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
//!   "let": { "multi_cpu": { "pred": { "distinct_gt": { "metrics": ["cpu_usage"], "by": ["id"], "n": 1 } } } },
//!   "sections": [
//!     { "builtin": "query" },
//!     { "name": "CPU", "route": "/cpu", "groups": [ ... ] }
//!   ]
//! }
//! ```
//!
//! A list of groups, subgroups or plots holds elements and three kinds of
//! node, which expand into elements:
//!
//! - `{"when": P, "then": [..], "else": [..]}`: `then` when predicate `P`
//!   holds, else `else` (optional).
//! - `{"repeat": {"var": "op", "in": V}, "body": [..]}`: `body` once per
//!   value of `V`, with `${op}` bound to it. `V` is `{"values": [..]}`, a
//!   list of strings or of objects (whose fields bind as `${op.field}`), or
//!   `{"label_values": {"metric": .., "label": .., "sort": "str"|"int"}}`.
//! - In a plot list, `{"split": {"when": P, "plots": [A, B]}}`: `A` and `B`
//!   side by side when `P` holds, else `A` alone at full width.
//!
//! Strings, including the metric and label names in predicates, take
//! `${name}` and `${name.field}` from repeats, `let` values and viewer
//! parameters (`${throughput}`). A `let` cannot refer to another `let`.
//!
//! A plot is built with the same constructors and options the generators
//! use (`kind`: `gauge`, `counter`, `histogram`, or `latency` for
//! [`PlotOpts::histogram_latency`]), so a template that makes the same calls
//! renders the same JSON. `{"rate_mean": {..}}` in a plot list is
//! [`SubGroup::histogram_rate_mean`].
//!
//! A group or subgroup left with nothing in it after expansion is omitted,
//! unless it says `"keep_empty": true`.
//!
//! # Versioning
//!
//! `schema` versions the language. An expander handles every schema up to
//! [`MAX_SCHEMA`]. A document with a larger schema, or an unknown field or
//! node anywhere, is refused whole, and the caller uses another template for
//! that source.

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;
use serde_json::Value;

use crate::MetricsSource;
use crate::plot::*;

/// The highest schema this expander handles.
pub const MAX_SCHEMA: u32 = 1;

/// A section template document.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SectionTemplate {
    pub schema: u32,
    /// Informational: which producer build the template describes.
    #[serde(default)]
    pub revision: Option<String>,
    /// Values every section can use.
    #[serde(default, rename = "let")]
    pub lets: BTreeMap<String, LetDef>,
    pub sections: Vec<SectionDef>,
}

/// A section, or a library section placed by name.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum SectionDef {
    /// `{"builtin": "query"}`: the query explorer.
    Builtin {
        builtin: String,
    },
    Section(Box<SectionBody>),
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SectionBody {
    pub name: String,
    pub route: String,
    #[serde(default, rename = "let")]
    pub lets: BTreeMap<String, LetDef>,
    /// The view's metadata: a static JSON value, or a computed one.
    #[serde(default)]
    pub metadata: BTreeMap<String, MetaDef>,
    #[serde(default = "Vec::new")]
    pub groups: Vec<Item<GroupDef>>,
}

/// A named value: a string, the first metric present from a list, or a
/// predicate.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum LetDef {
    Text(String),
    FirstPresent { first_present: FirstPresent },
    Pred { pred: Pred },
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FirstPresent {
    pub list: Vec<String>,
    pub default: String,
}

/// A view metadata value.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum MetaDef {
    /// `{"computed": "gpu_selector", "probe": [..]}`: the GPU selector over
    /// the probe metrics, present when they name more than one GPU.
    Computed {
        computed: String,
        #[serde(default)]
        probe: Vec<String>,
    },
    Static(Value),
}

/// A predicate over the metric catalog.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Pred {
    /// The metric exists as a counter, gauge or histogram.
    Present(String),
    /// The metric exists as a counter.
    HasCounter(String),
    /// Some series of `metric` has `label`, equal to `value` when given.
    HasLabel {
        metric: String,
        label: String,
        #[serde(default)]
        value: Option<String>,
    },
    /// More than `n` distinct values of the `by` labels. With `each`, any one
    /// metric's values of the first `by` label count on their own (as
    /// `metric_unique_label_count`); without it, label tuples are counted
    /// across the counter and gauge series of every metric, skipping a
    /// series missing one of them or whose `int` labels do not parse as
    /// integers.
    DistinctGt {
        metrics: Vec<String>,
        by: Vec<String>,
        n: usize,
        #[serde(default)]
        each: bool,
        #[serde(default)]
        int: Vec<String>,
    },
    Any(Vec<Pred>),
    All(Vec<Pred>),
    Not(Box<Pred>),
    /// A `let` predicate by name.
    Var(String),
    /// The viewer parameter is set.
    Param(String),
}

/// An element of a list, or a node that expands into elements.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum Item<T> {
    When {
        when: Pred,
        then: Vec<Item<T>>,
        #[serde(default = "Vec::new", rename = "else")]
        otherwise: Vec<Item<T>>,
    },
    Repeat {
        repeat: Repeat,
        body: Vec<Item<T>>,
    },
    Leaf(T),
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Repeat {
    pub var: String,
    #[serde(rename = "in")]
    pub over: RepeatOver,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum RepeatOver {
    Values(Vec<Value>),
    LabelValues {
        metric: String,
        label: String,
        #[serde(default)]
        sort: LabelSort,
    },
}

#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LabelSort {
    #[default]
    Str,
    Int,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GroupDef {
    pub name: String,
    pub id: String,
    #[serde(default)]
    pub metadata: BTreeMap<String, Value>,
    #[serde(default = "Vec::new")]
    pub subgroups: Vec<Item<SubGroupDef>>,
    #[serde(default)]
    pub keep_empty: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubGroupDef {
    /// Absent for an unnamed subgroup.
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default = "Vec::new")]
    pub plots: Vec<Item<PlotItem>>,
    #[serde(default)]
    pub keep_empty: bool,
}

/// A plot, a side-by-side pair, or a rate and mean card pair.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum PlotItem {
    Split { split: Box<SplitDef> },
    RateMean { rate_mean: RateMeanDef },
    Plot(Box<PlotDef>),
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SplitDef {
    pub when: Pred,
    pub plots: [PlotDef; 2],
}

/// [`SubGroup::histogram_rate_mean`]'s arguments.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RateMeanDef {
    pub title: String,
    pub id: String,
    pub selector: String,
    /// A rate query over a separate counter; absent to derive the rate from
    /// the histogram.
    #[serde(default)]
    pub rate: Option<String>,
    /// `time` or `bytes`.
    pub mean_unit: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlotDef {
    /// `gauge`, `counter`, `histogram` or `latency`.
    pub kind: String,
    pub title: String,
    pub id: String,
    /// The unit: required except for `latency`.
    #[serde(default)]
    pub unit: Option<String>,
    /// A histogram's subtype: `percentiles` or `buckets`.
    #[serde(default)]
    pub subtype: Option<String>,
    pub query: String,
    /// `full` for a full-width plot.
    #[serde(default)]
    pub width: Option<String>,
    #[serde(default)]
    pub percentage_range: bool,
    #[serde(default)]
    pub range: Option<[f64; 2]>,
    #[serde(default)]
    pub unit_system: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub percentiles: Option<Vec<f64>>,
    #[serde(default)]
    pub axis_label: Option<String>,
    #[serde(default)]
    pub row_label: Option<String>,
    #[serde(default)]
    pub style: Option<String>,
    #[serde(default)]
    pub log_scale: Option<bool>,
    /// The query the Mean/Total toggle swaps to.
    #[serde(default)]
    pub query_total: Option<String>,
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

impl SectionTemplate {
    /// Parse and check a template: its schema, and every node it holds.
    pub fn parse(json: &str) -> Result<Self, TemplateError> {
        let template: Self = serde_json::from_str(json)
            .map_err(|e| TemplateError(format!("not a section template: {e}")))?;
        if template.schema == 0 || template.schema > MAX_SCHEMA {
            return err(format!(
                "section template schema {} is not one this viewer expands (1 to {MAX_SCHEMA})",
                template.schema
            ));
        }
        for section in &template.sections {
            if let SectionDef::Builtin { builtin } = section
                && builtin != "query"
            {
                return err(format!("unknown builtin section {builtin:?}"));
            }
        }
        Ok(template)
    }

    /// The sections, in order, as the navigation lists them. The query
    /// explorer is `("Query Explorer", "/query")`.
    pub fn sections(&self) -> Vec<Section> {
        self.sections
            .iter()
            .map(|s| match s {
                SectionDef::Builtin { .. } => Section {
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
    /// a route the template has not.
    pub fn expand(
        &self,
        data: &dyn MetricsSource,
        route: &str,
        sections: Vec<Section>,
        params: &BTreeMap<String, String>,
    ) -> Result<Option<View>, TemplateError> {
        for section in &self.sections {
            match section {
                SectionDef::Builtin { .. } if route == "/query" => {
                    return Ok(Some(View::new(data, sections)));
                }
                SectionDef::Section(body) if body.route == route => {
                    let mut scope = Scope::new(data, params);
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
                    return Ok(Some(view));
                }
                _ => {}
            }
        }
        Ok(None)
    }
}

/// What strings and predicates are evaluated in: the catalog, the viewer
/// parameters, `let` values and repeat bindings.
struct Scope<'a> {
    data: &'a dyn MetricsSource,
    params: &'a BTreeMap<String, String>,
    values: BTreeMap<String, String>,
    preds: BTreeMap<String, bool>,
}

impl<'a> Scope<'a> {
    fn new(data: &'a dyn MetricsSource, params: &'a BTreeMap<String, String>) -> Self {
        Self {
            data,
            params,
            values: BTreeMap::new(),
            preds: BTreeMap::new(),
        }
    }

    fn bind_lets(&mut self, lets: &BTreeMap<String, LetDef>) -> Result<(), TemplateError> {
        for (name, def) in lets {
            match def {
                LetDef::Text(text) => {
                    let text = self.subst(text)?;
                    self.values.insert(name.clone(), text);
                }
                LetDef::FirstPresent { first_present } => {
                    let chosen = first_present
                        .list
                        .iter()
                        .find(|m| has_metric(self.data, m))
                        .unwrap_or(&first_present.default)
                        .clone();
                    self.values.insert(name.clone(), chosen);
                }
                LetDef::Pred { pred } => {
                    let holds = self.holds(pred)?;
                    self.preds.insert(name.clone(), holds);
                }
            }
        }
        Ok(())
    }

    fn holds(&self, pred: &Pred) -> Result<bool, TemplateError> {
        let data = self.data;
        Ok(match pred {
            Pred::Present(metric) => has_metric(data, &self.subst(metric)?),
            Pred::HasCounter(metric) => data.has_counter(&self.subst(metric)?),
            Pred::HasLabel {
                metric,
                label,
                value,
            } => {
                let values = data.label_values(&self.subst(metric)?, &self.subst(label)?);
                match value {
                    Some(v) => values.contains(&self.subst(v)?),
                    None => !values.is_empty(),
                }
            }
            Pred::DistinctGt {
                metrics,
                by,
                n,
                each,
                int,
            } => {
                if *each {
                    let Some(key) = by.first() else {
                        return err("distinct_gt with `each` needs a `by` label");
                    };
                    for metric in metrics {
                        if metric_unique_label_count(data, &self.subst(metric)?, key) > *n {
                            return Ok(true);
                        }
                    }
                    false
                } else {
                    let mut seen: BTreeSet<Vec<Option<String>>> = BTreeSet::new();
                    for metric in metrics {
                        let metric = self.subst(metric)?;
                        for labels in data
                            .counter_labels(&metric)
                            .into_iter()
                            .chain(data.gauge_labels(&metric))
                        {
                            let mut tuple = Vec::with_capacity(by.len());
                            let mut keep = true;
                            for key in by {
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

    /// `text` with every `${name}` and `${name.field}` replaced.
    fn subst(&self, text: &str) -> Result<String, TemplateError> {
        let mut out = String::with_capacity(text.len());
        let mut rest = text;
        while let Some(start) = rest.find("${") {
            out.push_str(&rest[..start]);
            let after = &rest[start + 2..];
            let Some(end) = after.find('}') else {
                return err(format!("unclosed `${{` in {text:?}"));
            };
            let name = &after[..end];
            let value = self
                .values
                .get(name)
                .or_else(|| self.params.get(name))
                .ok_or_else(|| TemplateError(format!("unknown name {name:?} in {text:?}")))?;
            out.push_str(value);
            rest = &after[end + 1..];
        }
        out.push_str(rest);
        Ok(out)
    }

    fn subst_opt(&self, text: &Option<String>) -> Result<Option<String>, TemplateError> {
        text.as_deref().map(|t| self.subst(t)).transpose()
    }

    fn meta(&self, meta: &MetaDef) -> Result<Option<Value>, TemplateError> {
        match meta {
            MetaDef::Static(value) => Ok(Some(value.clone())),
            MetaDef::Computed { computed, probe } => match computed.as_str() {
                "gpu_selector" => Ok(gpu_selector(self.data, probe)),
                other => err(format!("unknown computed metadata {other:?}")),
            },
        }
    }

    fn group(&mut self, def: &GroupDef) -> Result<Option<Group>, TemplateError> {
        let mut group = Group::new(self.subst(&def.name)?, self.subst(&def.id)?);
        for (key, value) in &def.metadata {
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
            if !sub.keep_empty {
                group.drop_last_subgroup_if_empty();
            }
            Ok(())
        })?;
        if group.is_empty() && !def.keep_empty {
            return Ok(None);
        }
        Ok(Some(group))
    }

    fn plot_item(&mut self, sg: &mut SubGroup, item: &PlotItem) -> Result<(), TemplateError> {
        match item {
            PlotItem::Plot(def) => self.plot(sg, def, None),
            PlotItem::Split { split } => {
                if self.holds(&split.when)? {
                    self.plot(sg, &split.plots[0], Some(PlotWidth::Half))?;
                    self.plot(sg, &split.plots[1], Some(PlotWidth::Half))
                } else {
                    self.plot(sg, &split.plots[0], Some(PlotWidth::Full))
                }
            }
            PlotItem::RateMean { rate_mean } => {
                let mean_unit = match rate_mean.mean_unit.as_str() {
                    "time" => Unit::Time,
                    "bytes" => Unit::Bytes,
                    other => return err(format!("rate_mean mean_unit {other:?}: time or bytes")),
                };
                let rate = match &rate_mean.rate {
                    Some(q) => RateSource::Counter(self.subst(q)?),
                    None => RateSource::FromHistogram,
                };
                sg.histogram_rate_mean(
                    &self.subst(&rate_mean.title)?,
                    &self.subst(&rate_mean.id)?,
                    &self.subst(&rate_mean.selector)?,
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
        let unit = || -> Result<Unit, TemplateError> {
            match def.unit.as_deref() {
                Some(u) => unit_of(u),
                None => err(format!("plot {:?} needs a unit", def.id)),
            }
        };
        let mut opts = match def.kind.as_str() {
            "gauge" => PlotOpts::gauge(title, id, unit()?),
            "counter" => PlotOpts::counter(title, id, unit()?),
            "histogram" => {
                let Some(subtype) = def.subtype.as_deref() else {
                    return err(format!("histogram plot {:?} needs a subtype", def.id));
                };
                PlotOpts::histogram(title, id, unit()?, subtype)
            }
            "latency" => PlotOpts::histogram_latency(title, id),
            other => return err(format!("unknown plot kind {other:?}")),
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
        let query = self.subst(&def.query)?;
        let full = match (width, def.width.as_deref()) {
            (Some(w), _) => w == PlotWidth::Full,
            (None, Some("full")) => true,
            (None, None) => false,
            (None, Some(other)) => return err(format!("plot width {other:?}: only \"full\"")),
        };
        if full {
            sg.plot_promql_full(opts, query);
        } else {
            sg.plot_promql(opts, query);
        }
        if let Some(total) = self.subst_opt(&def.query_total)?
            && let Some(plot) = sg.plots_mut_last()
        {
            plot.promql_query_total = Some(total);
        }
        Ok(())
    }
}

fn unit_of(name: &str) -> Result<Unit, TemplateError> {
    Ok(match name {
        "count" => Unit::Count,
        "rate" => Unit::Rate,
        "time" => Unit::Time,
        "bytes" => Unit::Bytes,
        "datarate" => Unit::Datarate,
        "bitrate" => Unit::Bitrate,
        "percentage" => Unit::Percentage,
        "frequency" => Unit::Frequency,
        "power" => Unit::Power,
        other => return err(format!("unknown unit {other:?}")),
    })
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
        match item {
            Item::Leaf(leaf) => visit(leaf, scope)?,
            Item::When {
                when,
                then,
                otherwise,
            } => {
                let branch = if scope.holds(when)? { then } else { otherwise };
                walk(branch, scope, visit)?;
            }
            Item::Repeat { repeat, body } => {
                for binding in repeat_values(&repeat.over, scope)? {
                    let saved = scope.values.clone();
                    match &binding {
                        Value::Object(fields) => {
                            for (field, value) in fields {
                                let Some(text) = value.as_str() else {
                                    return err(format!("repeat field {field:?} is not a string"));
                                };
                                scope
                                    .values
                                    .insert(format!("{}.{field}", repeat.var), text.to_string());
                            }
                        }
                        Value::String(text) => {
                            scope.values.insert(repeat.var.clone(), text.clone());
                        }
                        other => {
                            return err(format!("repeat value {other} is not a string or object"));
                        }
                    }
                    let result = walk(body, scope, visit);
                    scope.values = saved;
                    result?;
                }
            }
        }
    }
    Ok(())
}

fn repeat_values(over: &RepeatOver, scope: &Scope<'_>) -> Result<Vec<Value>, TemplateError> {
    Ok(match over {
        RepeatOver::Values(values) => values.clone(),
        RepeatOver::LabelValues {
            metric,
            label,
            sort,
        } => {
            let mut values: Vec<String> = scope
                .data
                .label_values(&scope.subst(metric)?, &scope.subst(label)?)
                .into_iter()
                .collect();
            match sort {
                LabelSort::Str => values.sort(),
                LabelSort::Int => values.sort_by_key(|v| v.parse::<i64>().unwrap_or(i64::MAX)),
            }
            values.into_iter().map(Value::String).collect()
        }
    })
}

/// The GPU selector's metadata: the distinct `(vendor, id)` pairs among the
/// probe metrics' counter and gauge series, when there is more than one.
fn gpu_selector(data: &dyn MetricsSource, probe: &[String]) -> Option<Value> {
    let mut seen: BTreeSet<(Option<String>, i64)> = BTreeSet::new();
    for metric in probe {
        for labels in data
            .counter_labels(metric)
            .into_iter()
            .chain(data.gauge_labels(metric))
        {
            let Some(id) = labels.get("id").and_then(|v| v.parse::<i64>().ok()) else {
                continue;
            };
            seen.insert((labels.get("vendor").cloned(), id));
        }
    }
    if seen.len() <= 1 {
        return None;
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
    Some(serde_json::json!({ "enabled": true, "ids": ids, "gpus": gpus }))
}
