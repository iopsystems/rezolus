// Shared file-upload landing page component.
// Used by both the binary viewer (src/viewer) and the static site (site/viewer).
//
// Props:
//   onFile(file: File)  — called when user selects or drops a .parquet file
//   onConnect(url: str) — called when user enters a live agent URL (optional; hidden if absent)
//   onDemo()            — called when user clicks "Try Demo" (optional; hidden if absent)
//   loading: boolean    — show "Loading..." indicator
//   error: string|null  — show error message

let connectUrl = '';

// Small reusable dropzone for a single parquet slot. Used by the
// compare-mode dual landing and available for other single-slot callers.
// Props: { label, disabled, onDrop(file), onChoose(file) }
const Dropzone = {
    view({ attrs }) {
        const { label, disabled, onDrop, onChoose, checked } = attrs;
        const handleFile = (file) => {
            if (disabled || !file) return;
            if (onDrop) onDrop(file);
            else if (onChoose) onChoose(file);
        };
        return m('div.upload-dropzone', {
            class: disabled ? 'disabled' : '',
            ondragover: (e) => {
                if (disabled) return;
                e.preventDefault();
                e.currentTarget.classList.add('dragover');
            },
            ondragleave: (e) => {
                e.currentTarget.classList.remove('dragover');
            },
            ondrop: (e) => {
                if (disabled) return;
                e.preventDefault();
                e.currentTarget.classList.remove('dragover');
                const file = e.dataTransfer.files[0];
                handleFile(file);
            },
        }, [
            checked
                ? m('div.upload-check', m.trust('<svg width="40" height="40" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><polyline points="20 6 9 17 4 12"/></svg>'))
                : m('svg.upload-icon', {
                    width: 40, height: 40, viewBox: '0 0 24 24',
                    fill: 'none', stroke: 'currentColor', 'stroke-width': 1.5,
                }, [
                    m('path', { d: 'M12 16V4m0 0L8 8m4-4l4 4', 'stroke-linecap': 'round', 'stroke-linejoin': 'round' }),
                    m('path', { d: 'M2 17l.621 2.485A2 2 0 004.561 21h14.878a2 2 0 001.94-1.515L22 17', 'stroke-linecap': 'round', 'stroke-linejoin': 'round' }),
                ]),
            m('p', label || 'Drag & drop a .parquet file here'),
            !disabled && !checked && m('label.upload-btn', [
                'Choose File',
                m('input', {
                    type: 'file',
                    accept: '.parquet',
                    style: 'display:none',
                    onchange: (e) => {
                        const file = e.target.files[0];
                        handleFile(file);
                    },
                }),
            ]),
        ]);
    },
};

// Compare-mode dual-slot landing. Shows a checkmark over the baseline
// slot once it's attached, then lights up the experiment slot as the
// active drop target.
//
// Props:
//   onBaselineFile(file)     — attach baseline parquet
//   onExperimentFile(file)   — attach experiment parquet
//   baselineAttached: bool
//   baselineFilename: string?
//   experimentAttached: bool
//   experimentFilename: string?
//   loading: bool
//   error: string|null
const CompareLanding = {
    view({ attrs }) {
        const {
            onBaselineFile, onExperimentFile,
            baselineAttached, baselineFilename,
            experimentAttached, experimentFilename,
            loading, error,
        } = attrs;
        return m('div.upload-container', [
            m('div.upload-card', [
                m('h1.upload-title', 'Rezolus Viewer — Compare'),
                m('p.upload-subtitle', 'Drop two parquet captures to compare them side-by-side.'),
                m('div.landing-dual', [
                    m('div.landing-slot', [
                        m('h3', 'Baseline'),
                        m(Dropzone, {
                            label: baselineAttached
                                ? (baselineFilename ? `\u2714 ${baselineFilename}` : '\u2714 baseline loaded')
                                : 'Drop baseline.parquet',
                            disabled: baselineAttached || loading,
                            checked: baselineAttached,
                            onDrop: onBaselineFile,
                        }),
                    ]),
                    m('div.landing-slot', [
                        m('h3', 'Experiment'),
                        m(Dropzone, {
                            label: experimentAttached
                                ? (experimentFilename ? `\u2714 ${experimentFilename}` : '\u2714 experiment loaded')
                                : (baselineAttached
                                    ? 'Drop experiment.parquet'
                                    : 'Load baseline first'),
                            disabled: !baselineAttached || experimentAttached || loading,
                            checked: experimentAttached,
                            onDrop: onExperimentFile,
                        }),
                    ]),
                ]),
                error && m('p.upload-error', error),
                loading && m('p.upload-loading', 'Loading...'),
            ]),
        ]);
    },
};

