// Small helpers for walking the dashboard's group/subgroup tree.
// Kept in its own dependency-free module so any consumer (layout, data,
// explorers, cgroup_selector, section_views, ...) can import without
// risking an import cycle.

// A section's route without its instance key. In an archive of several
// recordings of one run, a template that renders for more than one recording
// has its first instance at the template's routes and each further one at
// `<route>@<key>` (`/cpu@web-02`), so a check for a particular section
// compares this.
export const baseRoute = (route) => (route || '').split('@')[0];

// Whether a section renders as a single capture: a recording shown beside a
// comparison, whose section metadata names it (`single_capture`).
export const isSingleCapture = (metadata) => typeof metadata?.single_capture === 'string';

// The route the page opens on: the report in report mode, the category's
// section, the section the backend marked `landing` (a comparison whose
// baseline has no system metrics: dashboard::dashboard::add_beside_sections),
// the first service's section, the overview, or, for a simple capture with
// no overview, its first `/source/` entry.
export const defaultLandingRoute = ({ reportMode, categoryName, serviceNames = [], sections = [] }) => {
    if (reportMode) return '/report';
    if (categoryName) return `/service/${categoryName}`;
    const landing = sections.find((s) => s.landing === true)?.route;
    if (landing) return landing;
    if (serviceNames.length > 0) return `/service/${serviceNames[0]}`;
    if (sections.some((s) => s.route === '/overview')) return '/overview';
    return sections.find((s) => s.route && s.route.startsWith('/source/'))?.route || '/overview';
};

// The route an upload opens on, from the `/api/v1/sections` response that
// follows it (`{ data: { sections } }`): the landing rule of
// `defaultLandingRoute`, with no service names or category known yet.
export const uploadLandingRoute = (sectionsResponse) =>
    defaultLandingRoute({ sections: sectionsResponse?.data?.sections || [] });

// Whether a section draws A/B: compare mode, unless it belongs to a
// recording shown beside the comparison.
export const sectionCompares = (compareMode, metadata) =>
    !!compareMode && !isSingleCapture(metadata);

// Whether a Group draws A/B: the page's compare mode, unless the section
// that renders it said it is a single capture.
export const groupCompares = (compareMode, attrs) =>
    !!compareMode && attrs?.singleCapture !== true;

// What a chart's pin records about compare mode: the aliases in compare
// mode, `singleCapture` for a chart of a recording beside a comparison, or
// nothing outside compare mode.
export const pinCompareMeta = (pageCompare, groupCompare, aliases) => {
    if (groupCompare) return aliases;
    return pageCompare ? { singleCapture: true } : null;
};

// Whether a pinned chart draws A/B in the notebook, report and selection
// views. An entry pinned from a recording beside a comparison does not: it
// says so (`single_capture`), or, saved before that field existed, its
// query names the recording (`__capture__="<key>"`).
export const pinnedCompares = (compareMode, entry, spec) =>
    !!compareMode && !!spec?.promql_query
    && entry?.single_capture !== true
    && !/__capture__="/.test(entry?.promql_query || spec.promql_query);

// The instance names of the recordings shown beside a comparison: in compare
// mode only those sections carry an `instance`.
export const besideNames = (sections) => [
    ...new Set((sections || []).map((s) => s.instance).filter((n) => n != null)),
];

// Flatten a group's plots across its subgroups. Falls back to the legacy
// flat `group.plots` shape during the transition window.
export const collectGroupPlots = (group) =>
    Array.isArray(group.subgroups)
        ? group.subgroups.flatMap((sg) => sg.plots || [])
        : group.plots || [];
