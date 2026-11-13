// Loads the web UI, which is Rust compiled to WebAssembly (crates/hearth-web).
import init from "./hearth-web.js";
init({ module_or_path: new URL("./hearth-web_bg.wasm", import.meta.url) });
