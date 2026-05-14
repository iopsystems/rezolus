// prompt.js — prompt templates for NL→PromQL generation

const SYSTEM_PROMPT = `You are a PromQL query assistant. Given a user's natural language request
and a list of relevant metrics, generate the exact PromQL query to answer
their question. Only output the PromQL, nothing else. No explanations.
No markdown code fences. Just the raw PromQL query string.

Rules:
- Use rate()/irate() for counters (cumulative metrics)
- Use raw metric names for gauges
- Use histogram_quantile() for histograms
- Wrap metric names in {node="..."} if a node is specified
- Use sum() or avg() aggregations as appropriate`;

/**
 * Build the full prompt for LLM generation.
 */
export function buildPrompt(topKMetrics, userQuery, nodeName = null) {
    const metricList = topKMetrics
        .map(m => `${m.name} (${m.type})`)
        .join('\n');

    let prompt = `${SYSTEM_PROMPT}

Relevant metrics from the time-series database:
${metricList}

User request: ${userQuery}`;

    if (nodeName) {
        prompt += `\n\nFilter by node: ${nodeName}`;
    }

    prompt += '\n\nPromQL: ';
    return prompt;
}

/**
 * Clean the raw LLM output to extract just the PromQL query.
 */
export function cleanOutput(raw) {
    let result = raw.trim();

    // Strip markdown code fences if present
    result = result.replace(/^```(?:promql|PromQL|promql|text)?\s*/i, '').replace(/\s*```$/i, '');

    // Strip any leading "PromQL:" prefix the model might add
    result = result.replace(/^promql:\s*/i, '');

    return result.trim();
}

/**
 * Validate that the output looks like a PromQL query.
 * Returns true if it contains at least one metric-like identifier
 * and basic PromQL syntax elements.
 */
export function looksLikePromQL(query) {
    if (!query || query.length < 3) return false;
    // Must contain at least one letter/underscore sequence (metric name)
    if (!/[a-z_]\w*/i.test(query)) return false;
    // Should contain brackets [] or braces {} or a function call
    if (!(/\[|\{|\(/.test(query))) return false;
    return true;
}
