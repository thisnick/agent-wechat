// Browser-backed visibility observation within one page or component view.
export function intersection(scope, options = {}, retain) {
  const values = options.thresholds ?? [0];
  if (!Array.isArray(values)) throw new Error("Invalid intersection observer thresholds");
  const thresholds = values.length ? values.map(Number) : [0];
  const initial = Number(options.initialRatio ?? 0);
  if (thresholds.length > 100
    || thresholds.some(value => !Number.isFinite(value) || value < 0 || value > 1)
    || !Number.isFinite(initial)) {
    throw new Error("Invalid intersection observer options");
  }
  let relative, margin = "0px", observer, mutation, selector, callback, stopped = false;
  const observed = new Set();
  const initialized = new WeakSet();
  const rect = value => value ? {
    top: value.top, right: value.right, bottom: value.bottom, left: value.left,
    width: value.width, height: value.height,
  } : { top: 0, right: 0, bottom: 0, left: 0, width: 0, height: 0 };
  function relativeTo(next, margins = {}) {
    if (observer || stopped) throw new Error("Intersection observer already started");
    const values = ["top", "right", "bottom", "left"].map(side => margins[side] ?? 0);
    if (values.some(value => !Number.isFinite(value) || Math.abs(value) > 1e6)) {
      throw new Error("Invalid intersection observer margins");
    }
    relative = next;
    margin = values.map(value => value + "px").join(" ");
    return api;
  }
  function refresh() {
    const matches = [...scope.querySelectorAll(selector)];
    const targets = new Set(options.observeAll ? matches : matches.slice(0, 1));
    for (const node of observed) if (!targets.has(node)) {
      observer.unobserve(node);
      observed.delete(node);
    }
    for (const node of targets) if (!observed.has(node)) {
      observed.add(node);
      observer.observe(node);
    }
  }
  const api = {
    relativeTo: (selector, margins) => relativeTo(selector, margins),
    relativeToViewport: margins => relativeTo(null, margins),
    observe(targetSelector, listener) {
      if (observer || stopped || typeof listener !== "function") throw new Error("Invalid intersection observation");
      selector = targetSelector;
      callback = listener;
      const root = relative == null ? null : scope.querySelector(relative);
      if (relative != null && !root) throw new Error("Intersection reference node is missing");
      observer = new IntersectionObserver(entries => {
        if (stopped) return;
        for (const entry of entries) {
          if (!observed.has(entry.target)) continue;
          if (!initialized.has(entry.target)) {
            initialized.add(entry.target);
            if (entry.intersectionRatio === initial || !thresholds.some(value =>
              Math.min(initial, entry.intersectionRatio) <= value && Math.max(initial, entry.intersectionRatio) >= value)) continue;
          }
          callback({
            id: entry.target.id, dataset: { ...entry.target.dataset },
            intersectionRatio: entry.intersectionRatio,
            intersectionRect: rect(entry.intersectionRect),
            boundingClientRect: rect(entry.boundingClientRect),
            relativeRect: rect(entry.rootBounds), time: entry.time,
          });
        }
      }, { root, rootMargin: margin, threshold: thresholds });
      refresh();
      mutation = new MutationObserver(refresh);
      mutation.observe(scope, { subtree: true, childList: true });
    },
    disconnect() {
      stopped = true;
      observer?.disconnect();
      mutation?.disconnect();
      observed.clear();
      retain?.delete(api);
    },
  };
  retain?.add(api);
  return api;
}
