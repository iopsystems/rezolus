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

// Flatten a group's plots across its subgroups. Falls back to the legacy
// flat `group.plots` shape during the transition window.
export const collectGroupPlots = (group) =>
    Array.isArray(group.subgroups)
        ? group.subgroups.flatMap((sg) => sg.plots || [])
        : group.plots || [];
