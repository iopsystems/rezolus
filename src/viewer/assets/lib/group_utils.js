// Small helpers for walking the dashboard's group/subgroup tree.
// Kept in its own dependency-free module so any consumer (layout, data,
// explorers, cgroup_selector, section_views, ...) can import without
// risking an import cycle.

// Flatten a group's plots across its subgroups. Falls back to the legacy
// flat `group.plots` shape during the transition window.
export const collectGroupPlots = (group) =>
    Array.isArray(group.subgroups)
        ? group.subgroups.flatMap((sg) => sg.plots || [])
        : group.plots || [];
