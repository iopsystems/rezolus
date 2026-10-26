// The bundled demo recordings were `.parquet` files before Rezolus 6.0 and
// are `.dendro` files now. A bare name resolves under `data/`, which holds
// no `.parquet`, so a shared link naming `demo.parquet` loads `demo.dendro`.
// A URL is left as it is.
export const dendroDemoName = (source) => {
    if (typeof source !== 'string' || /^https?:\/\//i.test(source)) return source;
    return source.endsWith('.parquet') ? `${source.slice(0, -'.parquet'.length)}.dendro` : source;
};
