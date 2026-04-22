/* tslint:disable */
/* eslint-disable */

export class Viewer {
    free(): void;
    [Symbol.dispose](): void;
    /**
     * Returns all file-level metadata as a JSON object, mirroring the
     * server's /file_metadata endpoint.  Values that are valid JSON are
     * embedded as-is; everything else becomes a JSON string.
     *
     * Includes pre-computed `nodes`, `node_versions`, and
     * `service_instances` fields so the frontend doesn't have to
     * re-parse `per_source_metadata` itself.
     */
    file_metadata_json(): string;
    /**
     * Returns the full View JSON for a dashboard section.
     */
    get_section(key: string): string | undefined;
    /**
     * Returns the sections list as a JSON array.
     */
    get_sections(): string;
    /**
     * Returns JSON with viewer info (interval, source, version, metric names)
     */
    info(): string;
    /**
     * Accept a JSON array of ServiceExtension templates, detect which ones
     * match the loaded parquet file, and regenerate dashboards accordingly.
     */
    init_templates(templates_json: string): void;
    /**
     * Returns JSON metadata compatible with /api/v1/metadata
     */
    metadata(): string;
    constructor(data: Uint8Array, filename: string);
    /**
     * Execute a PromQL instant query.
     */
    query(query: string, time: number): string;
    /**
     * Execute a PromQL range query. Returns JSON compatible with
     * /api/v1/query_range response format.
     */
    query_range(query: string, start: number, end: number, step: number): string;
    /**
     * Returns selection JSON from parquet file metadata, or null
     */
    selection(): string | undefined;
    /**
     * Returns systeminfo JSON from parquet file metadata.
     *
     * For multi-node combined files (>1 node in per_source_metadata), returns
     * an object keyed by node name with each node's systeminfo.  For single-node
     * files, returns the flat systeminfo string.
     */
    systeminfo(): string | undefined;
}

/**
 * Registry wrapping up to two `Viewer` instances keyed by capture id
 * ("baseline" / "experiment").  Mirrors the server-side `CaptureRegistry`
 * shape so the JS transport layer can address either capture uniformly.
 *
 * This type is additive — existing single-capture `Viewer` consumers are
 * unaffected.
 */
export class WasmCaptureRegistry {
    free(): void;
    [Symbol.dispose](): void;
    /**
     * Attach a parquet capture under the given slot ("baseline" or
     * "experiment").  Replaces any previously attached capture in that slot.
     */
    attach(capture: string, data: Uint8Array, filename: string): void;
    /**
     * Drop the capture in the given slot (no-op if unknown or empty).
     */
    detach(capture: string): void;
    file_metadata_json(capture: string): string | undefined;
    get_section(capture: string, section: string): string | undefined;
    get_sections(capture: string): string | undefined;
    /**
     * Whether a capture is currently attached in the given slot.
     */
    has(capture: string): boolean;
    info(capture: string): string;
    /**
     * Initialise ServiceExtension templates for the given capture.  Mirrors
     * `Viewer::init_templates`.
     */
    init_templates(capture: string, templates_json: string): void;
    metadata(capture: string): string;
    constructor();
    query(capture: string, query: string, time: number): string;
    query_range(capture: string, query: string, start: number, end: number, step: number): string;
    selection(capture: string): string | undefined;
    systeminfo(capture: string): string | undefined;
}

export function init(): void;

export type InitInput = RequestInfo | URL | Response | BufferSource | WebAssembly.Module;

