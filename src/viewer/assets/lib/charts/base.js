import {
    createAxisLabelFormatter,
} from './util/units.js';
import { formatDateTime } from './util/utils.js';
import { COLORS, CHART_PALETTE } from './util/colormap.js';
import { FONTS } from './util/fonts.js';

function isDarkTheme() {
    return document.documentElement.getAttribute('data-theme') !== 'light';
}

// Shared x-axis time label format used by all chart types
export const TIME_AXIS_FORMATTER = {
    year: '{yyyy}',
    month: '{MMM}',
    day: '{d}',
    hour: '{HH}:{mm}',
    minute: '{HH}:{mm}',
    second: '{HH}:{mm}:{ss}',
    millisecond: '{hh}:{mm}:{ss}.{SSS}',
    none: '{hh}:{mm}:{ss}.{SSS}',
};

/**
 * Apply a no-data placeholder to a chart.
 *
 * By default, collapses the chart to a compact 56px bar with a muted title.
 * When `chart.spec.noCollapse` is set (e.g. cgroups section where data
 * arrives after user selection), shows only the title at full height instead.
 */
export function applyNoData(chart) {
    if (chart.spec.noCollapse) {
        chart.echart.setOption({ backgroundColor: 'transparent' }, { notMerge: true });
        return;
    }
    chart.echart.clear();
    chart.domNode.classList.add('no-data');
}

/**
 * Tooltip freeze footer HTML. Shows current freeze state and hint.
 */
export function getTooltipFreezeFooter(chart) {
    const frozen = chart && chart._tooltipFrozen;
    const text = frozen ? 'FROZEN · click to unfreeze' : 'click to freeze';
    const color = frozen ? COLORS.accent : COLORS.fgMuted;
    return `<div class="tooltip-freeze-footer" style="border-top: 1px solid ${COLORS.borderMuted}; margin-top: 6px; padding-top: 4px; margin-bottom: -6px; font-size: ${FONTS.footnote.fontSize}px; color: ${color}; text-align: center;">
        ${text}
    </div>`;
}

/**
 * Approximates echarts' built-in tooltip formatter, but with our own x axis formatting
 * (using formatDateTime) and our own value formatting (using valueFormatter).
 */
export function getTooltipFormatter(valueFormatter, pinnedSet, chart, style) {
    return (paramsArray) => {
        const hasPins = pinnedSet && pinnedSet.size > 0;

        const sortedParams = [...paramsArray].sort((a, b) => {
            const aName = a.seriesName;
            const bName = b.seriesName;

            if (hasPins) {
                const aPinned = pinnedSet.has(aName);
                const bPinned = pinnedSet.has(bName);
                if (aPinned && !bPinned) return -1;
                if (!aPinned && bPinned) return 1;
            }

            const aHasId = aName.startsWith('id=');
            const bHasId = bName.startsWith('id=');

            if (aHasId && bHasId) {
                return aName.localeCompare(bName, undefined, { numeric: true });
            } else if (aHasId) {
                return -1;
            } else if (bHasId) {
                return 1;
            } else {
                // Descending so higher percentiles (p99.99) appear above lower (p50)
                return bName.localeCompare(aName, undefined, { numeric: true });
            }
        });

        const result =
            `<div style="${FONTS.cssSans}">
                <div style="${FONTS.cssMono} font-size: ${FONTS.tooltipTimestamp.fontSize}px; color: ${COLORS.fgSecondary}; margin-bottom: 8px;">
                    ${formatDateTime(paramsArray[0].value[0])}
                </div>
                <div style="display: flex; flex-direction: column; gap: 4px;">
                    ${sortedParams.map(p => {
                        const faded = hasPins && !pinnedSet.has(p.seriesName);
                        const isClamped = p.value[2] != null;
                        const nameColor = faded ? COLORS.fgMuted : COLORS.fgSecondary;
                        const opacity = faded ? 'opacity: 0.5;' : '';
                        let marker, valColor, displayValue;
                        if (style === 'scatter') {
                            // Scatter/percentile: red dot marker, red text, show raw value
                            valColor = faded ? COLORS.fgMuted : (isClamped ? COLORS.clamped : COLORS.fg);
                            marker = isClamped && !faded
                                ? `<span style="display:inline-block;margin-right:4px;border-radius:10px;width:10px;height:10px;background-color:${COLORS.clamped};"></span>`
                                : p.marker;
                            displayValue = valueFormatter(isClamped ? p.value[2] : p.value[1]);
                        } else {
                            // Line/multi: normal color, (raw: value) annotation
                            valColor = faded ? COLORS.fgMuted : COLORS.fg;
                            marker = p.marker;
                            displayValue = valueFormatter(p.value[1]);
                            if (isClamped) {
                                displayValue += ` <span style="color: ${COLORS.fgMuted};">(raw: ${valueFormatter(p.value[2])})</span>`;
                            }
                        }
                        return `<div style="display: flex; justify-content: space-between; align-items: center; gap: 16px; ${opacity}">
                        <span style="display: flex; align-items: center; gap: 6px;">
                            ${marker}
                            <span style="color: ${nameColor}; font-size: ${FONTS.tooltipLabel.fontSize}px;">${p.seriesName}</span>
                        </span>
                        <span style="${FONTS.cssMono} font-weight: ${FONTS.tooltipValue.fontWeight}; font-size: ${FONTS.tooltipValue.fontSize}px; color: ${valColor};">
                            ${displayValue}
                        </span>
                    </div>`;
                    }).join('')}
                </div>
                ${getTooltipFreezeFooter(chart)}
            </div>`;

        return result;
    }
}