const FileUpload = {
    view({ attrs }) {
        const { onFile, onConnect, onDemo, loading, error } = attrs;
        return m('div.upload-container', [
            m('div.upload-card', [
                m('h1.upload-title', 'Rezolus Viewer'),
                m('p.upload-subtitle', onFile
                    ? 'Drop a parquet file to explore system performance metrics.'
                    : 'Explore system performance metrics.'),
                onFile && m('div.upload-dropzone', {
                    ondragover: (e) => {
                        e.preventDefault();
                        e.currentTarget.classList.add('dragover');
                    },
                    ondragleave: (e) => {
                        e.currentTarget.classList.remove('dragover');
                    },
                    ondrop: (e) => {
                        e.preventDefault();
                        e.currentTarget.classList.remove('dragover');
                        const file = e.dataTransfer.files[0];
                        if (file) onFile(file);
                    },
                }, [
                    m('svg.upload-icon', {
                        width: 48, height: 48, viewBox: '0 0 24 24',
                        fill: 'none', stroke: 'currentColor', 'stroke-width': 1.5,
                    }, [
                        m('path', { d: 'M12 16V4m0 0L8 8m4-4l4 4', 'stroke-linecap': 'round', 'stroke-linejoin': 'round' }),
                        m('path', { d: 'M2 17l.621 2.485A2 2 0 004.561 21h14.878a2 2 0 001.94-1.515L22 17', 'stroke-linecap': 'round', 'stroke-linejoin': 'round' }),
                    ]),
                    m('p', 'Drag & drop a .parquet file here'),
                    m('p.upload-or', 'or'),
                    m('label.upload-btn', [
                        'Choose File',
                        m('input', {
                            type: 'file',
                            accept: '.parquet',
                            style: 'display:none',
                            onchange: (e) => {
                                const file = e.target.files[0];
                                if (file) onFile(file);
                            },
                        }),
                    ]),
                ]),
                onConnect && m('div.upload-connect', [
                    m('p.upload-connect-label', 'or connect to a live agent'),
                    m('input.connect-input', {
                        type: 'text',
                        placeholder: 'http://localhost:4241',
                        value: connectUrl,
                        disabled: loading,
                        oninput: (e) => { connectUrl = e.target.value; },
                        onkeydown: (e) => {
                            if (e.key === 'Enter' && connectUrl.trim()) {
                                onConnect(connectUrl.trim());
                            }
                        },
                    }),
                    m('button.upload-btn.connect-btn', {
                        disabled: loading || !connectUrl.trim(),
                        onclick: () => onConnect(connectUrl.trim()),
                    }, 'Connect'),
                ]),
                (attrs.demos || (onDemo ? [{ label: 'Try Demo' }] : [])).length > 0 &&
                    m('div', { style: 'margin-top: 1.5rem' }, [
                        onFile && m('p.upload-or', 'or'),
                        m('div', { style: 'display: flex; gap: 0.5rem; justify-content: center; margin-top: 0.75rem; flex-wrap: wrap' },
                            (attrs.demos || [{ label: 'Try Demo' }]).map(demo =>
                                m('button.upload-btn', {
                                    style: 'background: #6c757d',
                                    onclick: () => onDemo(demo.file),
                                    disabled: loading,
                                }, demo.label),
                            ),
                        ),
                    ]),
                error && m('p.upload-error', error),
                loading && m('p.upload-loading', 'Loading...'),
            ]),
        ]);
    },
};

export { FileUpload, CompareLanding, Dropzone };
