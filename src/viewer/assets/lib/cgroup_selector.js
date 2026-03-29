// cgroup_selector.js - Cgroup selector component for selecting which cgroups to view individually
//
// Attrs:
//   groups: array of group objects with plots
//   executeQuery: (query) => Promise<result> — runs a PromQL range query
//   substitutePattern: (query, pattern) => string — substitutes cgroup placeholder
//   setActiveCgroupPattern: (pattern) => void — sets the global active cgroup pattern

export const CgroupSelector = {
    oninit(vnode) {
        vnode.state.selectedCgroups = new Set();
        vnode.state.availableCgroups = new Set();
        vnode.state.loading = true;
        vnode.state.error = null;

        // Fetch available cgroups from the data
        this.fetchAvailableCgroups(vnode);
    },

    async fetchAvailableCgroups(vnode) {
        const executeQuery = vnode.attrs.executeQuery;
        try {
            // Try multiple queries to find cgroups
            const queries = [
                'sum by (name) (cgroup_cpu_usage)',
                'group by (name) (cgroup_cpu_usage)',
                'cgroup_cpu_usage',
                'sum by (name) (rate(cgroup_cpu_usage[1m]))',
            ];

            let cgroups = new Set();
            let foundData = false;

            for (const query of queries) {
                try {
                    const result = await executeQuery(query);

                    if (
                        result.status === 'success' &&
                        result.data &&
                        result.data.result &&
                        result.data.result.length > 0
                    ) {
                        result.data.result.forEach((series) => {
                            // Check different possible locations for the name label
                            if (series.metric) {
                                if (series.metric.name) {
                                    cgroups.add(series.metric.name);
                                    foundData = true;
                                }
                                // Also check all other labels that might contain cgroup names
                                Object.entries(series.metric).forEach(
                                    ([key, value]) => {
                                        if (
                                            key === 'name' ||
                                            key.includes('cgroup') ||
                                            key === 'container'
                                        ) {
                                            if (value && value !== '') {
                                                cgroups.add(value);
                                                foundData = true;
                                            }
                                        }
                                    },
                                );
                            }
                        });

                        if (foundData) {
                            break;
                        }
                    }
                } catch (queryError) {
                    console.warn(`Query failed: ${query}`, queryError);
                }
            }

            if (!foundData) {
                // If no cgroup metrics found, try to extract from any plots that have cgroup in the query

                // Look for any existing cgroup data in the plots
                if (vnode.attrs.groups) {
                    vnode.attrs.groups.forEach((group) => {
                        if (group.plots) {
                            group.plots.forEach((plot) => {
                                if (
                                    plot.promql_query &&
                                    plot.promql_query.includes('cgroup')
                                ) {
                                    // Try to extract cgroup names from the query results if they exist
                                }
                            });
                        }
                    });
                }

                // If no cgroup data found, show error with empty list
                if (cgroups.size === 0) {
                    vnode.state.error = 'No cgroup data found';
                }
            }

            vnode.state.availableCgroups = cgroups;
            vnode.state.loading = false;
            m.redraw();
        } catch (error) {
            console.error('Failed to fetch available cgroups:', error);
            vnode.state.error = 'Failed to load cgroups: ' + error.message;
            vnode.state.loading = false;

            // Show empty list on error
            vnode.state.availableCgroups = new Set();
            m.redraw();
        }
    },

    async updateQueries(vnode) {
        const executeQuery = vnode.attrs.executeQuery;
        const substitutePattern = vnode.attrs.substitutePattern;
        const setActiveCgroupPattern = vnode.attrs.setActiveCgroupPattern;

        // Cancel any in-flight requests
        if (vnode.state.updateInProgress) {
            vnode.state.cancelUpdate = true;
            return;
        }

        vnode.state.updateInProgress = true;
        vnode.state.cancelUpdate = false;

        // Update all PromQL queries with the selected cgroups
        const selectedArray = Array.from(vnode.state.selectedCgroups);
        // Build alternation pattern for Labels::matches
        // No escaping needed — Labels::matches uses simple string equality,
        // and cgroup names don't contain | which is the only special character.
        const selectedPattern =
            selectedArray.length > 1
                ? '(' + selectedArray.join('|') + ')'
                : selectedArray.length === 1
                  ? selectedArray[0]
                  : ''; // Empty string matches nothing

        // Store globally so live refresh can re-apply the pattern
        setActiveCgroupPattern(selectedPattern || null);

        // Store the original queries if not already stored
        if (!vnode.state.originalQueries) {
            vnode.state.originalQueries = new Map();
            vnode.attrs.groups.forEach((group, groupIdx) => {
                if (group.plots) {
                    group.plots.forEach((plot, plotIdx) => {
                        if (plot.promql_query) {
                            const key = `${groupIdx}-${plotIdx}`;
                            vnode.state.originalQueries.set(
                                key,
                                plot.promql_query,
                            );
                        }
                    });
                }
            });
        }

        // Track the update generation to ignore stale results
        const updateGeneration = ++vnode.state.updateGeneration || 1;
        vnode.state.updateGeneration = updateGeneration;

        // Collect all plots that need updating
        const plotsToUpdate = [];
        vnode.attrs.groups.forEach((group, groupIdx) => {
            if (group.plots) {
                group.plots.forEach((plot, plotIdx) => {
                    const key = `${groupIdx}-${plotIdx}`;
                    const originalQuery = vnode.state.originalQueries.get(key);

                    if (
                        originalQuery &&
                        originalQuery.includes('__SELECTED_CGROUPS__')
                    ) {
                        const updatedQuery = substitutePattern(
                            originalQuery,
                            selectedPattern || null,
                        );
                        plotsToUpdate.push({
                            plot,
                            updatedQuery,
                            originalQuery,
                        });
                    }
                });
            }
        });

        // Execute queries in batches to avoid overwhelming the server
        const BATCH_SIZE = 5;
        for (let i = 0; i < plotsToUpdate.length; i += BATCH_SIZE) {
            // Check if this update was cancelled
            if (
                vnode.state.cancelUpdate ||
                vnode.state.updateGeneration !== updateGeneration
            ) {
                vnode.state.updateInProgress = false;
                return;
            }

            const batch = plotsToUpdate.slice(i, i + BATCH_SIZE);
            const promises = batch.map(
                async ({ plot, updatedQuery, originalQuery }) => {
                    plot.promql_query = updatedQuery;

                    try {
                        const result =
                            await executeQuery(updatedQuery);

                        // Check if this result is still relevant
                        if (vnode.state.updateGeneration !== updateGeneration) {
                            return;
                        }

                        if (result.status === 'success' && result.data) {
                            // Update the plot data directly
                            if (
                                result.data.result &&
                                result.data.result.length > 0
                            ) {
                                // Handle multi-series data
                                if (
                                    plot.opts.style === 'multi' ||
                                    plot.opts.style === 'heatmap'
                                ) {
                                    const seriesData = [];
                                    const seriesNames = [];

                                    result.data.result.forEach((series) => {
                                        if (
                                            series.values &&
                                            series.values.length > 0
                                        ) {
                                            const timestamps =
                                                series.values.map(
                                                    ([ts, _]) => ts,
                                                );
                                            const values = series.values.map(
                                                ([_, val]) => parseFloat(val),
                                            );

                                            // Use the first series for timestamps
                                            if (seriesData.length === 0) {
                                                seriesData.push(timestamps);
                                            }
                                            seriesData.push(values);

                                            // Extract series name from metric labels
                                            const name =
                                                series.metric.name ||
                                                series.metric.id ||
                                                series.metric.__name__ ||
                                                `Series ${seriesNames.length + 1}`;
                                            seriesNames.push(name);
                                        }
                                    });

                                    if (seriesData.length > 1) {
                                        plot.data = seriesData;
                                        plot.series_names = seriesNames;
                                    } else {
                                        plot.data = [];
                                    }
                                } else {
                                    // Single series data
                                    const sample = result.data.result[0];
                                    if (
                                        sample.values &&
                                        Array.isArray(sample.values)
                                    ) {
                                        const timestamps = sample.values.map(
                                            ([ts, _]) => ts,
                                        );
                                        const values = sample.values.map(
                                            ([_, val]) => parseFloat(val),
                                        );
                                        plot.data = [timestamps, values];
                                    } else {
                                        plot.data = [];
                                    }
                                }
                            } else {
                                plot.data = [];
                            }
                        } else {
                            console.warn(`No data for query: ${updatedQuery}`);
                            plot.data = [];
                        }
                    } catch (error) {
                        console.error(
                            `Failed to execute query for plot ${plot.opts.title}:`,
                            error,
                        );
                        plot.data = [];
                    }
                },
            );

            // Wait for this batch to complete before starting the next
            await Promise.all(promises);
        }

        // Only redraw if this update is still current
        if (vnode.state.updateGeneration === updateGeneration) {
            m.redraw();
        }

        vnode.state.updateInProgress = false;
    },

    addCgroup(vnode, cgroup) {
        vnode.state.selectedCgroups.add(cgroup);
        this.debouncedUpdateQueries(vnode);
    },

    removeCgroup(vnode, cgroup) {
        vnode.state.selectedCgroups.delete(cgroup);
        this.debouncedUpdateQueries(vnode);
    },

    debouncedUpdateQueries(vnode) {
        // Cancel any pending update
        if (vnode.state.updateTimer) {
            clearTimeout(vnode.state.updateTimer);
        }

        // Schedule a new update after a short delay
        vnode.state.updateTimer = setTimeout(() => {
            this.updateQueries(vnode);
        }, 300); // 300ms debounce
    },

    view(vnode) {
        const unselectedCgroups = Array.from(vnode.state.availableCgroups)
            .filter((cg) => !vnode.state.selectedCgroups.has(cg))
            .sort();
        const selectedCgroups = Array.from(vnode.state.selectedCgroups).sort();

        // Track which items are selected in the lists
        if (!vnode.state.leftSelected) vnode.state.leftSelected = new Set();
        if (!vnode.state.rightSelected) vnode.state.rightSelected = new Set();

        return m('div.cgroup-selector', [
            m('h3', 'Cgroup Selection'),
            vnode.state.error && m('div.error-message', vnode.state.error),
            m('div.selector-container', [
                m('div.selector-column', [
                    m('h4', 'Available Cgroups (Aggregate)'),
                    m(
                        'select.cgroup-select[multiple]',
                        {
                            size: 10,
                            onchange: (e) => {
                                vnode.state.leftSelected.clear();
                                Array.from(e.target.selectedOptions).forEach(
                                    (option) => {
                                        vnode.state.leftSelected.add(
                                            option.value,
                                        );
                                    },
                                );
                            },
                        },
                        vnode.state.loading
                            ? [m('option[disabled]', 'Loading cgroups...')]
                            : unselectedCgroups.length === 0
                              ? [m('option[disabled]', 'No cgroups available')]
                              : unselectedCgroups.map((cgroup) =>
                                    m(
                                        'option',
                                        {
                                            value: cgroup,
                                            selected:
                                                vnode.state.leftSelected.has(
                                                    cgroup,
                                                ),
                                        },
                                        cgroup,
                                    ),
                                ),
                    ),
                ]),
                m('div.selector-controls', [
                    m(
                        'button',
                        {
                            title: 'Move selected to individual',
                            disabled: vnode.state.leftSelected.size === 0,
                            onclick: () => {
                                // Batch add cgroups
                                vnode.state.leftSelected.forEach((cg) => {
                                    vnode.state.selectedCgroups.add(cg);

                                });
                                vnode.state.leftSelected.clear();
                                // Single update for all additions
                                this.debouncedUpdateQueries(vnode);
                            },
                        },
                        '>',
                    ),
                    m(
                        'button',
                        {
                            title: 'Move all to individual',
                            disabled: unselectedCgroups.length === 0,
                            onclick: () => {
                                // Batch add all unselected cgroups
                                unselectedCgroups.forEach((cg) => {
                                    vnode.state.selectedCgroups.add(cg);

                                });
                                vnode.state.leftSelected.clear();
                                // Single update for all additions
                                this.debouncedUpdateQueries(vnode);
                            },
                        },
                        '>>',
                    ),
                    m(
                        'button',
                        {
                            title: 'Move all to aggregate',
                            disabled: selectedCgroups.length === 0,
                            onclick: () => {
                                // Batch remove all selected cgroups
                                selectedCgroups.forEach((cg) => {
                                    vnode.state.selectedCgroups.delete(cg);

                                });
                                vnode.state.rightSelected.clear();
                                // Single update for all removals
                                this.debouncedUpdateQueries(vnode);
                            },
                        },
                        '<<',
                    ),
                    m(
                        'button',
                        {
                            title: 'Move selected to aggregate',
                            disabled: vnode.state.rightSelected.size === 0,
                            onclick: () => {
                                // Batch remove cgroups
                                vnode.state.rightSelected.forEach((cg) => {
                                    vnode.state.selectedCgroups.delete(cg);

                                });
                                vnode.state.rightSelected.clear();
                                // Single update for all removals
                                this.debouncedUpdateQueries(vnode);
                            },
                        },
                        '<',
                    ),
                ]),
                m('div.selector-column', [
                    m('h4', 'Individual Cgroups'),
                    m(
                        'select.cgroup-select[multiple]',
                        {
                            size: 10,
                            onchange: (e) => {
                                vnode.state.rightSelected.clear();
                                Array.from(e.target.selectedOptions).forEach(
                                    (option) => {
                                        vnode.state.rightSelected.add(
                                            option.value,
                                        );
                                    },
                                );
                            },
                        },
                        selectedCgroups.length === 0
                            ? [m('option[disabled]', 'No cgroups selected')]
                            : selectedCgroups.map((cgroup) =>
                                  m(
                                      'option',
                                      {
                                          value: cgroup,
                                          selected:
                                              vnode.state.rightSelected.has(
                                                  cgroup,
                                              ),
                                      },
                                      cgroup,
                                  ),
                              ),
                    ),
                ]),
            ]),
            m('div.selector-info', [
                m(
                    'small',
                    `${unselectedCgroups.length} available, ${selectedCgroups.length} selected`,
                ),
            ]),
        ]);
    },
};
