// Which view the page mounts for a section, by its route. The query explorer
// and the client-only views (system info, metadata, notebook, selection,
// report) are the page's own and are found by route, never by name: a
// recording's section template names its sections, and a template section
// named "Notebook" at another route is an ordinary section.

// The client-only sections: their navigation descriptors, keyed by view.
export const CLIENT_SECTIONS = {
    systeminfo: { name: 'System Info', route: '/systeminfo' },
    metadata: { name: 'Metadata', route: '/metadata' },
    notebook: { name: 'Notebook', route: '/notebook' },
    selection: { name: 'Selection', route: '/selection' },
    report: { name: 'Report', route: '/report' },
};

const VIEW_BY_ROUTE = new Map([
    ['/query', 'query'],
    ...Object.entries(CLIENT_SECTIONS).map(([view, s]) => [s.route, view]),
]);

// The page's own view for `route` ('query', 'systeminfo', 'metadata',
// 'notebook', 'selection' or 'report'), or null for a section whose groups
// the backend renders.
export const clientViewFor = (route) => VIEW_BY_ROUTE.get(route) ?? null;
