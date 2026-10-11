import "@testing-library/jest-dom/vitest"

// jsdom doesn't implement a few layout APIs that ProseMirror's EditorView
// touches on mount (used by Tiptap-based editors such as the message composer).
// Polyfill them as no-ops so headless/component editor tests can construct a
// view. Only defined when missing, so real browsers/environments are untouched.
if (typeof document !== "undefined" && !document.elementFromPoint) {
  document.elementFromPoint = () => null
}
if (typeof Element !== "undefined") {
  // jsdom doesn't implement scrollIntoView; the composer's suggestion popup
  // calls it to keep the active row visible.
  Element.prototype.scrollIntoView ??= () => {}
  // jsdom doesn't implement Pointer Capture; Radix menus/popovers touch these
  // during the pointer interactions @testing-library/user-event drives.
  Element.prototype.hasPointerCapture ??= () => false
  Element.prototype.setPointerCapture ??= () => {}
  Element.prototype.releasePointerCapture ??= () => {}
}
if (typeof globalThis !== "undefined" && !("ResizeObserver" in globalThis)) {
  // jsdom doesn't implement ResizeObserver; cmdk (the command palette used by
  // the branch/folder pickers) constructs one on mount. A no-op stub is enough
  // for headless rendering — layout callbacks never need to fire.
  ;(globalThis as { ResizeObserver?: unknown }).ResizeObserver = class {
    observe() {}
    unobserve() {}
    disconnect() {}
  }
}
if (typeof window !== "undefined" && !window.matchMedia) {
  // jsdom doesn't implement matchMedia, which `useMediaQuery` (and so
  // `useIsMobile`) subscribes to on mount. Nothing matches by default, so a
  // component tree renders at its desktop breakpoint; a test that needs the
  // other side stubs this itself.
  window.matchMedia = ((query: string) => ({
    matches: false,
    media: query,
    onchange: null,
    addEventListener: () => {},
    removeEventListener: () => {},
    addListener: () => {},
    removeListener: () => {},
    dispatchEvent: () => false,
  })) as unknown as typeof window.matchMedia
}
if (typeof Range !== "undefined") {
  Range.prototype.getClientRects ??= () =>
    ({
      length: 0,
      item: () => null,
      [Symbol.iterator]: function* () {},
    }) as unknown as DOMRectList
  Range.prototype.getBoundingClientRect ??= () =>
    ({
      top: 0,
      left: 0,
      right: 0,
      bottom: 0,
      width: 0,
      height: 0,
      x: 0,
      y: 0,
    }) as DOMRect
}

// [crewchat] Node ≥ 25 ships `localStorage` / `sessionStorage` globals that are
// unusable without `--localstorage-file`, and vitest's jsdom environment leaves
// keys that already exist on globalThis alone — so tests see `undefined` where
// jsdom's Storage should be. Provide real `Storage` instances here: they share
// the (patched) `Storage.prototype`, which keeps the suite's
// `vi.spyOn(Storage.prototype, "setItem")` assertions working. Remove once
// upstream handles the new Node globals.
if (typeof window !== "undefined") {
  const missing = (["localStorage", "sessionStorage"] as const).filter(
    (key) => !globalThis[key]
  )
  if (missing.length > 0 && typeof Storage !== "undefined") {
    // jsdom's Storage methods read internal slots, so a bare
    // `Object.create(Storage.prototype)` cannot use them; back every instance
    // with its own Map through the prototype methods instead.
    const slots = new WeakMap<object, Map<string, string>>()
    const storeOf = (self: object): Map<string, string> => {
      let store = slots.get(self)
      if (!store) {
        store = new Map()
        slots.set(self, store)
      }
      return store
    }
    Object.defineProperties(Storage.prototype, {
      length: {
        configurable: true,
        get(this: object) {
          return storeOf(this).size
        },
      },
      clear: {
        configurable: true,
        writable: true,
        value(this: object) {
          storeOf(this).clear()
        },
      },
      getItem: {
        configurable: true,
        writable: true,
        value(this: object, key: string) {
          const store = storeOf(this)
          return store.has(key) ? store.get(key)! : null
        },
      },
      key: {
        configurable: true,
        writable: true,
        value(this: object, index: number) {
          return Array.from(storeOf(this).keys())[index] ?? null
        },
      },
      removeItem: {
        configurable: true,
        writable: true,
        value(this: object, key: string) {
          storeOf(this).delete(key)
        },
      },
      setItem: {
        configurable: true,
        writable: true,
        value(this: object, key: string, value: string) {
          storeOf(this).set(key, String(value))
        },
      },
    })
    for (const key of missing) {
      Object.defineProperty(globalThis, key, {
        value: Object.create(Storage.prototype),
        configurable: true,
        writable: true,
      })
    }
  }
}
