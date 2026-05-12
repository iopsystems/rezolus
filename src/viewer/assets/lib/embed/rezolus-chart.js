// <rezolus-chart> — Lit web component that renders a single Plot
// descriptor (the shape produced by crates/dashboard/) inside Shadow DOM.
//
// Spike scope: inline-series adapter only. The component accepts a `plot`
// property whose `data` field is [[timestamps_ms], [values]]. Future
// adapters (HTTP, WASM, SSE) will live alongside this file and feed the
// same property contract.
//
// Requires echarts to be loaded as a global (see lib/charts/echarts.min.js).
import { LitElement, html, css } from '../lit/lit.js';

class RezolusChart extends LitElement {
    static properties = {
        plot: { type: Object },
    };

    static styles = css`
        :host {
            display: block;
            width: 100%;
            height: 280px;
            font-family: 'Inter', system-ui, sans-serif;
            color: #222;
        }
        .title {
            margin: 0 0 0.4rem;
            font-size: 14px;
            font-weight: 600;
        }
        .canvas {
            position: relative;
            width: 100%;
            height: calc(100% - 1.8rem);
        }
        .empty {
            display: flex;
            align-items: center;
            justify-content: center;
            height: 100%;
            color: #888;
            font-style: italic;
            font-size: 13px;
        }
    `;

    constructor() {
        super();
        this.plot = null;
        this._chart = null;
        this._observer = null;
    }

    firstUpdated() {
        const canvas = this.renderRoot.querySelector('.canvas');
        this._observer = new ResizeObserver(() => this._chart?.resize());
        this._observer.observe(canvas);
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
        const canvas = this.renderRoot.querySelector('.canvas');
        if (!canvas) return;

        const data = this.plot?.data;
        const hasData = Array.isArray(data) && data.length >= 2
            && Array.isArray(data[0]) && data[0].length > 0;

        if (!hasData) {
            this._chart?.dispose();
            this._chart = null;
            return;
        }

        if (typeof window.echarts === 'undefined') {
            canvas.textContent = 'echarts not loaded';
            return;
        }

        if (!this._chart) {
            this._chart = window.echarts.init(canvas, null, { renderer: 'canvas' });
        }

        const [times, values] = data;
        // PromQL convention: timestamps are seconds since epoch. echarts wants ms.
        const seriesData = times.map((t, i) => [t * 1000, values[i]]);
        const fmt = this.plot.opts?.format ?? {};

        this._chart.setOption({
            grid: { left: 56, right: 16, top: 12, bottom: 28 },
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
            ${title ? html`<h3 class="title">${title}</h3>` : ''}
            <div class="canvas">
                ${!hasData ? html`<div class="empty">no data</div>` : ''}
            </div>
        `;
    }
}

customElements.define('rezolus-chart', RezolusChart);

export { RezolusChart };
