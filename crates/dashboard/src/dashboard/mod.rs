use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use crate::MetricsSource;
use crate::combined::{self, Member, Scope, Scoped};
use crate::plot::*;
use crate::section_template::{SectionTemplate, TemplateError};
use crate::service_extension::{CategoryExtension, ServiceExtension, TemplateRegistry};
use crate::source_templates::TemplatePreference;

mod blockio;
mod category;
mod cgroups;
mod cpu;
mod exceptions;
mod ext4;
mod filesystem;
mod gpu;
mod hw_sensors;
mod memory;
mod network;
mod overview;
mod query_explorer;
mod rezolus;
mod scheduler;
mod service;
mod softirq;
mod syscall;
mod xfs;

type Generator = fn(&dyn MetricsSource, Vec<Section>) -> View;

static SECTION_META: &[(&str, &str, Generator)] = &[
    // Exceptions sits after Overview — it's a cross-sampler triage view,
    // not a per-sampler section.
    ("Exceptions", "/exceptions", exceptions::generate),
    ("Query Explorer", "/query", query_explorer::generate),
    ("CPU", "/cpu", cpu::generate),
    ("GPU", "/gpu", gpu::generate),
    ("Hardware Sensors", "/hw_sensors", hw_sensors::generate),
    ("Memory", "/memory", memory::generate),
    ("Network", "/network", network::generate),
    ("Scheduler", "/scheduler", scheduler::generate),
    ("Syscall", "/syscall", syscall::generate),
    ("Softirq", "/softirq", softirq::generate),
    ("BlockIO", "/blockio", blockio::generate),
    ("Filesystem", "/filesystem", filesystem::generate),
    ("ext4", "/ext4", ext4::generate),
    ("XFS", "/xfs", xfs::generate),
    ("cgroups", "/cgroups", cgroups::generate),
    ("Rezolus", "/rezolus", rezolus::generate),
];

/// Passive descriptor handed down by the viewer. The dashboard crate does not
/// classify — it renders what it is told.
pub struct SourceEntry {
    pub name: String,
    pub is_rezolus: bool,
}

/// Owned context produced by `build_dashboard_context`. Carries
/// everything `generate_section` needs to render any single section on
/// demand without re-deriving the dedup / category-fallback logic.
#[derive(Default, Clone)]
pub struct DashboardContext {
    /// Navigation list including overview, stock sections, and any
    /// service / category sections — same order as what the eager
    /// generator produced.
    pub sections: Vec<Section>,
    pub filesize: Option<u64>,
    /// Deduped (per `service_name`) and category-aware. The category
    /// flow stores the same list as the per-member flow today; the
    /// difference is whether `category` below is set.
    pub service_exts: Vec<(String, ServiceExtension)>,
    pub category: Option<(String, CategoryExtension)>,
    pub throughput_query: Option<String>,
    /// The section template the built-in sections render from, set by
    /// [`build_template_context`]. `None` in a context from
    /// [`build_dashboard_context`], whose built-in sections are the Rust
    /// generators'.
    pub template: Option<Arc<SectionTemplate>>,
    /// The routes that render for one recording of a combined source
    /// ([`build_combined_context`]), each with that recording's scope and
    /// the template its built-in sections expand. Empty otherwise.
    pub scopes: HashMap<String, Arc<RouteScope>>,
}

/// What a route of a combined source renders against.
pub struct RouteScope {
    /// The recording's sources, matchers, header and metadata.
    pub scope: Scope,
    /// The template the route's section expands, for a built-in section.
    /// `None` for a service section.
    pub template: Option<Arc<SectionTemplate>>,
    /// The viewer parameter `throughput` the section expands with; see
    /// [`build_combined_context`].
    pub throughput: Option<String>,
    /// The uuids of the sources of the recording the throughput query is
    /// a service of: a plot whose query includes it reads those sources
    /// besides its own recording's.
    pub throughput_sources: Vec<String>,
    /// For a recording shown beside a comparison ([`add_beside_sections`]):
    /// its key and its own source, which the section is expanded against,
    /// its queries carrying `__capture__="<key>"` for
    /// [`combined::Routed`] to send them to that source.
    pub capture: Option<(String, Arc<dyn MetricsSource>)>,
}

/// Routes the frontend renders itself (`app.js`'s `/about` route and its
/// client-only `/:section` views), which a template section must not take.
pub const CLIENT_ROUTES: &[&str] = &[
    "/about",
    "/systeminfo",
    "/metadata",
    "/notebook",
    "/selection",
    "/report",
];

/// Whether a template section at `route` is left out of the navigation and
/// never rendered: a route the frontend renders itself ([`CLIENT_ROUTES`]),
/// or one of more than one segment. The frontend routes a section as
/// `/:section`, so a deeper route is unreachable (`/a/chart/b` is the
/// single-chart route), and `/service/` and `/source/` belong to the service,
/// category and source sections.
pub fn is_reserved_route(route: &str) -> bool {
    let Some(rest) = route.strip_prefix('/') else {
        return true;
    };
    rest.is_empty() || rest.contains('/') || CLIENT_ROUTES.contains(&route)
}

/// Build the navigation list and other shared state needed to render
/// dashboard sections lazily. The dedup + category-fallback logic lives
/// here exclusively; `generate_section` consumes the resulting context.
///
/// `sources` lists every non-service source present in the recording.
/// Empty slice → legacy behavior (all built-ins shown). Non-empty with no
/// Rezolus entry → built-ins suppressed (simple-capture case).
pub fn build_dashboard_context(
    filesize: Option<u64>,
    service_exts: &[(&str, &ServiceExtension)],
    category: Option<(&str, &CategoryExtension)>,
    sources: &[SourceEntry],
) -> DashboardContext {
    let mut builtins = vec![Section {
        name: "Overview".to_string(),
        route: "/overview".to_string(),
        instance: None,
        landing: false,
    }];
    builtins.extend(SECTION_META.iter().map(|(name, route, _)| Section {
        name: (*name).to_string(),
        route: (*route).to_string(),
        instance: None,
        landing: false,
    }));
    build_context(builtins, None, filesize, service_exts, category, sources)
}

/// [`build_dashboard_context`], with the built-in sections listed and
/// rendered by `template` rather than by the Rust generators. The service,
/// category and source sections are the same in both.
///
/// Service and category sections are placed after the template's first
/// section, which in the Rezolus template is the Overview, as they are
/// placed after the generators' Overview.
pub fn build_template_context(
    template: Arc<SectionTemplate>,
    filesize: Option<u64>,
    service_exts: &[(&str, &ServiceExtension)],
    category: Option<(&str, &CategoryExtension)>,
    sources: &[SourceEntry],
) -> DashboardContext {
    let builtins: Vec<Section> = template
        .sections()
        .into_iter()
        .filter(|s| !is_reserved_route(&s.route))
        .collect();
    build_context(
        builtins,
        Some(template),
        filesize,
        service_exts,
        category,
        sources,
    )
}

