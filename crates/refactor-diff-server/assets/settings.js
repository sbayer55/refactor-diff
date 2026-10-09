"use strict";

// The assistant settings form, shared by the review UI's settings dialog (browser) and the
// standalone /settings page that the desktop app shows in its own Settings window (⌘,).
// Settings live on the server in ~/.config/refactor-diff/settings.json; see /api/settings.

const Settings = (() => {
  const PROVIDER_NAMES = { claude: "Claude", openai: "OpenAI-compatible", ollama: "Ollama" };
  const SPARK = '<svg class="spark" width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M12 3l1.8 5.2L19 10l-5.2 1.8L12 17l-1.8-5.2L5 10l5.2-1.8z"></path></svg>';

  function esc(s) {
    return String(s ?? "").replace(/[&<>"']/g, (c) => (
      { "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]
    ));
  }

  async function api(path, body) {
    const res = await fetch(path, body === undefined ? {} : {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(body),
    });
    const data = await res.json().catch(() => ({ error: `HTTP ${res.status}` }));
    if (!res.ok) throw new Error(data.error || `HTTP ${res.status}`);
    return data;
  }

  function shortModel(model) {
    return (model || "").replace(/^claude-/, "");
  }

  function providerLabel(ai) {
    if (!ai) return "";
    const name = PROVIDER_NAMES[ai.provider] || ai.provider;
    return ai.model ? `${name} · ${shortModel(ai.model)}` : name;
  }

  // Fill `root` with the form for `data` (an /api/settings response). `onSaved(saved)` gets
  // the saved settings; `onClose()` runs for Cancel and the × button.
  function render(root, data, { onSaved, onClose, closeLabel = "Cancel" }) {
    const ai = data.ai;
    const $ = (sel) => root.querySelector(sel);
    const field = (label, name, value, { type = "text", width = "", placeholder = "", list = "", hint = "" } = {}) =>
      `<label class="field ${width}">${label}${hint ? ` <span class="muted">${hint}</span>` : ""}
        <input name="${name}" type="${type}" value="${esc(String(value ?? ""))}" placeholder="${esc(placeholder)}" ${list ? `list="${list}"` : ""} autocomplete="off" spellcheck="false"></label>`;
    const p = ai.providers;
    root.innerHTML = `
      <header><span class="kind ai">AI</span><h3>Assistant settings</h3><span class="meta muted">Used by Ask on every hunk and line</span>
        <button type="button" class="close" aria-label="Close" id="settings-close">×</button></header>
      <form id="settings-form" class="settings-body" autocomplete="off">
        <div class="settings-row">
          <span class="muted">Provider</span>
          <div class="segmented" role="tablist" aria-label="AI provider">
            ${["claude", "openai", "ollama"].map((id) => `<button type="button" role="tab" data-provider="${id}" aria-selected="${ai.provider === id}">${PROVIDER_NAMES[id]}</button>`).join("")}
          </div>
          <span class="ai-status" id="settings-status"></span>
          <span class="spacer"></span>
          <button type="button" class="toggle" id="settings-test">Test connection</button>
        </div>
        <div class="fields" data-fields="claude" ${ai.provider === "claude" ? "" : "hidden"}>
          ${field("API key", "claude.api_key", p.claude.api_key, { type: "password", width: "wide", placeholder: "sk-ant-…" })}
          ${field("Model", "claude.model", p.claude.model, { list: "claude-models" })}
          ${field("Base URL", "claude.base_url", p.claude.base_url, { width: "wide", placeholder: "https://api.anthropic.com", hint: "(optional)" })}
          <datalist id="claude-models"><option value="claude-opus-5-5"><option value="claude-sonnet-5-5"><option value="claude-haiku-5-5"></datalist>
        </div>
        <div class="fields" data-fields="openai" ${ai.provider === "openai" ? "" : "hidden"}>
          ${field("Base URL", "openai.base_url", p.openai.base_url, { width: "wide", placeholder: "http://127.0.0.1:8080/v1" })}
          ${field("API key", "openai.api_key", p.openai.api_key, { type: "password", hint: "(if required)" })}
          ${field("Model", "openai.model", p.openai.model, { list: "openai-models" })}
          <datalist id="openai-models"></datalist>
          <p class="muted">Any endpoint that speaks the OpenAI chat-completions API: a Cursor proxy, OpenRouter, LM Studio, vLLM, or Ollama's <span class="mono">/v1</span>.</p>
        </div>
        <div class="fields" data-fields="ollama" ${ai.provider === "ollama" ? "" : "hidden"}>
          ${field("Host", "ollama.host", p.ollama.host, { width: "wide" })}
          ${field("Model", "ollama.model", p.ollama.model, { list: "ollama-models", placeholder: "qwen3-coder:30b" })}
          ${field("Context window", "ollama.num_ctx", p.ollama.num_ctx, { type: "number", width: "narrow" })}
          <datalist id="ollama-models"></datalist>
        </div>
        <div class="settings-preview">
          <div class="filter-label">How it shows up</div>
          <div class="row"><span class="muted">Top bar</span><span class="chip ai-chip">${SPARK} AI <b data-preview></b></span><span class="muted">always visible; click to open these settings</span></div>
          <div class="row"><span class="muted">Ask menu</span><span class="pill ai-provider" data-preview></span><span class="muted">on every menu before you pick a task</span></div>
          <div class="row"><span class="muted">Each answer</span><span class="kind ai">AI</span><b>Explain this change</b><span class="pill ai-provider" data-preview></span><span class="muted">2.1 s</span></div>
        </div>
        <div class="settings-row toggles">
          <span class="muted">Custom prompts include by default</span>
          <label class="toggle"><input type="checkbox" name="context.function" ${ai.context.function ? "checked" : ""}>Enclosing function</label>
          <label class="toggle"><input type="checkbox" name="context.pr" ${ai.context.pr ? "checked" : ""}>PR description</label>
          <label class="toggle"><input type="checkbox" name="context.references" ${ai.context.references ? "checked" : ""}>References</label>
        </div>
      </form>
      <footer><span class="meta muted">Saved to <span class="mono">~/.config/refactor-diff/settings.json</span> · the key only ever goes to the provider</span>
        <span class="spacer"></span>
        <button type="button" class="toggle" id="settings-cancel">${esc(closeLabel)}</button>
        <button type="button" class="primary" id="settings-save">Save</button></footer>`;
    const form = $("#settings-form");
    let provider = ai.provider;
    const value = (name) => form.querySelector(`[name="${name}"]`).value;
    const current = () => ({
      provider,
      providers: {
        claude: { api_key: value("claude.api_key"), model: value("claude.model"), base_url: value("claude.base_url") },
        openai: { base_url: value("openai.base_url"), api_key: value("openai.api_key"), model: value("openai.model") },
        ollama: { host: value("ollama.host"), model: value("ollama.model"), num_ctx: value("ollama.num_ctx") },
      },
      context: {
        function: form.querySelector('[name="context.function"]').checked,
        pr: form.querySelector('[name="context.pr"]').checked,
        references: form.querySelector('[name="context.references"]').checked,
      },
    });
    const preview = () => {
      const cfg = current();
      const label = providerLabel({ provider, model: cfg.providers[provider].model });
      for (const el of root.querySelectorAll("[data-preview]")) el.textContent = label;
    };
    const status = (text, cls = "") => { const el = $("#settings-status"); el.textContent = text; el.className = `ai-status ${cls}`; };
    const selectProvider = (id) => {
      provider = id;
      for (const b of root.querySelectorAll("[data-provider]")) b.setAttribute("aria-selected", String(b.dataset.provider === id));
      for (const f of root.querySelectorAll("[data-fields]")) f.hidden = f.dataset.fields !== id;
      status("");
      preview();
    };
    for (const b of root.querySelectorAll("[data-provider]")) b.addEventListener("click", () => selectProvider(b.dataset.provider));
    form.addEventListener("input", preview);
    preview();
    $("#settings-test").addEventListener("click", async () => {
      status("Testing…", "live");
      const btn = $("#settings-test");
      btn.disabled = true;
      try {
        const res = await api("/api/settings/test", { provider, config: current().providers[provider] });
        if (res.ok) {
          status(`Connected · ${(res.latency_ms / 1000).toFixed(1)} s`, "ok");
          const list = root.querySelector(`#${provider}-models`);
          if (list && res.models?.length) list.innerHTML = res.models.map((m) => `<option value="${esc(m)}">`).join("");
        } else {
          status(res.error, "error");
        }
      } catch (e) {
        status(e.message, "error");
      } finally {
        btn.disabled = false;
      }
    });
    $("#settings-close").addEventListener("click", onClose);
    $("#settings-cancel").addEventListener("click", onClose);
    $("#settings-save").addEventListener("click", async () => {
      const btn = $("#settings-save");
      btn.disabled = true;
      try {
        const saved = await api("/api/settings", { ai: current() });
        onSaved(saved, status);
      } catch (e) {
        status(e.message, "error");
      } finally {
        btn.disabled = false;
      }
    });
    form.addEventListener("keydown", (e) => {
      if (e.key === "Enter" && e.target.tagName === "INPUT") { e.preventDefault(); $("#settings-save").click(); }
    });
  }

  return { PROVIDER_NAMES, SPARK, api, providerLabel, shortModel, render };
})();
