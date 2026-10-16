"use strict";
(() => {
  const viewId = document.body.dataset.view;
  const app = document.getElementById("app");
  const dark = window.matchMedia("(prefers-color-scheme: dark)").matches;
  const collapsed = new Set();
  const previewing = new Set();
  const loaded = new Set();

  mermaid.initialize({ startOnLoad: false, securityLevel: "strict", theme: dark ? "dark" : "default" });

  function renderMermaid(root) {
    const nodes = root.querySelectorAll("pre.mermaid:not([data-processed])");
    if (nodes.length) mermaid.run({ nodes }).catch((e) => console.error(e));
  }

  function setLayout(layout) {
    document.body.dataset.layout = layout;
    for (const b of document.querySelectorAll("[data-layout]")) {
      b.setAttribute("aria-pressed", String(b.dataset.layout === layout));
    }
    try { localStorage.setItem("abeview.layout", layout); } catch (_) {}
  }

  async function showPreview(section) {
    const box = section.querySelector(".md-preview");
    const path = section.dataset.path;
    const res = await fetch(`/v/${viewId}/preview?path=${encodeURIComponent(path)}`);
    box.innerHTML = res.ok ? await res.text() : `<p class="error">Failed to load preview (${res.status})</p>`;
    box.hidden = false;
    section.classList.add("previewing");
    renderMermaid(box);
  }

  function hidePreview(section) {
    section.querySelector(".md-preview").hidden = true;
    section.classList.remove("previewing");
  }

  function setDisplayButtons(section, display) {
    for (const b of section.querySelectorAll("[data-display]")) {
      b.setAttribute("aria-pressed", String(b.dataset.display === display));
    }
  }

  async function loadDiff(section) {
    const box = section.querySelector(".load");
    if (!box) return;
    const res = await fetch(`/v/${viewId}/file?path=${encodeURIComponent(section.dataset.path)}`);
    if (!res.ok) {
      box.querySelector("span").textContent = `Failed to load diff (${res.status})`;
      return;
    }
    box.outerHTML = await res.text();
  }

  function restoreFileState() {
    for (const section of app.querySelectorAll("section.file")) {
      const path = section.dataset.path;
      if (loaded.has(path)) loadDiff(section);
      section.classList.toggle("collapsed", collapsed.has(path));
      if (previewing.has(path)) {
        setDisplayButtons(section, "preview");
        showPreview(section);
      } else {
        setDisplayButtons(section, "diff");
      }
    }
  }

  document.addEventListener("click", (e) => {
    const layout = e.target.closest("button[data-layout]");
    if (layout) return setLayout(layout.dataset.layout);

    const section = e.target.closest("section.file");
    if (!section) return;
    const path = section.dataset.path;
    if (e.target.closest(".load-diff")) {
      loaded.add(path);
      loadDiff(section);
      return;
    }
    if (e.target.closest(".collapse")) {
      const now = section.classList.toggle("collapsed");
      now ? collapsed.add(path) : collapsed.delete(path);
      return;
    }
    const display = e.target.closest("button[data-display]");
    if (display) {
      setDisplayButtons(section, display.dataset.display);
      if (display.dataset.display === "preview") {
        previewing.add(path);
        showPreview(section);
      } else {
        previewing.delete(path);
        hidePreview(section);
      }
    }
  });

  async function refresh() {
    const res = await fetch(`/v/${viewId}/content${location.search}`);
    if (!res.ok) return;
    const sidebar = app.querySelector(".sidebar");
    const sidebarScroll = sidebar ? sidebar.scrollTop : 0;
    app.innerHTML = await res.text();
    const newSidebar = app.querySelector(".sidebar");
    if (newSidebar) newSidebar.scrollTop = sidebarScroll;
    restoreFileState();
    renderMermaid(app);
  }

  function connect() {
    const live = document.getElementById("live");
    const events = new EventSource(`/v/${viewId}/events`);
    events.onopen = () => { live.classList.add("on"); live.title = "Live: updates automatically"; };
    events.onerror = () => { live.classList.remove("on"); live.title = "Disconnected"; };
    events.addEventListener("reload", refresh);
  }

  let layout = "unified";
  try { layout = localStorage.getItem("abeview.layout") || layout; } catch (_) {}
  setLayout(layout);
  restoreFileState();
  renderMermaid(app);
  connect();
})();