fn build_context(
    builtins: Vec<Section>,
    template: Option<Arc<SectionTemplate>>,
    filesize: Option<u64>,
    service_exts: &[(&str, &ServiceExtension)],
    category: Option<(&str, &CategoryExtension)>,
    sources: &[SourceEntry],
) -> DashboardContext {
    // Two captures of the same service collapse into a single nav entry —
    // both render through the same template and the existing compare-mode
    // overlay handles the per-capture pairing. Without this dedup the nav
    // shows the same route twice and the rendered map double-generates
    // (then HashMap-collapses) the same section.
    let mut seen = std::collections::HashSet::new();
    let unique_service_exts: Vec<(&str, &ServiceExtension)> = service_exts
        .iter()
        .copied()
        .filter(|(name, _)| seen.insert(*name))
        .collect();

    // A category requires exactly two distinct member service exts. If a
    // caller passes Some(category) without that, the category can't be
    // rendered; fall back to per-member sections so the section list and
    // the rendered output stay in agreement (no nav entry for a route the
    // generator can't produce, no orphaned member sections).
    let category_active = category.is_some() && unique_service_exts.len() == 2;

    // Empty sources = legacy caller; any Rezolus entry = show built-ins.
    let show_builtins = sources.is_empty() || sources.iter().any(|s| s.is_rezolus);

    let mut sections: Vec<Section> = Vec::new();

    if show_builtins {
        sections.extend(builtins);
    }
    // Service/category sections go after the first built-in (the Overview),
    // before source: entries.
    let insert_pos = sections.len().min(1);

    if category_active {
        let (category_name, _) = category.unwrap();
        sections.insert(
            insert_pos,
            Section {
                name: category_name.to_string(),
                route: format!("/service/{category_name}"),
                instance: None,
                landing: false,
            },
        );
    } else {
        for (i, (source_name, _)) in unique_service_exts.iter().enumerate() {
            sections.insert(
                insert_pos + i,
                Section {
                    name: source_name.to_string(),
                    route: format!("/service/{source_name}"),
                    instance: None,
                    landing: false,
                },
            );
        }
    }

    // One nav entry per non-Rezolus (Simple) source.
    for s in sources.iter().filter(|s| !s.is_rezolus) {
        sections.push(Section {
            name: format!("source: {}", s.name),
            route: format!("/source/{}", s.name),
            instance: None,
            landing: false,
        });
    }

    let throughput_query = unique_service_exts
        .first()
        .and_then(|(_, e)| e.throughput_query())
        .map(str::to_string);

    let owned_service_exts: Vec<(String, ServiceExtension)> = unique_service_exts
        .iter()
        .map(|(name, ext)| ((*name).to_string(), (*ext).clone()))
        .collect();

    let owned_category = if category_active {
        category.map(|(name, ext)| (name.to_string(), ext.clone()))
    } else {
        None
    };

    DashboardContext {
        sections,
        filesize,
        service_exts: owned_service_exts,
        category: owned_category,
        throughput_query,
        template,
        scopes: HashMap::new(),
    }
}

/// Run each KPI's query against `data` and mark the KPIs that return
/// nothing unavailable, so their charts are listed as such rather than drawn
/// empty. What both viewers do for a capture's service templates.
pub fn validate_service_extensions(
    data: &dyn MetricsSource,
    exts: &mut [(String, ServiceExtension)],
) {
    use metriken_query::QueryResult;
    let (start, end) = data.time_range().unwrap_or((0.0, 0.0));
    for (_source, ext) in exts.iter_mut() {
        for kpi in &mut ext.kpis {
            let query = kpi.effective_query();
            kpi.available = match data.query_range(&query, start, end, 1.0) {
                Ok(QueryResult::Vector { result, .. }) => !result.is_empty(),
                Ok(QueryResult::Matrix { result, .. }) => !result.is_empty(),
                Ok(QueryResult::Scalar { .. }) => true,
                Ok(QueryResult::HistogramHeatmap { result, .. }) => !result.data.is_empty(),
                Err(_) => false,
            };
        }
    }
}

