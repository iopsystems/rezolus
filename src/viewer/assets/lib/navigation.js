const buildTopNavAttrs = ({
    data,
    sectionRoute,
    chartsState,
    fileChecksum,
    liveMode = false,
    recording = false,
    onStartRecording,
    onStopRecording,
    onSaveCapture,
    onUploadParquet,
    granularity = null,
    onGranularityChange,
    nodeList = [],
    selectedNode = null,
    nodeVersions = {},
    onNodeChange,
    extra = {},
}) => ({
    sectionRoute,
    groups: data.groups,
    filename: data.filename,
    source: data.source,
    version: data.version,
    interval: data.interval,
    filesize: data.filesize,
    num_series: data.num_series,
    liveMode,
    recording,
    fileChecksum,
    onStartRecording,
    onStopRecording,
    onSaveCapture,
    onUploadParquet,
    granularity,
    onGranularityChange,
    chartsState,
    nodeList,
    selectedNode,
    nodeVersions,
    onNodeChange,
    ...extra,
});

const createMainComponent = ({
    TopNav,
    Sidebar,
    SaveModal,
    SectionContent,
    CompareBanner,
    sectionResponseCache,
    getHasSystemInfo,
    getHasFileMetadata,
    getBaselineSysinfo,
    getExperimentSysinfo,
    getCompareBadgeAttrs,
    buildAttrs,
}) => ({
    view({
        attrs: { activeSection, groups, sections, source, version, filename, interval, filesize, start_time, end_time, num_series, metadata, compareMode },
    }) {
        const badgeAttrs = typeof getCompareBadgeAttrs === 'function'
            ? getCompareBadgeAttrs()
            : null;
        return m(
            'div',
            m(TopNav, buildAttrs(
                { groups, filename, source, version, interval, filesize, num_series },
                activeSection?.route,
                {
                    start_time,
                    end_time,
                    compareMode: !!compareMode,
                    experimentFilename: badgeAttrs?.experimentFilename,
                    onLoadBaseline: badgeAttrs?.onLoadBaseline,
                    onLoadExperiment: badgeAttrs?.onLoadExperiment,
                },
            )),
            CompareBanner && m(CompareBanner, {
                compareMode: !!compareMode,
                baselineSysinfo: typeof getBaselineSysinfo === 'function' ? getBaselineSysinfo() : null,
                experimentSysinfo: typeof getExperimentSysinfo === 'function' ? getExperimentSysinfo() : null,
            }),
            m('main', [
                m(Sidebar, {
                    activeSection,
                    sections,
                    sectionResponseCache,
                    compareMode,
                    hasSystemInfo: !!getHasSystemInfo(),
                    hasFileMetadata: !!(getHasFileMetadata && getHasFileMetadata()),
                }),
                m(SectionContent, {
                    section: activeSection,
                    groups,
                    interval,
                    metadata,
                }),
            ]),
            m(SaveModal),
        );
    },
});

export { buildTopNavAttrs, createMainComponent };
