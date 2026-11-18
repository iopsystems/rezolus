// The viewer's charts/ and charts.css are served from the metriken-charts
// crate and are not in this tree. Load this with
// `node --import ./tests/support/charts.mjs --test tests/*.mjs`: it resolves
// an import of src/viewer/assets/lib/charts/ or charts.css to the crate's
// assets/ directory, which `cargo metadata` locates in the registry (or in a
// `[patch]` path). The directory is found once and passed to the per-file
// test processes in METRIKEN_CHARTS_ASSETS.
import { execFileSync } from 'node:child_process';
import { registerHooks } from 'node:module';
import { dirname, join } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const ROOT = join(dirname(fileURLToPath(import.meta.url)), '../..');

if (!process.env.METRIKEN_CHARTS_ASSETS) {
    const meta = JSON.parse(execFileSync(
        'cargo', ['metadata', '--format-version', '1', '--locked'],
        { cwd: ROOT, maxBuffer: 256 << 20, encoding: 'utf8' },
    ));
    const pkg = meta.packages.find((p) => p.name === 'metriken-charts');
    if (!pkg) throw new Error('cargo metadata lists no metriken-charts package');
    process.env.METRIKEN_CHARTS_ASSETS = join(dirname(pkg.manifest_path), 'assets');
}

const LIB = pathToFileURL(join(ROOT, 'src/viewer/assets/lib')).href + '/';
const ASSETS = pathToFileURL(process.env.METRIKEN_CHARTS_ASSETS).href + '/';

registerHooks({
    resolve(specifier, context, nextResolve) {
        if (context.parentURL && /^\.{0,2}\//.test(specifier)) {
            const target = new URL(specifier, context.parentURL).href;
            if (target.startsWith(LIB + 'charts/') || target === LIB + 'charts.css') {
                return nextResolve(ASSETS + target.slice(LIB.length), context);
            }
        }
        return nextResolve(specifier, context);
    },
});