/// The section context of a combined source: `data` reads every recording
/// of an archive, and `members` describes each one ([`Member`]). Both viewers
/// call this for an archive that opens combined ([`combined::layout`]) with
/// more than one recording.
///
/// Each recording is classified and its templates resolved from its own
/// metadata, as a single recording's are, and every template is
/// instantiated once per recording that carries it:
///
/// - The built-in sections render for each recording classified as a
///   Rezolus agent, from that recording's section template. The first
///   instance has the template's routes; each further one has them followed
///   by `@` and a key from its name (`/cpu@web-02`), without the query
///   explorer, which renders once. With two or more instances, every entry
///   names its instance ([`Section::instance`]).
/// - A service template renders for each recording whose metadata (or the
///   registry, by its source) gives it one: `/service/<name>`, then
///   `/service/<name>@<key>` for a second recording of the same service.
/// - A recording with neither is a simple capture's `/source/<name>` entry.
///
/// Every route but the query explorer and `/source/` renders against
/// [`Scoped`]: the recording's catalog, its own header (interval, source,
/// version and time range), and each plot's [`Plot::scope`] set to the
/// uuids of the recording's sources, which are also under
/// [`combined::SCOPE_KEY`] in the view's metadata. A recording without
/// source uuids ([`combined::source_uuids`]) has no scope; its queries are
/// restricted by [`combined::restrict_query`] instead.
///
/// An instance's Overview shows the throughput of a service of its own
/// recording, else of a service recorded from the same `host`, a plot that
/// includes it reading that service's sources as well. An instance with
/// neither has none, rather than another host's, and so does one whose
/// throughput would come from another recording when either recording has
/// no source uuids.
/// `DashboardContext::throughput_query` is the first instance's (or, with
/// no built-in instance, the first service's).
///
/// `catalog`, when the shell has the archive `data` reads, answers each
/// recording's names and label sets without opening other recordings'
/// tables ([`combined::ScopeCatalog`]); it must read what `data` reads.
///
/// Returns the context and what the template lookups had to say.
pub fn build_combined_context(
    data: &dyn MetricsSource,
    catalog: Option<Arc<dyn combined::ScopeCatalog>>,
    members: &[Member],
    registry: &TemplateRegistry,
    preference: TemplatePreference,
    filename_stem: Option<&str>,
    filesize: Option<u64>,
) -> (DashboardContext, Vec<String>) {
    let series_labels: Vec<BTreeMap<String, String>> =
        members.iter().map(|m| m.series_labels.clone()).collect();
    let matchers = combined::scope_matchers(&series_labels);
    let mut notices = Vec::new();

    struct Part {
        scope: Scope,
        template: Option<Arc<SectionTemplate>>,
        service_exts: Vec<(String, ServiceExtension)>,
        simple: Vec<SourceEntry>,
    }
    let parts: Vec<Part> = members
        .iter()
        .zip(matchers)
        .map(|(member, matchers)| {
            let scope = Scope {
                sources: member.sources.clone(),
                matchers,
                header: member.header.clone(),
                metadata: member.metadata.clone(),
                catalog: catalog.clone(),
            };
            let scoped = Scoped::new(data, &scope);
            let resolved = crate::source_templates::resolve_source_templates(
                &member.metadata,
                registry,
                preference,
            );
            notices.extend(resolved.notices);
            let mut service_exts = resolved.service_exts;
            validate_service_extensions(&scoped, &mut service_exts);
            let service_names: HashSet<&str> =
                service_exts.iter().map(|(s, _)| s.as_str()).collect();
            let mut metric_names = scoped.counter_names();
            metric_names.extend(scoped.gauge_names());
            metric_names.extend(scoped.histogram_names());
            let mut kv = serde_json::Map::new();
            for (k, v) in &member.metadata {
                let json = serde_json::from_str(v)
                    .unwrap_or_else(|_| serde_json::Value::String(v.clone()));
                kv.insert(k.clone(), json);
            }
            let sources = crate::source_kind::classify_sources(
                &serde_json::Value::Object(kv),
                &metric_names,
                &service_names,
                filename_stem,
            );
            let template = sources.iter().any(|s| s.is_rezolus).then(|| {
                let chosen = crate::source_templates::resolve_dashboard_template(
                    &member.metadata,
                    preference,
                );
                notices.extend(chosen.notice);
                chosen.template
            });
            let simple = sources.into_iter().filter(|s| !s.is_rezolus).collect();
            drop(service_names);
            Part {
                scope,
                template,
                service_exts,
                simple,
            }
        })
        .collect();

    let mut sections: Vec<Section> = Vec::new();
    let mut scopes: HashMap<String, Arc<RouteScope>> = HashMap::new();

    // Built-in sections, one instance per Rezolus recording.
    let builtin: Vec<usize> = (0..parts.len())
        .filter(|&i| parts[i].template.is_some())
        .collect();
    // Each service's throughput query, with the recording it reads.
    let throughputs: Vec<(usize, String)> = parts
        .iter()
        .enumerate()
        .flat_map(|(i, part)| {
            part.service_exts
                .iter()
                .filter_map(move |(_, ext)| ext.throughput_query().map(|q| (i, q.to_string())))
        })
        .collect();
    // An instance's throughput: a service of its own recording's, else of a
    // recording on its host, else none; with that recording's sources. None
    // for a service of another recording when either recording has no
    // source uuids, since its query could not be scoped to the service; a
    // service of the instance's own recording is read as the instance is.
    let host = |i: usize| series_labels[i].get("host");
    let throughput_for = |i: usize| -> Option<(String, Vec<String>)> {
        throughputs
            .iter()
            .find(|(j, _)| *j == i)
            .or_else(|| host(i).and_then(|h| throughputs.iter().find(|(j, _)| host(*j) == Some(h))))
            .filter(|(j, _)| {
                *j == i
                    || (!parts[i].scope.sources.is_empty() && !parts[*j].scope.sources.is_empty())
            })
            .map(|(j, q)| (q.clone(), parts[*j].scope.sources.clone()))
    };
    let (names, keys) = instance_naming(&builtin, &series_labels);
    for (n, &i) in builtin.iter().enumerate() {
        let template = parts[i].template.clone().expect("filtered on a template");
        let (throughput, throughput_sources) = throughput_for(i).unzip();
        let scope = Arc::new(RouteScope {
            scope: parts[i].scope.clone(),
            template: Some(Arc::clone(&template)),
            throughput,
            throughput_sources: throughput_sources.unwrap_or_default(),
            capture: None,
        });
        for mut section in template.sections() {
            if is_reserved_route(&section.route) || (n > 0 && section.route == "/query") {
                continue;
            }
            if n > 0 {
                section.route = format!(
                    "{}{}{}",
                    section.route,
                    combined::INSTANCE_SEPARATOR,
                    keys[n]
                );
            }
            // The query explorer reads the whole source, once.
            if section.route != "/query" {
                if builtin.len() > 1 {
                    section.instance = Some(names[n].clone());
                }
                scopes.insert(section.route.clone(), Arc::clone(&scope));
            }
            sections.push(section);
        }
    }

    // Service sections, after the first built-in (the Overview).
    let insert_pos = sections.len().min(1);
    let mut service_exts: Vec<(String, ServiceExtension)> = Vec::new();
    let mut service_sections: Vec<Section> = Vec::new();
    let mut by_service: Vec<(String, Vec<(usize, ServiceExtension)>)> = Vec::new();
    for (i, part) in parts.iter().enumerate() {
        for (name, ext) in &part.service_exts {
            match by_service.iter_mut().find(|(n, _)| n == name) {
                Some((_, holders)) => holders.push((i, ext.clone())),
                None => by_service.push((name.clone(), vec![(i, ext.clone())])),
            }
        }
    }
    for (name, holders) in by_service {
        let idx: Vec<usize> = holders.iter().map(|(i, _)| *i).collect();
        let (names, keys) = instance_naming(&idx, &series_labels);
        for (n, (i, ext)) in holders.into_iter().enumerate() {
            let scope = &parts[i].scope;
            let route_name = if n == 0 {
                name.clone()
            } else {
                format!("{name}{}{}", combined::INSTANCE_SEPARATOR, keys[n])
            };
            let route = format!("/service/{route_name}");
            scopes.insert(
                route.clone(),
                Arc::new(RouteScope {
                    scope: scope.clone(),
                    template: None,
                    throughput: None,
                    throughput_sources: Vec::new(),
                    capture: None,
                }),
            );
            service_sections.push(Section {
                name: name.clone(),
                route,
                instance: (idx.len() > 1).then(|| names[n].clone()),
                landing: false,
            });
            service_exts.push((route_name, ext));
        }
    }
    for (n, section) in service_sections.into_iter().enumerate() {
        sections.insert(insert_pos + n, section);
    }

    // Simple captures, once per name.
    let mut seen = HashSet::new();
    for part in &parts {
        for s in &part.simple {
            if seen.insert(s.name.clone()) {
                sections.push(Section {
                    name: format!("source: {}", s.name),
                    route: format!("/source/{}", s.name),
                    instance: None,
                    landing: false,
                });
            }
        }
    }

    let template = builtin.first().and_then(|&i| parts[i].template.clone());
    let throughput_query = builtin
        .first()
        .and_then(|&i| throughput_for(i).map(|(q, _)| q))
        .or_else(|| throughputs.first().map(|(_, q)| q.clone()));
    let context = DashboardContext {
        sections,
        filesize,
        service_exts,
        category: None,
        throughput_query,
        template,
        scopes,
    };
    (context, notices)
}

