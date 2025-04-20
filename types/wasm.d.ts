declare module '~/wasm/rust-graph-layouts/pkg/rust-graph-layouts' {
  export class LayoutManager {
    add_node(id: string, x: number, y: number): void;
    add_edge(id: string, source: string, target: string): void;
    remove_node(id: string): void;
    remove_edge(id: string): void;
    apply_fcose_layout(options: string): Promise<string>;
    get_graph_json(): Promise<string>;
    load_graph_json(json: string): Promise<void>;
    parse_and_load_graph(content: string, fileType: string): Promise<void>;
  }

  export default function initJumpCannon(): Promise<void>;
}

// Augment Nuxt's runtime
declare module '#app' {
  interface NuxtApp {
    $createLayoutManager: () => LayoutManager
  }
}

declare module '@vue/runtime-core' {
  interface ComponentCustomProperties {
    $createLayoutManager: () => LayoutManager
  }
}
