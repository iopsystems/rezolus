// <rezolus-chart> — embeds one dashboard chart in shadow DOM.
//
// Mounts the viewer's real `Chart` (same configureChartByType path)
// instead of reimplementing config, so every style renders identically.
// Needs echarts + mithril (window.m) as host globals and the rezolus
// CSS tokens on document.documentElement (link /lib/style.css), which
// COLORS reads. `plot.data` is the viewer spec shape, e.g.
// [timestamps_s, values].
import { LitElement, html, css } from '../lit/lit.js';
import { Chart, ChartsState } from '../charts/chart.js';

// Shared, inert: only supplies the zoom/cursor registry Chart's
// lifecycle needs; no cross-chart sync is driven here.
const embedChartsState = new ChartsState();

class RezolusChart extends LitElement {
    static properties = {
        plot: { type: Object },
        // Sampling interval (s) — only feeds the Chart's min-zoom calc.
        interval: { type: Number },
    };

    static styles = css`
        :host {
            display: block;
            min-width: 0;
        }
        /* charts.css collapses .no-data wrappers; embed shows a placeholder. */
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
        this.interval = 1;
        this._mountHost = null;
        this._mounted = false;
        this._themeObserver = null;
    }

    connectedCallback() {
        super.connectedCallback();
        // echarts bakes colors at setOption; remount to re-read tokens.
        this._themeObserver = new MutationObserver(() => this._remount());
        this._themeObserver.observe(document.documentElement, {
            attributes: true,
            attributeFilter: ['data-theme'],
        });
    }

    firstUpdated() {
        this._mountHost = this.renderRoot.querySelector('.rz-chart-host');
        this._remount();
    }

    updated(changed) {
        if (changed.has('plot') || changed.has('interval')) this._remount();
    }

    disconnectedCallback() {
        super.disconnectedCallback();
        this._themeObserver?.disconnect();
        this._unmount();
    }

    _unmount() {
        // null mount → Chart.onremove (dispose + cleanup + de-register).
        if (this._mounted && this._mountHost && window.m) {
            window.m.mount(this._mountHost, null);
        }
        this._mounted = false;
    }

    _remount() {
        if (!this._mountHost) return;
        const m = window.m;
        if (!m) { this._mountHost.textContent = 'mithril not loaded'; return; }
        if (typeof window.echarts === 'undefined') {
            this._mountHost.textContent = 'echarts not loaded';
            return;
        }

        const spec = this.plot;
        const hasData = Array.isArray(spec?.data) && spec.data.length >= 2
            && Array.isArray(spec.data[0]) && spec.data[0].length > 0;
        if (!spec || !spec.opts || !hasData) {
            this._unmount();
            this._mountHost.innerHTML =
                '<div class="chart no-data"><div>no data</div></div>';
            return;
        }

        // Remount fresh: Chart reads spec on construct.
        this._unmount();
        const interval = this.interval;
        window.m.mount(this._mountHost, {
            view: () => m(Chart, { spec, chartsState: embedChartsState, interval }),
        });
        this._mounted = true;
    }

    render() {
        const title = this.plot?.opts?.title;
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
                <div class="rz-chart-host"></div>
            </div>
        `;
    }
}

customElements.define('rezolus-chart', RezolusChart);

export { RezolusChart };
