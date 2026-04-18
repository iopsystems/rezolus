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
     * Returns JSON with viewer info (interval, source, version, metric names)
     */
    info(): string;
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

export function init(): void;

export type InitInput = RequestInfo | URL | Response | BufferSource | WebAssembly.Module;

export interface InitOutput {
    readonly memory: WebAssembly.Memory;
    readonly __wbg_viewer_free: (a: number, b: number) => void;
    readonly init: () => void;
    readonly viewer_file_metadata_json: (a: number) => [number, number];
    readonly viewer_info: (a: number) => [number, number];
    readonly viewer_metadata: (a: number) => [number, number];
    readonly viewer_new: (a: number, b: number, c: number, d: number) => [number, number, number];
    readonly viewer_query: (a: number, b: number, c: number, d: number) => [number, number];
    readonly viewer_query_range: (a: number, b: number, c: number, d: number, e: number, f: number) => [number, number];
    readonly viewer_selection: (a: number) => [number, number];
    readonly viewer_systeminfo: (a: number) => [number, number];
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
