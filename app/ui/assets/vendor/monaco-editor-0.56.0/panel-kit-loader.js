// panel-kit ↔ monaco-editor integration glue. panel_kit::editor injects
// this file inline (include_str!) and then talks to it exclusively through
// the typed wasm-bindgen externs declared on `globalThis.__panelKitMonaco`.
// The vendored ESM bundle itself (monaco.esm.js) is pulled in with a dynamic
// import from the consumer-served assets directory, so nothing here needs a
// bundler and the same files work under trunk and Tauri. All application
// logic lives in Rust; this file only bridges DOM/JS APIs wasm-bindgen
// cannot express (dynamic import, Worker construction, Monaco's JS object
// graph).
(() => {
  if (globalThis.__panelKitMonaco) return;

  let monaco = null;
  let loadPromise = null;
  const editors = new Map();
  let nextId = 1;

  function load(base) {
    if (!loadPromise) {
      const root = base.replace(/\/+$/, '');
      const css = document.createElement('link');
      css.rel = 'stylesheet';
      css.href = `${root}/monaco.esm.css`;
      document.head.appendChild(css);
      // Classic worker, not {type:'module'}: the bundle carries no ESM
      // syntax, and classic loads in webviews without module-worker support.
      globalThis.MonacoEnvironment = {
        getWorker: () => new Worker(`${root}/editor.worker.js`),
      };
      loadPromise = import(`${root}/monaco.esm.js`).then((m) => {
        monaco = m;
      });
    }
    return loadPromise;
  }

  function editor(id) {
    const e = editors.get(id);
    if (!e) throw new Error(`panel-kit: unknown monaco editor ${id}`);
    return e;
  }

  globalThis.__panelKitMonaco = {
    load,

    create(el, options) {
      const id = nextId++;
      editors.set(id, monaco.editor.create(el, options));
      return id;
    },

    dispose(id) {
      const e = editors.get(id);
      if (e) {
        e.getModel()?.dispose();
        e.dispose();
        editors.delete(id);
      }
    },

    // Plain replace (resets the undo stack) — imperative EditorHandle writes.
    setValue(id, value) {
      const e = editor(id);
      if (e.getValue() !== value) e.setValue(value);
    },

    // Full-model edit that keeps the undo stack and restores the cursor /
    // selection — external Signal<String> writes into a bound editor.
    setValueKeepCursor(id, value) {
      const e = editor(id);
      const model = e.getModel();
      if (!model || e.getValue() === value) return;
      const selections = e.getSelections();
      e.pushUndoStop();
      e.executeEdits('panel-kit', [{ range: model.getFullModelRange(), text: value }]);
      e.pushUndoStop();
      if (selections) e.setSelections(selections);
    },

    getValue(id) {
      return editor(id).getValue();
    },

    setLanguage(id, language) {
      const model = editor(id).getModel();
      if (model) monaco.editor.setModelLanguage(model, language);
    },

    setReadOnly(id, readOnly) {
      editor(id).updateOptions({ readOnly });
    },

    layout(id) {
      editor(id).layout();
    },

    setTheme(name) {
      monaco.editor.setTheme(name);
    },

    onChange(id, callback) {
      const e = editor(id);
      e.onDidChangeModelContent(() => callback(e.getValue()));
    },

    registerLanguage(id, monarch, configuration) {
      if (!monaco.languages.getLanguages().some((l) => l.id === id)) {
        monaco.languages.register({ id });
      }
      monaco.languages.setMonarchTokensProvider(id, monarch);
      if (configuration) monaco.languages.setLanguageConfiguration(id, configuration);
    },

    defineTheme(name, data) {
      monaco.editor.defineTheme(name, data);
    },
  };
})();
