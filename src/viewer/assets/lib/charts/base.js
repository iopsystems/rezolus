import {
    createAxisLabelFormatter,
} from './util/units.js';
import { formatDateTime } from './util/utils.js';

/**
 * Approximates echarts' built-in tooltip formatter, but with our own x axis formatting
 * (using formatDateTime) and our own value formatting (using valueFormatter).
 * @param {function} valueFormatter - A function from raw value to formatted value.
 */
export function getTooltipFormatter(valueFormatter) {
    return (paramsArray) => {
        const result =
            `<div>
                <div>
                    ${formatDateTime(paramsArray[0].value[0])}
                </div>
                <div style="margin-top: 5px;">
                    ${paramsArray.map(p => `<div>
                        ${p.marker}
                        <span style="margin-left: 2px;">
                            ${p.seriesName}
                        </span>
                        <span style="float: right; margin-left: 20px; font-weight: bold;">
                            ${valueFormatter(p.value[1])}
                        </span>
                    </div>`).join('')}
                </div>
            </div>`;

        return result;
    }
}

export function getBaseOption(title) {
    return {
        grid: {
            left: '14%',
            right: '5%',
            // Subtracting from the element height, these give 384px height for the chart itself.
            top: '35',
            bottom: '35',
            containLabel: false,
        },
        xAxis: {
            type: 'time',
            min: 'dataMin',
            max: 'dataMax',
            // splitNumber appears to control the MINIMUM number of ticks. The max number is much higher.
            // This value is lowered from the default of 5 in order to reduce the max number of ticks,
            // which cause visual overlap of labels. It feels like this shouldn't be necessary.
            // Testing showed that their "automatic" determination of how many ticks fit is independent
            // of the size of the chart. So this value is trying to be empirically correct for charts of
            // a reasonable size (which is dependent on the size of the window).
            // TODO: should we adjust split number based on the size of the window? Or take x axis labels
            // into our own hands?
            splitNumber: 4,
            axisLine: {
                lineStyle: {
                    color: '#ABABAB'
                }
            },
            axisLabel: {
                color: '#ABABAB',
                formatter: {
                    year: '{yyyy}',
                    month: '{MMM}',
                    day: '{d}',
                    hour: '{HH}:{mm}',
                    minute: '{HH}:{mm}',
                    second: '{HH}:{mm}:{ss}',
                    millisecond: '{hh}:{mm}:{ss}.{SSS}',
                    none: '{hh}:{mm}:{ss}.{SSS}'
                }
            },
        },
        tooltip: {
            trigger: 'axis',
            axisPointer: {
                type: 'line',
                snap: true,
                animation: false,
                label: {
                    backgroundColor: '#505765'
                }
            },
            textStyle: {
                color: '#E0E0E0'
            },
            backgroundColor: 'rgba(50, 50, 50, 0.8)',
            borderColor: 'rgba(70, 70, 70, 0.8)',
        },
        // This invisible toolbox is a workaround to have drag-to-zoom as the default behavior.
        // We programmatically activate the zoom tool and hide the interface.
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
                        zoom: 'path://', // hack to remove zoom button
                        back: 'path://', // hack to remove restore button
                    },
                },
            },
        },
        title: {
            text: title,
            left: 'center',
            textStyle: {
                color: '#E0E0E0'
            }
        },
        textStyle: {
            color: '#E0E0E0'
        },
        darkMode: true,
        backgroundColor: 'transparent'
    };
}

export function getBaseYAxisOption(logScale, minValue, maxValue, unitSystem) {
    return {
        type: logScale ? 'log' : 'value',
        logBase: 10,
        scale: true,
        min: minValue,
        max: maxValue,
        axisLine: {
            lineStyle: {
                color: '#ABABAB'
            }
        },
        axisLabel: {
            color: '#ABABAB',
            margin: 16,
            formatter: unitSystem ?
                createAxisLabelFormatter(unitSystem) :
                function (value) {
                    // Format log scale labels more compactly if needed
                    if (logScale && Math.abs(value) >= 1000) {
                        return value.toExponential(0);
                    }
                    // Use scientific notation for large/small numbers
                    if (Math.abs(value) > 10000 || (Math.abs(value) > 0 && Math.abs(value) < 0.01)) {
                        return value.toExponential(1);
                    }
                    return value;
                }
        },
        splitLine: {
            lineStyle: {
                color: 'rgba(171, 171, 171, 0.2)'
            }
        }
    };
}