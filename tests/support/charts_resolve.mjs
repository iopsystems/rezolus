// The resolve hook tests/support/charts.mjs registers: a relative or absolute
// import whose target is under src/viewer/assets/lib/charts/, or is
// src/viewer/assets/lib/charts.css, goes to the same path under the
// metriken-charts crate's assets/ directory.
let lib;
let assets;

export function initialize(data) {
    lib = data.lib;
    assets = data.assets;
}

export async function resolve(specifier, context, nextResolve) {
    if (context.parentURL && /^\.{0,2}\//.test(specifier)) {
        const target = new URL(specifier, context.parentURL).href;
        if (target.startsWith(lib + 'charts/') || target === lib + 'charts.css') {
            return nextResolve(assets + target.slice(lib.length), context);
        }
    }
    return nextResolve(specifier, context);
}
