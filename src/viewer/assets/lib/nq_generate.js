// generate.js — GGUF LLM loading and text generation via transformers.js

let generator = null;
let isLoaded = false;
let loadPromise = null;

const DEFAULT_MODEL = 'Xenova/qwen2.5-1.5b-instruct';

/**
 * Lazy-load the GGUF LLM model.
 * Models are cached in IndexedDB by transformers.js.
 */
async function load() {
    if (isLoaded) return;
    if (loadPromise) return loadPromise;

    loadPromise = (async () => {
        const { pipeline } = await import(
            'https://cdn.jsdelivr.net/npm/@xenova/transformers@2.17.2'
        );

        generator = await pipeline(
            'text-generation',
            DEFAULT_MODEL,
            { quantized: true }
        );

        isLoaded = true;
        loadPromise = null;
    })();

    return loadPromise;
}

/**
 * Generate a PromQL query from the LLM.
 * @param {string} prompt - The full prompt with metric list and user query
 * @param {object} options - Generation options
 * @param {number} options.maxNewTokens - Max output tokens (default: 256)
 * @param {number} options.temperature - Sampling temperature (default: 0.1)
 * @returns {Promise<string>} The generated PromQL text
 */
export async function generate(prompt, options = {}) {
    if (!generator) await load();

    const { maxNewTokens = 256, temperature = 0.1 } = options;

    const response = await generator(prompt, {
        max_new_tokens: maxNewTokens,
        temperature,
        repetition_penalty: 1.1,
        truncate: false,
    });

    // Extract the generated text from the response
    const text = response[0]?.generated_text || prompt;

    // Return only the part after the last "PromQL: " prefix
    const lastPrefix = text.lastIndexOf('PromQL: ');
    if (lastPrefix >= 0) {
        return text.substring(lastPrefix + 8).trim();
    }

    // If no prefix found, return the generated portion
    return text.substring(prompt.length).trim();
}

/**
 * Check if the generator is loaded and ready.
 */
export function isReady() {
    return isLoaded && generator !== null;
}

/**
 * Reset the generator state (for garbage collection).
 */
export function reset() {
    generator = null;
    isLoaded = false;
    loadPromise = null;
}