export interface InitOutput {
    readonly memory: WebAssembly.Memory;
    readonly __wbg_viewer_free: (a: number, b: number) => void;
    readonly __wbg_wasmcaptureregistry_free: (a: number, b: number) => void;
    readonly init: () => void;
    readonly viewer_file_metadata_json: (a: number) => [number, number];
    readonly viewer_get_section: (a: number, b: number, c: number) => [number, number];
    readonly viewer_get_sections: (a: number) => [number, number];
    readonly viewer_info: (a: number) => [number, number];
    readonly viewer_init_templates: (a: number, b: number, c: number) => [number, number];
    readonly viewer_metadata: (a: number) => [number, number];
    readonly viewer_new: (a: number, b: number, c: number, d: number) => [number, number, number];
    readonly viewer_query: (a: number, b: number, c: number, d: number) => [number, number];
    readonly viewer_query_range: (a: number, b: number, c: number, d: number, e: number, f: number) => [number, number];
    readonly viewer_selection: (a: number) => [number, number];
    readonly viewer_systeminfo: (a: number) => [number, number];
    readonly wasmcaptureregistry_attach: (a: number, b: number, c: number, d: number, e: number, f: number, g: number) => [number, number];
    readonly wasmcaptureregistry_detach: (a: number, b: number, c: number) => void;
    readonly wasmcaptureregistry_file_metadata_json: (a: number, b: number, c: number) => [number, number];
    readonly wasmcaptureregistry_get_section: (a: number, b: number, c: number, d: number, e: number) => [number, number];
    readonly wasmcaptureregistry_get_sections: (a: number, b: number, c: number) => [number, number];
    readonly wasmcaptureregistry_has: (a: number, b: number, c: number) => number;
    readonly wasmcaptureregistry_info: (a: number, b: number, c: number) => [number, number, number, number];
    readonly wasmcaptureregistry_init_templates: (a: number, b: number, c: number, d: number, e: number) => [number, number];
    readonly wasmcaptureregistry_metadata: (a: number, b: number, c: number) => [number, number, number, number];
    readonly wasmcaptureregistry_new: () => number;
    readonly wasmcaptureregistry_query: (a: number, b: number, c: number, d: number, e: number, f: number) => [number, number, number, number];
    readonly wasmcaptureregistry_query_range: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number) => [number, number, number, number];
    readonly wasmcaptureregistry_selection: (a: number, b: number, c: number) => [number, number];
    readonly wasmcaptureregistry_systeminfo: (a: number, b: number, c: number) => [number, number];
    readonly rust_zstd_wasm_shim_calloc: (a: number, b: number) => number;
    readonly rust_zstd_wasm_shim_free: (a: number) => void;
    readonly rust_zstd_wasm_shim_malloc: (a: number) => number;
    readonly rust_zstd_wasm_shim_memcmp: (a: number, b: number, c: number) => number;
    readonly rust_zstd_wasm_shim_memcpy: (a: number, b: number, c: number) => number;
    readonly rust_zstd_wasm_shim_memmove: (a: number, b: number, c: number) => number;
    readonly rust_zstd_wasm_shim_memset: (a: number, b: number, c: number) => number;
    readonly rust_zstd_wasm_shim_qsort: (a: number, b: number, c: number, d: number) => void;
    readonly __wbindgen_free: (a: number, b: number, c: number) => void;
    readonly __wbindgen_malloc: (a: number, b: number) => number;
    readonly __wbindgen_realloc: (a: number, b: number, c: number, d: number) => number;
    readonly __wbindgen_externrefs: WebAssembly.Table;
    readonly __externref_table_dealloc: (a: number) => void;
    readonly __wbindgen_start: () => void;
}

export type SyncInitInput = BufferSource | WebAssembly.Module;

/**
 * Instantiates the given `module`, which can either be bytes or
 * a precompiled `WebAssembly.Module`.
 *
 * @param {{ module: SyncInitInput }} module - Passing `SyncInitInput` directly is deprecated.
 *
 * @returns {InitOutput}
 */
export function initSync(module: { module: SyncInitInput } | SyncInitInput): InitOutput;

/**
 * If `module_or_path` is {RequestInfo} or {URL}, makes a request and
 * for everything else, calls `WebAssembly.instantiate` directly.
 *
 * @param {{ module_or_path: InitInput | Promise<InitInput> }} module_or_path - Passing `InitInput` directly is deprecated.
 *
 * @returns {Promise<InitOutput>}
 */
export default function __wbg_init (module_or_path?: { module_or_path: InitInput | Promise<InitInput> } | InitInput | Promise<InitInput>): Promise<InitOutput>;
