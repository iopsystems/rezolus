import { baseRoute } from '../features/group_utils.js';

// The sidebar's grouping of the navigation. The Overview and Exceptions are
// found by route, not by name: a section template names its sections, and a
// recording's template may rename them. The query explorer is found by route
// too; its name is fixed, since a template places it with
// `{"builtin": "query"}` and cannot put another section at `/query`.

// Routes the sidebar lists on their own, above the sampler sections.
const PINNED_ROUTES = ['/query', '/overview', '/exceptions'];

// The Overview, the Exceptions and the sampler sections among `sections`.
const builtIns = (sections) => ({
    overview: sections.find((s) => baseRoute(s.route) === '/overview'),
    exceptions: sections.find((s) => baseRoute(s.route) === '/exceptions'),
    samplers: sections.filter((s) => !PINNED_ROUTES.includes(baseRoute(s.route))),
});

// Group `sections` (`[{ name, route, instance? }]`) for the sidebar. In
// compare mode the cgroup section is hidden (v1 scope).
//
// A section carrying `instance` is one of several renderings of the
// built-in sections, one per recording of a combined archive: those are
// grouped by instance, in order, under `instances` (`[{ name, overview,
// exceptions, samplers }]`), and the top-level overview, exceptions and
// samplers are left empty.
export const groupSections = (sections, compareMode) => {
    const visible = compareMode
        ? sections.filter((s) => baseRoute(s.route) !== '/cgroups')
        : sections;
    const services = visible.filter((s) => s.route.startsWith('/service/'));
    // Foreign (non-Rezolus) source sections get their own "Metrics" group;
    // "Samplers" is reserved for the Rezolus agent's built-in samplers.
    const metrics = visible.filter((s) => s.route.startsWith('/source/'));
    const rest = visible.filter(
        (s) => s.route !== '/query'
            && !s.route.startsWith('/service/')
            && !s.route.startsWith('/source/'),
    );
    const queryExplorer = visible.find((s) => s.route === '/query');
    const instances = [];
    for (const s of rest) {
        if (s.instance == null) continue;
        let entry = instances.find((i) => i.name === s.instance);
        if (!entry) {
            entry = { name: s.instance, sections: [] };
            instances.push(entry);
        }
        entry.sections.push(s);
    }
    if (instances.length > 0) {
        return {
            queryExplorer,
            overview: undefined,
            exceptions: undefined,
            services,
            metrics,
            samplers: [],
            instances: instances.map((i) => ({ name: i.name, ...builtIns(i.sections) })),
        };
    }
    return { queryExplorer, services, metrics, ...builtIns(rest), instances: [] };
};
