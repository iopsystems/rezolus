// overlays.js — Overlay UI: toast notifications and save-as modal.

// ── Toast Notifications ─────────────────────────────────────────────

let toastContainer = null;

const ensureContainer = () => {
    if (toastContainer && document.body.contains(toastContainer)) return;
    toastContainer = document.createElement('div');
    toastContainer.id = 'toast-container';
    document.body.appendChild(toastContainer);
};

const notify = (level, message, durationMs = 5000) => {
    ensureContainer();
    const el = document.createElement('div');
    el.className = `toast toast-${level}`;
    el.textContent = message;

    const dismiss = document.createElement('button');
    dismiss.className = 'toast-dismiss';
    dismiss.textContent = '\u00d7';
    dismiss.onclick = () => remove();
    el.appendChild(dismiss);

    toastContainer.appendChild(el);
    // trigger reflow then add visible class for transition
    el.offsetHeight; // eslint-disable-line no-unused-expressions
    el.classList.add('toast-visible');

    let timer = setTimeout(() => remove(), durationMs);

    function remove() {
        clearTimeout(timer);
        el.classList.remove('toast-visible');
        el.addEventListener('transitionend', () => el.remove(), { once: true });
        // fallback removal if transition doesn't fire
        setTimeout(() => el.remove(), 400);
    }
};

// ── Save-As Modal ───────────────────────────────────────────────────

const modalState = {
    visible: false,
    prefix: '',
    suffix: '',
    onConfirm: null,
    checkboxes: [],  // [{key, label, checked}]
};

const showSaveModal = (defaultPrefix, suffix, checkboxes) => {
    return new Promise((resolve) => {
        modalState.visible = true;
        modalState.prefix = defaultPrefix;
        modalState.suffix = suffix;
        modalState.checkboxes = (checkboxes || []).map(cb => ({ ...cb }));
        modalState.onConfirm = (result) => resolve(result);
        m.redraw();
    });
};

const _closeModal = (result) => {
    const cb = modalState.onConfirm;
    modalState.visible = false;
    modalState.prefix = '';
    modalState.suffix = '';
    modalState.checkboxes = [];
    modalState.onConfirm = null;
    m.redraw();
    if (cb) cb(result);
};

const _buildResult = () => {
    const filename = modalState.prefix.trim() + modalState.suffix;
    if (modalState.checkboxes.length === 0) return filename;
    const opts = {};
    for (const cb of modalState.checkboxes) opts[cb.key] = cb.checked;
    return { filename, ...opts };
};

const SaveModal = {
    view() {
        if (!modalState.visible) return null;

        return m('div.save-modal-overlay', {
            onclick: (e) => {
                if (e.target === e.currentTarget) _closeModal(null);
            },
            onkeydown: (e) => {
                if (e.key === 'Escape') _closeModal(null);
            },
        }, [
            m('div.save-modal', [
                m('div.save-modal-title', 'Save as'),
                m('div.save-modal-input-row', [
                    m('input.save-modal-input', {
                        type: 'text',
                        value: modalState.prefix,
                        oninput: (e) => { modalState.prefix = e.target.value; },
                        oncreate: (vnode) => {
                            vnode.dom.focus();
                            vnode.dom.select();
                        },
                        onkeydown: (e) => {
                            if (e.key === 'Enter' && modalState.prefix.trim()) {
                                e.preventDefault();
                                _closeModal(_buildResult());
                            }
                            if (e.key === 'Escape') {
                                e.preventDefault();
                                _closeModal(null);
                            }
                        },
                    }),
                    m('span.save-modal-suffix', modalState.suffix),
                ]),
                modalState.checkboxes.length > 0 && m('div.save-modal-options',
                    modalState.checkboxes.map(cb =>
                        m('label.save-modal-checkbox', [
                            m('input', {
                                type: 'checkbox',
                                checked: cb.checked,
                                onchange: (e) => { cb.checked = e.target.checked; },
                            }),
                            ' ', cb.label,
                        ]),
                    ),
                ),
                m('div.save-modal-actions', [
                    m('button.save-modal-btn.save-modal-cancel', {
                        onclick: () => _closeModal(null),
                    }, 'Cancel'),
                    m('button.save-modal-btn.save-modal-confirm', {
                        onclick: () => {
                            if (modalState.prefix.trim()) {
                                _closeModal(_buildResult());
                            }
                        },
                        disabled: !modalState.prefix.trim(),
                    }, 'Save'),
                ]),
            ]),
        ]);
    },
};

export { notify, showSaveModal, SaveModal };
