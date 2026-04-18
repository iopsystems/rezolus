// Dashboard definitions — auto-generated from src/viewer/dashboard/*.rs
// Do not edit manually. Run `cargo xtask generate-dashboards` to update.

const cache = {};
let sectionsCache = null;
const templateCache = {};

async function loadSections() {
    if (sectionsCache) return sectionsCache;
    const resp = await fetch('dashboards/sections.json');
    if (!resp.ok) throw new Error('Failed to load sections');
    sectionsCache = await resp.json();
    return sectionsCache;
}

async function loadDashboard(sectionKey) {
    if (cache[sectionKey]) return cache[sectionKey];
    const resp = await fetch(`dashboards/${sectionKey}.json`);
    if (!resp.ok) throw new Error(`Failed to load dashboard: ${sectionKey}`);
    cache[sectionKey] = await resp.json();
    return cache[sectionKey];
}

// --- Service template support ---

// Map of known service templates available on the static site.
// Keys are source names that appear in parquet metadata.
const KNOWN_TEMPLATES = ['cachecannon', 'llm-perf', 'sglang', 'valkey', 'vllm'];

async function loadTemplate(name) {
    if (templateCache[name]) return templateCache[name];
    const resp = await fetch(`templates/${name}.json`);
    if (!resp.ok) return null;
    templateCache[name] = await resp.json();
    return templateCache[name];
}

function slugify(text) {
    return text.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/(^-|-$)/g, '');
}

/**
 * Generate dashboard data for a service section from a template.
 * Mirrors the Rust `service::generate()` logic.
 */
function generateServiceDashboard(template) {
    const groupMap = new Map();
    const unavailable = [];

    for (const kpi of template.kpis) {
        if (kpi.available === false) {
            unavailable.push({ title: kpi.title, role: kpi.role, query: kpi.query });
            continue;
        }

        const role = kpi.role || 'other';
        if (!groupMap.has(role)) {
            groupMap.set(role, []);
        }

        const plotId = `kpi-${slugify(role)}-${slugify(kpi.title)}`;
        const plot = {
            data: [],
            opts: {
                title: kpi.title,
                id: plotId,
                type: kpi.type || 'gauge',
                format: {
                    unit_system: kpi.unit_system || null,
                    precision: 2,
                },
            },
            promql_query: kpi.query,
        };

        if (kpi.description) {
            plot.opts.description = kpi.description;
        }
        if (kpi.subtype) {
            plot.opts.subtype = kpi.subtype;
        }
        if (kpi.percentiles) {
            plot.opts.percentiles = kpi.percentiles;
        }

        groupMap.get(role).push(plot);
    }

    const groups = [];
    for (const [role, plots] of groupMap) {
        groups.push({
            name: role.charAt(0).toUpperCase() + role.slice(1),
            id: `kpi-${slugify(role)}`,
            plots,
        });
    }

    return {
        groups,
        metadata: {
            service_name: template.service_name,
            service_metadata: template.service_metadata || {},
            unavailable_kpis: unavailable,
        },
    };
}

/**
 * Detect which service templates match the loaded parquet file's metrics.
 * Returns array of { name, template } objects.
 */
function detectServices(viewerInfo) {
    const allMetrics = new Set([
        ...(viewerInfo.counter_names || []),
        ...(viewerInfo.gauge_names || []),
        ...(viewerInfo.histogram_names || []),
    ]);

    const results = [];
    for (const [name, template] of Object.entries(templateCache)) {
        // A template matches if any of its KPI queries reference metrics present in the file
        const hasMatch = template.kpis.some(kpi => {
            // Extract metric names from the query (strip functions, labels, etc.)
            const metrics = kpi.query.match(/[a-zA-Z_][a-zA-Z0-9_]*/g) || [];
            return metrics.some(m => allMetrics.has(m));
        });
        if (hasMatch) {
            results.push({ name, template });
        }
    }
    return results;
}

// Preload all known templates at module init time
const templateLoadPromise = Promise.all(KNOWN_TEMPLATES.map(loadTemplate));

// Cached detected services per viewerInfo identity
let detectedServicesCache = null;
let detectedServicesKey = null;

async function getDetectedServices(viewerInfo) {
    await templateLoadPromise;

    const key = viewerInfo.filename || '';
    if (detectedServicesCache && detectedServicesKey === key) {
        return detectedServicesCache;
    }

    detectedServicesCache = detectServices(viewerInfo);
    detectedServicesKey = key;
    return detectedServicesCache;
}

/**
 * Generate a section's View-compatible data structure.
 * Loads pre-generated JSON and merges in runtime viewer info.
 */
export async function generateSectionData(sectionKey, viewerInfo) {
    const runtimeFields = {
        interval: viewerInfo.interval,
        source: viewerInfo.source,
        version: viewerInfo.version,
        filename: viewerInfo.filename,
        start_time: viewerInfo.minTime * 1000,
        end_time: viewerInfo.maxTime * 1000,
        num_series: (viewerInfo.counter_names?.length || 0) +
                    (viewerInfo.gauge_names?.length || 0) +
                    (viewerInfo.histogram_names?.length || 0),
    };

    // Build sections list with detected services appended
    const [baseSections, services] = await Promise.all([
        loadSections(),
        getDetectedServices(viewerInfo),
    ]);

    const sections = [
        ...baseSections,
        ...services.map(s => ({
            name: s.template.service_name,
            route: `/service/${s.name}`,
        })),
    ];

    // Handle service section requests
    if (sectionKey.startsWith('service/')) {
        const serviceName = sectionKey.replace('service/', '');
        const svc = services.find(s => s.name === serviceName);
        if (!svc) throw new Error(`Unknown service: ${serviceName}`);

        const dashboard = generateServiceDashboard(svc.template);
        return { ...dashboard, sections, ...runtimeFields };
    }

    // Regular dashboard section
    const dashboard = await loadDashboard(sectionKey);
    return { ...dashboard, sections, ...runtimeFields };
}
