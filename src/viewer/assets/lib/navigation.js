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
    ...extra,
});

const createMainComponent = ({
    TopNav,
    Sidebar,
    SaveModal,
    SectionContent,
    sectionResponseCache,
    getHasSystemInfo,
    buildAttrs,
}) => ({
    view({
        attrs: { activeSection, groups, sections, source, version, filename, interval, filesize, start_time, end_time, num_series, metadata },
    }) {
        return m(
            'div',
            m(TopNav, buildAttrs(
                { groups, filename, source, version, interval, filesize, num_series },
                activeSection?.route,
                { start_time, end_time },
            )),
            m('main', [
                m(Sidebar, {
                    activeSection,
                    sections,
                    sectionResponseCache,
                    hasSystemInfo: !!getHasSystemInfo(),
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
