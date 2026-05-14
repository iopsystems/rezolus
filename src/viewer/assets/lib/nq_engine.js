// engine.js — transforms.js model loading, embedding, metric index

let pipeline = null;
let metricIndex = null; // Array of { name, type, vector }
let isLoaded = false;
let loadPromise = null;

/**
 * Lazy-load the transformers pipeline and build the metric embedding index.
 * Models are cached in IndexedDB by transformers.js.
 */
async function load() {
    if (isLoaded) return;
    if (loadPromise) return loadPromise;

    loadPromise = (async () => {
        const { pipeline, env } = await import(
            'https://cdn.jsdelivr.net/npm/@xenova/transformers@2.17.2'
        );

        // Use WebGPU backend if available, fallback to wasm
        env.backends.onnx.wasm.numThreads = 1;

        // Load the embedding model (feature-extraction)
        const extractor = await pipeline(
            'feature-extraction',
            'Xenova/all-MiniLM-L6-v2',
            { quantized: true }
        );

        pipeline = extractor;
        isLoaded = true;
        loadPromise = null;
    })();

    return loadPromise;
}

/**
 * Encode text into a 384-dim embedding vector.
 * Accepts a single string or an array of strings (batch).
 */
async function encode(texts) {
    if (!pipeline) await load();

    const output = await pipeline(texts, {
        pooling: 'mean',
        normalize: true,
        return_tensors: false,
    });

    // output is a Float32Array for single string, or array of Float32Array for batch
    if (Array.isArray(texts)) {
        return output.data ? [output] : output;
    }
    return output;
}

/**
 * Build the metric embedding index from metric names and types.
 * Called once after models are loaded and metrics are enumerated.
 */
export async function buildIndex(metricNames, metricTypes) {
    if (!pipeline) await load();

    // Build text representations: "metric_name counter"
    const texts = metricNames.map(name => `${name} ${metricTypes?.[name] || ''}`);

    const embeddings = await encode(texts);

    // Convert to array of Float32Array
    const vectors = Array.isArray(embeddings.data)
        ? embeddings.data
        : [embeddings];

    metricIndex = metricNames.map((name, i) => ({
        name,
        type: metricTypes?.[name] || '',
        vector: vectors[i] || vectors[vectors.length - 1],
    }));
}

/**
 * Get the current metric embedding index.
 */
export function getIndex() {
    return metricIndex;
}

/**
 * Encode a user query into a vector for similarity search.
 */
export async function queryEmbed(query) {
    if (!pipeline) await load();
    const output = await pipeline(query, {
        pooling: 'mean',
        normalize: true,
        return_tensors: false,
    });
    return output;
}

/**
 * Check if the engine is loaded and ready.
 */
export function isReady() {
    return isLoaded && metricIndex !== null;
}

/**
 * Reset the engine state (for garbage collection).
 */
export function reset() {
    pipeline = null;
    metricIndex = null;
    isLoaded = false;
    loadPromise = null;
}
