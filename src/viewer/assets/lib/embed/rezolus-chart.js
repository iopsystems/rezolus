// <rezolus-chart> — Lit web component that renders a single Plot
// descriptor (the shape produced by crates/dashboard/) inside Shadow DOM.
//
// Chrome (card frame, title typography, sizing) comes from
// /lib/charts.css linked into the shadow root, so the component looks
// like a regular rezolus chart when embedded inside the viewer. CSS
// custom properties inherited from the host page's :root (--fg,
// --accent, --bg-card-gradient, etc.) reach into the shadow root
// automatically. External embedders need to load the same tokens (or
// override them per :host) for the chrome to render correctly.
//
// Spike scope: inline-series adapter only. The component accepts a `plot`
// property whose `data` field is [[timestamps_s], [values]] (PromQL's
// seconds-since-epoch convention). Future adapters (HTTP, WASM, SSE)
// will live alongside this file and feed the same property contract.
//
// Requires echarts to be loaded as a global (see lib/charts/echarts.min.js).
import { LitElement, html, css } from '../lit/lit.js';

class RezolusChart extends LitElement {
    static properties = {
        plot: { type: Object },
    };

    // Component-local styles. The :host prefix on the overrides bumps
    // specificity above charts.css so they win without !important
    // (except for the `> div` selector, which charts.css already marks
    // !important — must match).
    static styles = css`
        :host {
            display: block;
            min-width: 0;
        }
        /* charts.css collapses chart wrappers whose .chart has .no-data
           so section grids fold around missing data. For embed we want a
           visible placeholder so the consumer can see the slot exists. */
        :host .chart-wrapper:has(.no-data) {
            display: block;
        }
        :host .chart.no-data {
            height: 270px;
            border: none;
            background: none;
            display: flex;
            align-items: center;
            justify-content: center;
            color: var(--fg-muted, #888);
            font-style: italic;
            font-size: 13px;
        }
        :host .chart.no-data > div {
            display: block !important;
        }
    `;

    constructor() {
        super();
        this.plot = null;
        this._chart = null;
        this._observer = null;
    }

    firstUpdated() {
        const chartEl = this.renderRoot.querySelector('.chart');
        this._observer = new ResizeObserver(() => this._chart?.resize());
        if (chartEl) this._observer.observe(chartEl);
        this._render();
    }

    updated(changed) {
        if (changed.has('plot')) this._render();
    }

    disconnectedCallback() {
        super.disconnectedCallback();
        this._observer?.disconnect();
        this._chart?.dispose();
        this._chart = null;
    }

    _render() {
        const chartEl = this.renderRoot.querySelector('.chart');
        if (!chartEl) return;

        const data = this.plot?.data;
        const hasData = Array.isArray(data) && data.length >= 2
            && Array.isArray(data[0]) && data[0].length > 0;

        if (!hasData) {
            this._chart?.dispose();
            this._chart = null;
            return;
        }

        if (typeof window.echarts === 'undefined') {
            chartEl.textContent = 'echarts not loaded';
            return;
        }

        if (!this._chart) {
            this._chart = window.echarts.init(chartEl, null, { renderer: 'canvas' });
        }

        const [times, values] = data;
        // PromQL convention: timestamps are seconds since epoch. echarts wants ms.
        const seriesData = times.map((t, i) => [t * 1000, values[i]]);
        const fmt = this.plot.opts?.format ?? {};

        this._chart.setOption({
            // Top is large enough to clear the absolutely-positioned
            // .chart-header (10px padding + 13px title with line-height).
            grid: { left: 56, right: 16, top: 36, bottom: 28 },
            xAxis: { type: 'time' },
            yAxis: {
                type: fmt.log_scale ? 'log' : 'value',
                name: fmt.y_axis_label ?? '',
                nameTextStyle: { fontSize: 11 },
            },
            tooltip: { trigger: 'axis', appendToBody: false },
            series: [{
                name: this.plot.opts?.title ?? '',
                type: 'line',
                showSymbol: false,
                data: seriesData,
            }],
        }, { notMerge: true });
    }

    render() {
        const title = this.plot?.opts?.title;
        const hasData = this.plot?.data?.[0]?.length > 0;
        return html`
            <link rel="stylesheet" href="/lib/charts.css">
            <div class="chart-wrapper">
                ${title ? html`
                    <div class="chart-header">
                        <div class="chart-title-row">
                            <h3 class="chart-title">${title}</h3>
                        </div>
                    </div>
                ` : ''}
                <div class="chart ${hasData ? '' : 'no-data'}">
                    ${!hasData ? html`<div>no data</div>` : ''}
                </div>
            </div>
        `;
    }
}

customElements.define('rezolus-chart', RezolusChart);

export { RezolusChart };
