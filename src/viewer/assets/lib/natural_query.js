// natural_query.js — Mithril component for the Natural Query tab

import { runPipeline } from './nq_pipeline.js';
import { renderQueryChart } from './explorers.js';
import { ChartsState } from './charts/chart.js';

// Status states
const STATUS_IDLE = 'idle';
const STATUS_LOADING = 'loading';
const STATUS_EMBEDDING = 'embedding';
const STATUS_GENERATING = 'generating';
const STATUS_RESULT = 'result';
const STATUS_ERROR = 'error';

function statusLabel(status) {
    switch (status) {
        case STATUS_LOADING: return 'Loading models...';
        case STATUS_EMBEDDING: return 'Building metrics index...';
        case STATUS_GENERATING: return 'Generating query...';
        default: return '';
    }
}

export const NaturalQuery = {
    oninit(vnode) {
        vnode.state.status = STATUS_IDLE;
        vnode.state.query = '';
        vnode.state.result = null;
        vnode.state.error = null;
        vnode.state.loading = false;
        vnode.state.promql = '';
        vnode.state.rawOutput = '';
        vnode.state.chartsState = new ChartsState();
        vnode.state.editMode = false;
    },

    oncreate(vnode) {
        // Check WebGPU support on mount
        if (!navigator.gpu) {
            vnode.state.error = 'WebGPU not supported — NL queries require a modern browser with WebGPU.';
            vnode.state.status = STATUS_ERROR;
        }
    },

    view(vnode) {
        const st = vnode.state;

        return m('div.natural-query', [
            // Status banner
            (st.status !== STATUS_IDLE && st.status !== STATUS_RESULT && st.status !== STATUS_ERROR) && m('div.query-status', [
                m('span.status-spinner', st.status === STATUS_LOADING ? '◐' :
                    st.status === STATUS_EMBEDDING ? '◑' :
                    st.status === STATUS_GENERATING ? '◒' : '◐'),
                ' ' + statusLabel(st.status),
            ]),

            // Error state
            st.status === STATUS_ERROR && m('div.error-message', [
                m('strong', 'Error: '), st.error,
                m('button.retry-btn', {
                    onclick: () => { st.status = STATUS_IDLE; st.error = null; }
                }, 'Retry'),
            ]),

            // Input section
            m('div.query-input-section', [
                m('h2', 'Natural Language Query'),
                m('div.query-input-wrapper', [
                    m('input.natural-query-input', {
                        type: 'text',
                        placeholder: 'e.g. "show me cpu usage over time"',
                        value: st.query,
                        oninput: (e) => { st.query = e.target.value; },
                        onkeydown: (e) => {
                            if (e.key === 'Enter' && !e.shiftKey) {
                                e.preventDefault();
                                st.executeQuery();
                            }
                        },
                        disabled: st.loading,
                    }),
                    m('button.execute-btn', {
                        onclick: () => st.executeQuery(),
                        disabled: st.loading || !st.query.trim(),
                    }, st.loading ? 'Running...' : 'Execute'),
                ]),
            ]),

            // Result section
            st.status === STATUS_RESULT && st.result && m('div.query-result', [
                m('h3', 'Generated PromQL'),
                m('div.promql-display', [
                    m('code', st.promql),
                    m('button.copy-btn', {
                        onclick: () => {
                            navigator.clipboard.writeText(st.promql);
                        }
                    }, 'Copy'),
                    m('button.edit-btn', {
                        onclick: () => { st.editMode = true; }
                    }, 'Edit'),
                ]),
                st.editMode && m('div.promql-edit', [
                    m('textarea.promql-edit-input', {
                        value: st.promql,
                        oninput: (e) => { st.promql = e.target.value; },
                        rows: 3,
                    }),
                    m('button.apply-edit-btn', {
                        onclick: () => st.executeEditedPromQL()
                    }, 'Apply & Run'),
                ]),
                m('div.chart-container', [
                    renderQueryChart(
                        st.result.data?.result,
                        st.promql,
                        st.chartsState,
                        undefined,
                    ),
                ]),
            ]),

            // Loading models banner
            st.status === STATUS_LOADING && m('div.model-loading', [
                m('p', 'Loading AI models (first time ~1GB, cached afterwards)...'),
                m('div.progress-bar', m('div.progress-fill.indeterminate')),
            ]),
        ]);
    },

    executeQuery() {
        const st = this.state;
        if (!st.query.trim()) return;

        st.loading = true;
        st.error = null;
        st.status = STATUS_LOADING;
        st.result = null;
        st.promql = '';
        m.redraw();

        // Yield so the browser can render the status before heavy work
        Promise.resolve().then(() => {
            st.status = STATUS_EMBEDDING;
            m.redraw();
        });

        const mapStatus = (msg) => {
            if (msg.includes('Loading')) st.status = STATUS_LOADING;
            else if (msg.includes('Building')) st.status = STATUS_EMBEDDING;
            else if (msg.includes('Generating')) st.status = STATUS_GENERATING;
        };

        runPipeline(st.query, { onStatus: mapStatus })
            .then((result) => {
                st.status = STATUS_RESULT;
                st.result = result.data;
                st.promql = result.promql;
                st.rawOutput = result.raw;
                st.loading = false;
            })
            .catch((error) => {
                st.status = STATUS_ERROR;
                st.error = error.message || 'Pipeline failed';
                st.loading = false;
            })
            .then(() => { m.redraw(); });
    },

    executeEditedPromQL() {
        // Hand off the edited PromQL to the existing chart render path
        const st = this.state;
        st.loading = true;
        st.status = STATUS_GENERATING;
        m.redraw();

        // Re-run with the edited query through the pipeline
        st.executeQuery();
    },
};
