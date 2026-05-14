// search.js — cosine similarity search over metric embedding index

import { getIndex } from './engine.js';

/**
 * Compute cosine similarity between query vector and all indexed vectors.
 * Returns sorted array of { name, type, score } descending by score.
 */
export function search(queryVector, topK = 5) {
    const index = getIndex();
    if (!index || index.length === 0) return [];

    const scores = index.map((entry) => {
        const q = queryVector;
        const v = entry.vector;
        let dot = 0;
        let qNorm = 0;
        let vNorm = 0;
        const len = Math.min(q.length, v.length);
        for (let i = 0; i < len; i++) {
            dot += q[i] * v[i];
            qNorm += q[i] * q[i];
            vNorm += v[i] * v[i];
        }
        qNorm = Math.sqrt(qNorm);
        vNorm = Math.sqrt(vNorm);
        const score = (qNorm * vNorm) > 0 ? dot / (qNorm * vNorm) : 0;
        return { name: entry.name, type: entry.type, score };
    });

    scores.sort((a, b) => b.score - a.score);
    return scores.slice(0, topK);
}

/**
 * Fallback keyword search when embeddings return no meaningful results.
 * Simple substring matching against metric names.
 */
export function keywordSearch(query, topK = 5) {
    const index = getIndex();
    if (!index || index.length === 0) return [];

    const terms = query.toLowerCase().split(/\s+/).filter(t => t.length > 2);
    const scores = index.map((entry) => {
        const lowerName = entry.name.toLowerCase();
        let score = 0;
        for (const term of terms) {
            if (lowerName.includes(term)) score += 1;
            // Also check type hint
            if (entry.type && lowerName.includes(entry.type)) score += 0.5;
        }
        return { name: entry.name, type: entry.type, score };
    });

    scores.sort((a, b) => b.score - a.score);
    return scores.filter(s => s.score > 0).slice(0, topK);
}