export function getBaseOption() {
    return {
        grid: {
            left: '12',
            right: '17',
            top: '62',
            bottom: '24',
            containLabel: true,
        },
        xAxis: {
            type: 'time',
            min: 'dataMin',
            max: 'dataMax',
            splitNumber: 5,
            axisLine: { show: false },
            axisTick: { show: false },
            axisLabel: {
                color: COLORS.fgSecondary,
                ...FONTS.axisLabel,
                formatter: TIME_AXIS_FORMATTER,
            },
            splitLine: {
                show: true,
                lineStyle: {
                    color: COLORS.gridLine,
                    type: 'dashed',
                }
            },
        },
        tooltip: {
            trigger: 'axis',
            confine: true,
            axisPointer: {
                type: 'line',
                snap: true,
                animation: false,
                lineStyle: {
                    color: COLORS.accent,
                    opacity: 0.6,
                    width: 1,
                },
                label: {
                    backgroundColor: COLORS.bgCard,
                    borderColor: COLORS.borderSubtle,
                    color: COLORS.fg,
                    ...FONTS.axisLabel,
                }
            },
            textStyle: {
                color: COLORS.fg,
                ...FONTS.tooltipBody,
            },
            backgroundColor: COLORS.bgCard,
            borderColor: COLORS.borderDefault,
            borderWidth: 1,
            padding: [12, 14],
            extraCssText: `background-color: ${COLORS.bgCard} !important; box-shadow: 0 8px 24px ${COLORS.shadow}; border-radius: 8px;`,
        },
        // Invisible toolbox workaround for drag-to-zoom as default behavior.
        // https://github.com/apache/echarts/issues/13397#issuecomment-814864873
        toolbox: {
            orient: 'vertical',
            itemSize: 13,
            top: 15,
            right: -6,
            feature: {
                dataZoom: {
                    yAxisIndex: 'none',
                    icon: {
                        zoom: 'path://',
                        back: 'path://',
                    },
                },
            },
        },
        textStyle: {
            color: COLORS.fg,
            fontFamily: FONTS.sans,
        },
        darkMode: isDarkTheme(),
        backgroundColor: 'transparent',
        color: CHART_PALETTE,
    };
}

export function getBaseYAxisOption(logScale, unitSystem) {
    return {
        type: logScale ? 'log' : 'value',
        logBase: 10,
        scale: true,
        min: null,
        max: null,
        axisLine: { show: false },
        axisTick: { show: false },
        axisLabel: {
            color: COLORS.fgSecondary,
            ...FONTS.axisLabel,
            margin: 12,
            formatter: unitSystem ?
                createAxisLabelFormatter(unitSystem) :
                function (value) {
                    if (logScale && Math.abs(value) >= 1000) {
                        return value.toExponential(0);
                    }
                    if (Math.abs(value) > 10000 || (Math.abs(value) > 0 && Math.abs(value) < 0.01)) {
                        return value.toExponential(1);
                    }
                    return value;
                }
        },
        splitLine: {
            lineStyle: {
                color: COLORS.gridLine,
                type: 'dashed',
            }
        }
    };
}

/**
 * Build the right-aligned circle-swatch legend shared by scatter and
 * multi-series line overlays. Pass `{ tooltipFormatter }` when the
 * legend should show hover tooltips (scatter); omit it for bare line
 * overlays. The caller is responsible for pairing this with
 * `grid: { ..., top: '71' }` on the plot's option.
 */
export function buildOverlayLegendOption(names, { tooltipFormatter, top = '42' } = {}) {
    const legendItemW = 56;
    const textStyleBase = {
        color: COLORS.fgSecondary,
        ...FONTS.legend,
        width: legendItemW,
        borderColor: 'transparent',
        borderWidth: 1,
        borderRadius: 3,
        padding: [2, 4],
    };
    const legend = {
        show: true,
        right: '16',
        top,
        icon: 'circle',
        itemWidth: 8,
        itemHeight: 8,
        itemGap: 0,
        formatter: (name) => name.padEnd(6),
        textStyle: textStyleBase,
        data: names.map((name) => ({
            name,
            itemStyle: { borderColor: 'transparent', borderWidth: 2 },
            textStyle: {
                color: COLORS.fgSecondary,
                backgroundColor: 'transparent',
                borderColor: 'transparent',
                borderWidth: 1,
                borderRadius: 3,
                padding: [2, 4],
                width: legendItemW,
            },
        })),
    };
    if (tooltipFormatter) {
        legend.tooltip = { show: true, formatter: tooltipFormatter };
    }
    return legend;
}

/**
 * Calculate the minimum zoom span (as a percentage of total duration)
 * to prevent zooming tighter than 5x the sample interval.
 */
export function calculateMinZoomSpan(timeData) {
    if (!timeData || timeData.length < 2) return 0.1;
    const sampleInterval = timeData[1] - timeData[0];
    const totalDuration = timeData[timeData.length - 1] - timeData[0];
    return Math.max(0.1, (sampleInterval * 5 / totalDuration) * 100);
}

/**
 * Standard dataZoom config for charts with a time x-axis.
 */
export function getDataZoomConfig(minZoomSpan) {
    return [{
        type: 'slider',
        show: false,
        xAxisIndex: 0,
        minSpan: minZoomSpan,
        filterMode: 'none',
    }];
}

/**
 * Apply a chart option with notMerge and re-enable drag-to-zoom.
 */
export function applyChartOption(chart, option) {
    chart.domNode.classList.remove('no-data');
    chart.echart.setOption(option, { notMerge: true });
    chart.echart.dispatchAction({
        type: 'takeGlobalCursor',
        key: 'dataZoomSelect',
        dataZoomSelectActive: true,
    });
}

// Re-export for convenience — chart modules import these from base.js
export { COLORS, CHART_PALETTE, FONTS };
