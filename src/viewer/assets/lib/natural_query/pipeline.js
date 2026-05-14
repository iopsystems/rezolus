// pipeline.js — orchestrates embed → search → generate → TSDB query

import { queryEmbed, buildIndex, isReady as engineReady, reset as resetEngine } from './engine.js';
import { search, keywordSearch } from './search.js';
import { generate as llmGenerate, isReady as llmReady, reset as resetLlm } from './generate.js';
import { buildPrompt, cleanOutput, looksLikePromQL } from './prompt.js';
import { getMetricNames, getMetricTypes, isMetricNamesLoaded, getSelectedNode } from '../data.js';
import { ViewerApi } from '../viewer_api.js';

const MAX_RETRIES = 2;

/**
 * Run the full NL→PromQL pipeline.
 * @param {string} nlQuery - User's natural language question
 * @param {object} options
 * @returns {{ promql: string, raw: string, error?: string, data?: object }}
 */
export async function runPipeline(nlQuery, options = {}) {
    const { maxRetries = MAX_RETRIES } = options;

    // Wait for models to be ready
    if (!engineReady()) {
        throw new Error('Embedding engine not loaded');
    }
    if (!llmReady()) {
        throw new Error('LLM not loaded');
    }

    // Get metric names
    if (!isMetricNamesLoaded()) {
        throw new Error('Metrics not loaded — load a parquet file first');
    }

    const metricNames = getMetricNames();
    const metricTypes = getMetricTypes();
    const nodeName = getSelectedNode();

    // Build the embedding index (cached, idempotent)
    await buildIndex(metricNames, metricTypes);

    // Step 1: Embed the user query
    const queryVector = await queryEmbed(nlQuery);

    // Step 2: Similarity search
    let topK = search(queryVector, 5);

    // Fallback to keyword search if embeddings return nothing
    if (topK.length === 0) {
        topK = keywordSearch(nlQuery, 5);
    }

    if (topK.length === 0) {
        throw new Error('No matching metrics found. Try a different query.');
    }

    // Step 3: Build prompt and generate PromQL
    let rawOutput = '';
    let promql = '';
    let retries = 0;

    while (retries <= maxRetries) {
        const prompt = buildPrompt(topK, nlQuery, nodeName);
        rawOutput = await llmGenerate(prompt, {
            max_new_tokens: 256,
            temperature: 0.1,
        });

        promql = cleanOutput(rawOutput);

        if (looksLikePromQL(promql)) {
            break; // Valid PromQL
        }

        retries++;
    }

    if (!looksLikePromQL(promql)) {
        throw new Error(`Failed to generate valid PromQL. Raw output: ${rawOutput}`);
    }

    // Step 4: Execute the PromQL via existing TSDB path
    try {
        const data = await ViewerApi.queryRange(promql, 0, 3600, 1);
        return { promql, raw: rawOutput, data };
    } catch (e) {
        throw new Error(`Query error: ${e.message || 'unknown'}`);
    }
}

/**
 * Reset all pipeline modules (for garbage collection).
 */
export function reset() {
    resetEngine();
    resetLlm();
}
