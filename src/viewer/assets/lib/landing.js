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

const FileUpload = {
    view({ attrs }) {
        const { onFile, onConnect, onDemo, loading, error } = attrs;
        return m('div.upload-container', [
            m('div.upload-card', [
                m('h1.upload-title', 'Rezolus Viewer'),
                m('p.upload-subtitle', 'Drop a parquet file to explore system performance metrics.'),
                m('div.upload-dropzone', {
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
                        if (file && onFile) onFile(file);
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
                                if (file && onFile) onFile(file);
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
                        m('p.upload-or', 'or'),
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

export { FileUpload };
