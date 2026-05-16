// <rezolus-chart> — Lit web component that renders a single Plot
// descriptor (the shape produced by crates/dashboard/) inside Shadow DOM.
//
// It does NOT reimplement chart configuration. It mounts the viewer's
// real `Chart` mithril component (the same `configureChartByType`
// dispatcher the dashboard uses), so every style — line, scatter,
// heatmap, quantile, multi — renders byte-for-byte like the viewer:
// unit-aware axes/tooltip, fonts, gridlines, dataZoom, OOB bands, etc.
// Visual parity is structural, not chased.
//
// Card chrome (frame, title, sizing) comes from /lib/charts.css linked
// into the shadow root. The viewer's COLORS read CSS tokens from
// document.documentElement, so as long as the host page carries the
// rezolus tokens (link /lib/style.css) chrome and chart contents theme
// correctly. A data-theme MutationObserver remounts so a theme flip
// re-reads the tokens (echarts bakes colors at setOption time).
//
// Requires, loaded as globals on the host page (the viewer provides
// all of these): echarts (/lib/charts/echarts.min.js) and mithril
// (/lib/mithril.js, which sets window.m). `plot` is a descriptor whose
// `data` field is the viewer spec shape (e.g. [timestamps_s, values]
// for a single line).
import { LitElement, html, css } from '../lit/lit.js';
import { Chart, ChartsState } from '../charts/chart.js';

// One throwaway ChartsState shared by all embedded charts on the page.
// It only provides the zoom/cursor registry the Chart lifecycle needs;
// nothing here drives cross-chart sync, so a single bare instance is
// fine (Chart de-registers itself on unmount via its onremove).
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
        /* charts.css collapses wrappers whose .chart has .no-data so
           section grids fold; for embed show a visible placeholder. */
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
        // A theme flip changes the CSS tokens; echarts baked the old
        // colors at setOption time, so remount to re-read them.
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
        // m.mount(host, null) triggers Chart.onremove → echarts dispose
        // + observer/listener cleanup + chartsState de-registration.
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

        // Remount fresh so the new descriptor goes through the exact
        // viewer construction path (Chart reads spec on construct, then
        // configureChartByType dispatches by resolved style).
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