/// The names and route keys of the recordings at `indices`, told apart
/// among themselves.
fn instance_naming(
    indices: &[usize],
    labels: &[BTreeMap<String, String>],
) -> (Vec<String>, Vec<String>) {
    let group: Vec<BTreeMap<String, String>> = indices.iter().map(|&i| labels[i].clone()).collect();
    let names = combined::instance_names(&group);
    let keys = combined::instance_keys(&names);
    (names, keys)
}

/// A section [`render_section`] rendered.
pub struct Rendered {
    pub view: View,
    /// Why the template could not expand this section. The view is then an
    /// empty one (the navigation and no groups) whose `metadata` carries the
    /// message under `template_error`, so the page shows an empty section
    /// rather than failing, and the shells report the message as a notice.
    pub error: Option<TemplateError>,
}

/// The `View::metadata` key an unexpandable section's message is under.
pub const TEMPLATE_ERROR_KEY: &str = "template_error";

/// Render a single dashboard section by route, as the viewers do.
///
/// With a template in `ctx` ([`build_template_context`]), every route outside
/// `/service/` and `/source/` is the template's: expanded with the viewer
/// parameter `throughput` set from the throughput KPI, `None` for a route the
/// template does not have. `/service/` routes, and every route of a context
/// without a template, are [`generate_section`]'s. `None` is a 404.
pub fn render_section(
    data: &dyn MetricsSource,
    route: &str,
    ctx: &DashboardContext,
) -> Option<Rendered> {
    if let Some(scope) = ctx.scopes.get(route) {
        return render_scoped(data, route, ctx, scope);
    }
    let Some(template) = ctx.template.as_deref() else {
        return generate_section(data, route, ctx).map(|view| Rendered { view, error: None });
    };
    if route.starts_with("/service/") {
        return generate_section(data, route, ctx).map(|view| Rendered { view, error: None });
    }
    if is_reserved_route(route) {
        return None;
    }
    expand(data, template, route, ctx, ctx.throughput_query.as_deref())
}

/// A route of a combined source ([`build_combined_context`]): rendered
/// against [`Scoped`], each plot carrying the scope its queries read with
/// (the recording's sources, and the throughput's recording's where the
/// plot's query includes the throughput query), and the section's metadata
/// the recording's under [`combined::SCOPE_KEY`].
fn render_scoped(
    data: &dyn MetricsSource,
    route: &str,
    ctx: &DashboardContext,
    route_scope: &RouteScope,
) -> Option<Rendered> {
    let data = match &route_scope.capture {
        Some((_, own)) => own.as_ref(),
        None => data,
    };
    let scoped = Scoped::new(data, &route_scope.scope);
    let mut rendered = match route_scope.template.as_deref() {
        Some(template) => expand(
            &scoped,
            template,
            combined::base_route(route),
            ctx,
            route_scope.throughput.as_deref(),
        )?,
        None => generate_section(&scoped, route, ctx).map(|view| Rendered { view, error: None })?,
    };
    let own = &route_scope.scope.sources;
    if !own.is_empty() {
        let mut with_throughput = own.clone();
        for source in &route_scope.throughput_sources {
            if !with_throughput.contains(source) {
                with_throughput.push(source.clone());
            }
        }
        let throughput = route_scope.throughput.as_deref();
        rendered.view.scope_plots(|queries| {
            let reads_throughput =
                throughput.is_some_and(|t| queries.iter().any(|q| q.contains(t)));
            match reads_throughput {
                true => with_throughput.clone(),
                false => own.clone(),
            }
        });
        rendered.view.metadata.insert(
            combined::SCOPE_KEY.to_string(),
            serde_json::to_value(own).unwrap_or_default(),
        );
    } else {
        // A recording without source uuids: its queries restricted by its
        // matchers.
        let matchers = &route_scope.scope.matchers;
        rendered
            .view
            .map_queries(|q| combined::restrict_query(q, matchers));
    }
    if let Some((key, _)) = &route_scope.capture {
        // A recording beside a comparison renders as a single capture, its
        // queries sent to its source by their `__capture__` matcher.
        let capture = [combined::Matcher::eq(combined::CAPTURE_LABEL, key)];
        rendered
            .view
            .map_queries(|q| combined::restrict_query(q, &capture));
        rendered.view.metadata.insert(
            combined::SINGLE_CAPTURE_KEY.to_string(),
            serde_json::Value::String(key.clone()),
        );
    }
    Some(rendered)
}

/// A recording shown beside a comparison: one whose `arm` does not take a
/// slot (it has none), or that `--baseline`/`--experiment` did not name.
pub struct Beside {
    /// Its route key (`/cpu@<key>`) and the value of its queries'
    /// `__capture__` matcher.
    pub key: String,
    /// What the sidebar groups its sections under.
    pub name: String,
    /// Its own source.
    pub data: Arc<dyn MetricsSource>,
}

/// The keys and names of recordings shown beside a comparison, from their
/// labels, told apart among themselves as instances are
/// ([`combined::instance_names`]). One that no label tells apart (a lone
/// beside recording, or twins) is named by its `host`, else its `source`,
/// else `recording N` from its archive position (`positions`, 0-based).
pub fn beside_names(
    labels: &[BTreeMap<String, String>],
    positions: &[usize],
) -> Vec<(String, String)> {
    let names = combined::names_or(labels, |i, l| {
        l.get("host")
            .or_else(|| l.get("source"))
            .cloned()
            .unwrap_or_else(|| format!("recording {}", positions.get(i).copied().unwrap_or(i) + 1))
    });
    let keys = combined::instance_keys(&names);
    keys.into_iter().zip(names).collect()
}

/// Add to a comparison's context the sections of each recording shown
/// beside it: each renders as a single capture, from its own templates as
/// a combined view's instance does ([`build_combined_context`] of that
/// recording alone), at its routes followed by `@<key>` and grouped under
/// its name. Every query of those sections carries `__capture__="<key>"`,
/// so the comparison's baseline source, a [`combined::Routed`], answers it
/// from that recording; the view's metadata carries
/// [`combined::SINGLE_CAPTURE_KEY`], so the page draws it without the
/// experiment. A simple capture's `/source/` entry is not added (the page
/// lists its metrics from the baseline). Returns the template notices.
///
/// When `baseline` is not an agent's recording ([`has_system_metrics`]; two
/// services compared, their agent beside), its built-in sections are
/// empty, so the page opens on the comparison's first service section,
/// else the first beside recording's overview ([`Section::landing`]).
pub fn add_beside_sections(
    ctx: &mut DashboardContext,
    baseline: &dyn MetricsSource,
    beside: &[Beside],
    registry: &TemplateRegistry,
    preference: TemplatePreference,
) -> Vec<String> {
    let mut notices = Vec::new();
    for b in beside {
        // Rendered against its own source, so its queries need no scope.
        let member = Member {
            series_labels: BTreeMap::new(),
            metadata: b.data.file_metadata(),
            header: combined::Header::of(b.data.as_ref()),
            sources: Vec::new(),
        };
        let (sub, sub_notices) = build_combined_context(
            b.data.as_ref(),
            None,
            std::slice::from_ref(&member),
            registry,
            preference,
            None,
            None,
        );
        notices.extend(sub_notices);
        let suffix = |route: &str| format!("{route}{}{}", combined::INSTANCE_SEPARATOR, b.key);
        for section in sub.sections {
            if section.route == "/query" || section.route.starts_with("/source/") {
                continue;
            }
            let route = suffix(&section.route);
            if let Some(scope) = sub.scopes.get(&section.route) {
                ctx.scopes.insert(
                    route.clone(),
                    Arc::new(RouteScope {
                        scope: scope.scope.clone(),
                        template: scope.template.clone(),
                        throughput: scope.throughput.clone(),
                        throughput_sources: scope.throughput_sources.clone(),
                        capture: Some((b.key.clone(), Arc::clone(&b.data))),
                    }),
                );
            }
            ctx.sections.push(Section {
                name: section.name,
                route,
                instance: Some(b.name.clone()),
                landing: false,
            });
        }
        for (name, ext) in sub.service_exts {
            ctx.service_exts.push((suffix(&name), ext));
        }
    }
    if let Some(first) = beside.first()
        && !has_system_metrics(baseline)
    {
        let overview = format!("/overview{}{}", combined::INSTANCE_SEPARATOR, first.key);
        let at = ctx
            .sections
            .iter()
            .position(|s| s.route.starts_with("/service/") && s.instance.is_none())
            .or_else(|| ctx.sections.iter().position(|s| s.route == overview));
        if let Some(at) = at {
            ctx.sections[at].landing = true;
        }
    }
    notices
}

/// Whether the built-in sections can show `data`: it is a Rezolus agent's
/// recording (its `source` is `rezolus`), whichever samplers it ran, or it
/// carries the agent's `cpu_usage` under another source (a combined
/// recording). A service's recording is neither.
pub fn has_system_metrics(data: &dyn MetricsSource) -> bool {
    data.source() == "rezolus" || data.counter_names().iter().any(|n| n == "cpu_usage")
}

/// The file-metadata key under which a comparison's baseline lists the
/// recordings shown beside it: an object of each one's metadata by its
/// name ([`with_beside_metadata`]).
pub const BESIDE_METADATA_KEY: &str = "beside_recordings";

/// `metadata` (a capture's file metadata, a JSON object) with the metadata
/// of each recording in `beside` under [`BESIDE_METADATA_KEY`], by name,
/// leaving out its section template (`dashboard`). Unchanged when `beside`
/// is empty or `metadata` is not an object.
pub fn with_beside_metadata(metadata: &str, beside: &[Beside]) -> String {
    let Ok(serde_json::Value::Object(mut map)) = serde_json::from_str(metadata) else {
        return metadata.to_string();
    };
    if beside.is_empty() {
        return metadata.to_string();
    }
    let mut each = serde_json::Map::new();
    for b in beside {
        let meta: serde_json::Map<String, serde_json::Value> = b
            .data
            .file_metadata()
            .into_iter()
            .filter(|(k, _)| k != crate::source_templates::KEY_DASHBOARD)
            .map(|(k, v)| {
                let value =
                    serde_json::from_str(&v).unwrap_or(serde_json::Value::String(v.clone()));
                (k, value)
            })
            .collect();
        each.insert(b.name.clone(), serde_json::Value::Object(meta));
    }
    map.insert(
        BESIDE_METADATA_KEY.to_string(),
        serde_json::Value::Object(each),
    );
    serde_json::to_string(&map).unwrap_or_else(|_| metadata.to_string())
}

/// A comparison's baseline systeminfo with that of each recording in
/// `beside` that has one, as the System Info page's per-node object: the
/// baseline's under `baseline_name` (its entries, when it is already
/// per-node), each beside recording's under its name. `baseline` unchanged
/// when no beside recording has systeminfo.
pub fn with_beside_systeminfo(
    baseline: Option<String>,
    baseline_name: &str,
    beside: &[Beside],
) -> Option<String> {
    let parse = |raw: &str| {
        serde_json::from_str(raw).unwrap_or_else(|_| serde_json::Value::String(raw.to_string()))
    };
    let infos: Vec<(&str, serde_json::Value)> = beside
        .iter()
        .filter_map(|b| {
            b.data
                .file_metadata()
                .get("systeminfo")
                .map(|raw| (b.name.as_str(), parse(raw)))
        })
        .collect();
    if infos.is_empty() {
        return baseline;
    }
    let mut nodes = serde_json::Map::new();
    if let Some(raw) = &baseline {
        match parse(raw) {
            // Already per node: every value a node's object (one node's
            // systeminfo has string and array fields, `hostname`, `cpus`).
            serde_json::Value::Object(map)
                if !map.is_empty() && map.values().all(serde_json::Value::is_object) =>
            {
                nodes.extend(map);
            }
            one => {
                nodes.insert(baseline_name.to_string(), one);
            }
        }
    }
    for (name, info) in infos {
        nodes.insert(name.to_string(), info);
    }
    serde_json::to_string(&nodes).ok()
}

/// Expand `route` of `template` against `data`, with the viewer parameter
/// `throughput` set to `throughput`.
fn expand(
    data: &dyn MetricsSource,
    template: &SectionTemplate,
    route: &str,
    ctx: &DashboardContext,
    throughput: Option<&str>,
) -> Option<Rendered> {
    let mut params = BTreeMap::new();
    if let Some(query) = throughput {
        params.insert("throughput".to_string(), query.to_string());
    }
    match template.expand(data, route, ctx.sections.clone(), &params) {
        Ok(view) => view.map(|view| Rendered { view, error: None }),
        Err(error) => {
            let mut view = View::new(data, ctx.sections.clone());
            view.metadata.insert(
                TEMPLATE_ERROR_KEY.to_string(),
                serde_json::Value::String(error.to_string()),
            );
            Some(Rendered {
                view,
                error: Some(error),
            })
        }
    }
}

/// Render a single dashboard section by route. Returns `None` for an
/// unknown route — callers (the viewer) treat this as a 404.
///
/// Filesize is not applied — callers that want a filesize on the response
/// should call `view.set_filesize(...)` themselves.
pub fn generate_section(
    data: &dyn MetricsSource,
    route: &str,
    ctx: &DashboardContext,
) -> Option<View> {
    let view = if route == "/overview" {
        overview::generate(data, ctx.sections.clone(), ctx.throughput_query.as_deref())
    } else if let Some((_, _, generator)) = SECTION_META.iter().find(|(_, r, _)| *r == route) {
        generator(data, ctx.sections.clone())
    } else {
        let name = route.strip_prefix("/service/")?;
        // Category route takes precedence when active.
        if let Some((category_name, category_ext)) = &ctx.category {
            if category_name == name && ctx.service_exts.len() == 2 {
                let (a_name, a_ext) = &ctx.service_exts[0];
                let (b_name, b_ext) = &ctx.service_exts[1];
                category::generate(
                    data,
                    ctx.sections.clone(),
                    category_ext,
                    a_name,
                    a_ext,
                    b_name,
                    b_ext,
                )
            } else {
                return None;
            }
        } else if let Some((_, ext)) = ctx.service_exts.iter().find(|(n, _)| n == name) {
            service::generate(data, ctx.sections.clone(), ext)
        } else {
            return None;
        }
    };

    Some(view)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An agent is known by its source, whichever samplers it ran; another
    /// source by the agent's `cpu_usage` among its metrics.
    #[test]
    fn system_metrics_by_source_or_cpu_usage() {
        let agent = metriken_query::MemoryStore::builder()
            .source("rezolus")
            .build();
        assert!(has_system_metrics(&agent));
        let service = metriken_query::MemoryStore::builder().source("svc").build();
        assert!(!has_system_metrics(&service));
        let joined = metriken_query::MemoryStore::builder().source("svc").build();
        joined
            .insert_counter_series(
                "cpu_usage",
                BTreeMap::<String, String>::new(),
                vec![0],
                vec![0],
                None,
            )
            .unwrap();
        assert!(has_system_metrics(&joined));
    }

    /// The baseline's systeminfo is one node unless every value is a node's
    /// object; a beside recording's is added under its name.
    #[test]
    fn beside_systeminfo_keeps_a_one_node_baseline_whole() {
        let agent = metriken_query::MemoryStore::builder().build();
        agent.set_metadata("systeminfo", r#"{"hostname":"h"}"#);
        let beside = [Beside {
            key: "h".into(),
            name: "h".into(),
            data: Arc::new(agent),
        }];
        let one = r#"{"hostname":"a","cpus":[],"memory":{"total":1}}"#.to_string();
        let got: serde_json::Value =
            serde_json::from_str(&with_beside_systeminfo(Some(one), "svcA", &beside).unwrap())
                .unwrap();
        assert_eq!(got["svcA"]["hostname"], "a");
        assert_eq!(got["h"]["hostname"], "h");
        let nodes = r#"{"a":{"hostname":"a"},"b":{"hostname":"b"}}"#.to_string();
        let got: serde_json::Value =
            serde_json::from_str(&with_beside_systeminfo(Some(nodes), "svcA", &beside).unwrap())
                .unwrap();
        assert_eq!(got.as_object().unwrap().len(), 3, "{got}");
        assert_eq!(with_beside_systeminfo(None, "svcA", &[]), None);
    }

    /// A beside recording no label tells apart is named by its host, then
    /// its source, then its archive position; several are told apart by
    /// the labels that differ.
    #[test]
    fn beside_names_fall_back_to_host_source_position() {
        let l = |kv: &[(&str, &str)]| -> BTreeMap<String, String> {
            kv.iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect()
        };
        let name = |labels: &[BTreeMap<String, String>], pos: &[usize]| beside_names(labels, pos);
        assert_eq!(
            name(&[l(&[("source", "rezolus"), ("host", "h")])], &[0]),
            [("h".to_string(), "h".to_string())]
        );
        assert_eq!(
            name(&[l(&[("source", "rezolus")])], &[0]),
            [("rezolus".to_string(), "rezolus".to_string())]
        );
        assert_eq!(
            name(&[l(&[])], &[2]),
            [("recording-3".to_string(), "recording 3".to_string())]
        );
        assert_eq!(
            name(&[l(&[("source", "a")]), l(&[("source", "b")])], &[0, 2]),
            [
                ("a".to_string(), "a".to_string()),
                ("b".to_string(), "b".to_string())
            ]
        );
    }

    #[test]
    fn simple_only_file_has_no_builtins_and_one_source_section() {
        let sources = vec![SourceEntry {
            name: "myapp".into(),
            is_rezolus: false,
        }];
        let ctx = build_dashboard_context(None, &[], None, &sources);
        let routes: Vec<&str> = ctx.sections.iter().map(|s| s.route.as_str()).collect();
        assert!(routes.contains(&"/source/myapp"));
        assert!(!routes.contains(&"/overview"));
        assert!(!routes.contains(&"/cpu"));
    }

    #[test]
    fn mixed_file_shows_builtins_and_source_section() {
        let sources = vec![
            SourceEntry {
                name: "rezolus".into(),
                is_rezolus: true,
            },
            SourceEntry {
                name: "myapp".into(),
                is_rezolus: false,
            },
        ];
        let ctx = build_dashboard_context(None, &[], None, &sources);
        let routes: Vec<&str> = ctx.sections.iter().map(|s| s.route.as_str()).collect();
        assert!(routes.contains(&"/overview"));
        assert!(routes.contains(&"/cpu"));
        assert!(routes.contains(&"/source/myapp"));
    }

    /// A service section is listed second, after the first built-in (the
    /// Overview), from either builder; with no built-ins it is first.
    #[test]
    fn a_service_section_follows_the_first_built_in() {
        let ext: ServiceExtension = serde_json::from_value(serde_json::json!({
            "service_name": "svc",
            "kpis": [{"role": "throughput", "title": "t", "query": "up", "type": "gauge"}],
        }))
        .unwrap();
        let services = [("svc", &ext)];
        let template = crate::source_templates::embedded_dashboard_template();
        for ctx in [
            build_dashboard_context(None, &services, None, &[]),
            build_template_context(template.clone(), None, &services, None, &[]),
        ] {
            let routes: Vec<&str> = ctx.sections.iter().map(|s| s.route.as_str()).collect();
            assert_eq!(routes[..3], ["/overview", "/service/svc", "/exceptions"]);
        }
        let simple = [SourceEntry {
            name: "app".into(),
            is_rezolus: false,
        }];
        let ctx = build_template_context(template, None, &services, None, &simple);
        let routes: Vec<&str> = ctx.sections.iter().map(|s| s.route.as_str()).collect();
        assert_eq!(routes, ["/service/svc", "/source/app"]);
    }

    /// Two recordings of one service in one run render the service's
    /// template once each, named by host, where a comparison shows one entry
    /// for both (`build_context_dedupes_section_when_two_captures_share_service_name`).
    #[test]
    fn a_combined_source_renders_a_service_once_per_recording() {
        let template = serde_json::json!({
            "service_name": "svc",
            "kpis": [{"role": "throughput", "title": "t", "query": "up", "type": "gauge"}],
        })
        .to_string();
        let member = |host: &str| crate::combined::Member {
            series_labels: [("source", "svc"), ("host", host)]
                .into_iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            metadata: [("source", "svc"), ("service_queries", template.as_str())]
                .into_iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            header: Default::default(),
            sources: vec![format!("uuid-{host}")],
        };
        let data = metriken_query::MemoryStore::builder().build();
        let (ctx, notices) = build_combined_context(
            &data,
            None,
            &[member("h1"), member("h2")],
            &TemplateRegistry::empty(),
            TemplatePreference::Archive,
            None,
            None,
        );
        assert!(notices.is_empty(), "{notices:?}");
        let entries: Vec<(&str, &str, Option<&str>)> = ctx
            .sections
            .iter()
            .map(|s| (s.name.as_str(), s.route.as_str(), s.instance.as_deref()))
            .collect();
        assert_eq!(
            entries,
            [
                ("svc", "/service/svc", Some("h1")),
                ("svc", "/service/svc@h2", Some("h2")),
            ]
        );
        let names: Vec<&str> = ctx.service_exts.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["svc", "svc@h2"]);
        // The KPI has no data here, so it is listed, not drawn; the view is
        // the second recording's, its scope that recording's sources.
        let view = render_section(&data, "/service/svc@h2", &ctx).unwrap().view;
        assert_eq!(view.metadata["scope"], serde_json::json!(["uuid-h2"]));
    }

    /// A recording whose own metadata carries a service template keeps that
    /// service's throughput on its Overview with or without source uuids:
    /// the throughput query is read as the recording's other queries are.
    #[test]
    fn a_recording_without_uuids_keeps_its_own_services_throughput() {
        let template = serde_json::json!({
            "service_name": "svc",
            "kpis": [{
                "role": "throughput",
                "title": "t",
                "query": "sum(irate(svc_requests[5m]))",
                "type": "delta_counter",
                "denominator": true,
            }],
        })
        .to_string();
        let member = |host: &str, sources: Vec<String>| crate::combined::Member {
            series_labels: [("source", "rezolus"), ("host", host)]
                .into_iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            metadata: [
                ("source", "rezolus"),
                ("service_queries", template.as_str()),
            ]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
            header: Default::default(),
            sources,
        };
        let data = metriken_query::MemoryStore::builder().build();
        for sources in [vec!["u".to_string()], vec![]] {
            let (ctx, _) = build_combined_context(
                &data,
                None,
                &[member("h1", sources.clone())],
                &TemplateRegistry::empty(),
                TemplatePreference::Archive,
                None,
                None,
            );
            assert_eq!(
                ctx.scopes["/overview"].throughput.as_deref(),
                Some("sum(irate(svc_requests[5m]))"),
                "{sources:?}"
            );
        }
    }

    #[test]
    fn build_context_produces_full_navigation() {
        let ctx = build_dashboard_context(None, &[], None, &[]);

        // Sections must be: Overview, then SECTION_META in order. No
        // service / category entries when none supplied.
        let mut expected: Vec<(&str, &str)> = vec![("Overview", "/overview")];
        for (name, route, _) in SECTION_META {
            expected.push((name, route));
        }

        let actual: Vec<(&str, &str)> = ctx
            .sections
            .iter()
            .map(|s| (s.name.as_str(), s.route.as_str()))
            .collect();

        assert_eq!(actual, expected);
        assert!(ctx.service_exts.is_empty());
        assert!(ctx.category.is_none());
        assert!(ctx.throughput_query.is_none());
        assert!(ctx.filesize.is_none());
    }

    #[test]
    fn generate_section_renders_known_routes_returns_none_for_unknown() {
        let data = metriken_query::MemoryStore::builder().build();
        let ctx = build_dashboard_context(None, &[], None, &[]);

        // Overview renders.
        let overview = generate_section(&data, "/overview", &ctx).expect("overview some");
        let overview_json = serde_json::to_string(&overview).unwrap();
        assert!(overview_json.contains("\"groups\""));
        assert!(overview_json.contains("\"sections\""));
        assert!(overview_json.contains("\"interval\""));

        // A SECTION_META route renders.
        let cpu = generate_section(&data, "/cpu", &ctx).expect("cpu some");
        let cpu_json = serde_json::to_string(&cpu).unwrap();
        assert!(cpu_json.contains("\"groups\""));
        assert!(cpu_json.contains("\"sections\""));
        assert!(cpu_json.contains("\"interval\""));

        // Unknown route returns None.
        assert!(generate_section(&data, "/no-such-route", &ctx).is_none());
        // Unknown service route returns None too.
        assert!(generate_section(&data, "/service/missing", &ctx).is_none());
    }

    #[test]
    fn generate_section_renders_service_and_category_routes() {
        use crate::service_extension::{CategoryExtension, CategoryKpi, Kpi, ServiceExtension};
        use std::collections::HashMap;

        let kpi = |role: &str, title: &str, query: &str| Kpi {
            role: role.to_string(),
            title: title.to_string(),
            description: None,
            query: query.to_string(),
            metric_type: "delta_counter".to_string(),
            subtype: None,
            unit_system: Some("rate".to_string()),
            percentiles: None,
            available: true,
            denominator: false,
            subgroup: None,
            subgroup_description: None,
            full_width: false,
            check: None,
        };
        let vllm = ServiceExtension {
            service_name: "vllm".to_string(),
            aliases: vec![],
            service_metadata: HashMap::new(),
            kpis: vec![kpi("throughput", "Generation Token Rate", "vllm_q")],
        };
        let sglang = ServiceExtension {
            service_name: "sglang".to_string(),
            aliases: vec![],
            service_metadata: HashMap::new(),
            kpis: vec![kpi("throughput", "Generation Token Rate", "sglang_q")],
        };

        let data = metriken_query::MemoryStore::builder().build();

        // Per-service flow (no category).
        let ctx = build_dashboard_context(None, &[("vllm", &vllm)], None, &[]);
        let view = generate_section(&data, "/service/vllm", &ctx).expect("vllm renders");
        let json = serde_json::to_string(&view).unwrap();
        assert!(json.contains("\"service_name\""));
        assert!(json.contains("\"vllm\""));

        // Category flow with two members.
        let category = CategoryExtension {
            service_name: "inference-library".to_string(),
            category: true,
            members: vec!["vllm".to_string(), "sglang".to_string()],
            kpis: vec![CategoryKpi {
                role: "throughput".to_string(),
                title: "Generation Token Rate".to_string(),
                metric_type: "delta_counter".to_string(),
                subtype: None,
                unit_system: Some("rate".to_string()),
                percentiles: None,
                denominator: false,
                subgroup: None,
                subgroup_description: None,
                full_width: false,
                member_titles: HashMap::new(),
            }],
        };
        let ctx = build_dashboard_context(
            None,
            &[("vllm", &vllm), ("sglang", &sglang)],
            Some(("inference-library", &category)),
            &[],
        );
        // Category renders at /service/<category-name>.
        let view =
            generate_section(&data, "/service/inference-library", &ctx).expect("category renders");
        let json = serde_json::to_string(&view).unwrap();
        assert!(json.contains("inference-library"));
        // Per-member sections are absent in category mode.
        assert!(generate_section(&data, "/service/vllm", &ctx).is_none());
        assert!(generate_section(&data, "/service/sglang", &ctx).is_none());
    }

    #[test]
    fn build_context_emits_category_nav_entry_when_category_supplied() {
        use crate::service_extension::{CategoryExtension, CategoryKpi, Kpi, ServiceExtension};
        use std::collections::HashMap;

        let kpi = |role: &str, title: &str, query: &str| Kpi {
            role: role.to_string(),
            title: title.to_string(),
            description: None,
            query: query.to_string(),
            metric_type: "delta_counter".to_string(),
            subtype: None,
            unit_system: Some("rate".to_string()),
            percentiles: None,
            available: true,
            denominator: false,
            subgroup: None,
            subgroup_description: None,
            full_width: false,
            check: None,
        };
        let vllm = ServiceExtension {
            service_name: "vllm".to_string(),
            aliases: vec![],
            service_metadata: HashMap::new(),
            kpis: vec![kpi("throughput", "Generation Token Rate", "vllm_q")],
        };
        let sglang = ServiceExtension {
            service_name: "sglang".to_string(),
            aliases: vec![],
            service_metadata: HashMap::new(),
            kpis: vec![kpi("throughput", "Generation Token Rate", "sglang_q")],
        };
        let category = CategoryExtension {
            service_name: "inference-library".to_string(),
            category: true,
            members: vec!["vllm".to_string(), "sglang".to_string()],
            kpis: vec![CategoryKpi {
                role: "throughput".to_string(),
                title: "Generation Token Rate".to_string(),
                metric_type: "delta_counter".to_string(),
                subtype: None,
                unit_system: Some("rate".to_string()),
                percentiles: None,
                denominator: false,
                subgroup: None,
                subgroup_description: None,
                full_width: false,
                member_titles: HashMap::new(),
            }],
        };

        let ctx = build_dashboard_context(
            None,
            &[("vllm", &vllm), ("sglang", &sglang)],
            Some(("inference-library", &category)),
            &[],
        );

        // Category nav entry present.
        let routes: Vec<&str> = ctx.sections.iter().map(|s| s.route.as_str()).collect();
        assert!(routes.contains(&"/service/inference-library"));
        // Per-member nav entries absent under category mode.
        assert!(!routes.contains(&"/service/vllm"));
        assert!(!routes.contains(&"/service/sglang"));

        // generate_section honors the same: category route renders, members 404.
        let data = metriken_query::MemoryStore::builder().build();
        assert!(generate_section(&data, "/service/inference-library", &ctx).is_some());
        assert!(generate_section(&data, "/service/vllm", &ctx).is_none());
        assert!(generate_section(&data, "/service/sglang", &ctx).is_none());
    }

    #[test]
    fn build_context_dedupes_section_when_two_captures_share_service_name() {
        use crate::service_extension::{Kpi, ServiceExtension};
        use std::collections::HashMap;

        let kpi = Kpi {
            role: "throughput".to_string(),
            title: "Generation Token Rate".to_string(),
            description: None,
            query: "vllm_q".to_string(),
            metric_type: "delta_counter".to_string(),
            subtype: None,
            unit_system: Some("rate".to_string()),
            percentiles: None,
            available: true,
            denominator: false,
            subgroup: None,
            subgroup_description: None,
            full_width: false,
            check: None,
        };
        let vllm_a = ServiceExtension {
            service_name: "vllm".to_string(),
            aliases: vec![],
            service_metadata: HashMap::new(),
            kpis: vec![kpi.clone()],
        };
        let vllm_b = vllm_a.clone();

        let ctx = build_dashboard_context(None, &[("vllm", &vllm_a), ("vllm", &vllm_b)], None, &[]);

        // Nav contains exactly one /service/vllm entry.
        let vllm_count = ctx
            .sections
            .iter()
            .filter(|s| s.route == "/service/vllm")
            .count();
        assert_eq!(
            vllm_count, 1,
            "expected one /service/vllm entry in nav, got {vllm_count}"
        );

        // The deduped service ext list also collapses to one entry, so
        // generate_section can still render the route.
        assert_eq!(ctx.service_exts.len(), 1);
        let data = metriken_query::MemoryStore::builder().build();
        assert!(generate_section(&data, "/service/vllm", &ctx).is_some());
    }
}
