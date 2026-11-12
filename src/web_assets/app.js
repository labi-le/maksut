"use strict";

const tg = window.Telegram && window.Telegram.WebApp ? window.Telegram.WebApp : null;
if (tg) {
  tg.ready();
  tg.expand();
}

const initData = tg ? tg.initData : "";
const who = document.getElementById("who");
const statusEl = document.getElementById("status");
const topicsEl = document.getElementById("topics");
const memoryEmpty = document.getElementById("memory-empty");
const settingsEl = document.getElementById("settings");
const panelMemory = document.getElementById("panel-memory");
const panelConfig = document.getElementById("panel-config");
const tabMemory = document.getElementById("tab-memory");
const tabConfig = document.getElementById("tab-config");

const user = tg && tg.initDataUnsafe ? tg.initDataUnsafe.user : null;
if (user && user.first_name) {
  who.textContent = user.username ? "владелец @" + user.username : "владелец " + user.first_name;
}

function setStatus(text, isError) {
  statusEl.textContent = text || "";
  statusEl.classList.toggle("error", Boolean(isError));
}

async function api(path, options) {
  const request = Object.assign({}, options);
  const headers = Object.assign(
    { Authorization: "tma " + initData },
    options && options.headers ? options.headers : {}
  );
  request.headers = headers;
  const response = await fetch(path, request);
  const text = await response.text();
  let body = null;
  if (text) {
    try {
      body = JSON.parse(text);
    } catch (error) {
      body = null;
    }
  }
  if (!response.ok) {
    const message = body && body.error ? body.error : "HTTP " + response.status;
    throw new Error(message);
  }
  return body;
}

function button(label, className, onClick) {
  const element = document.createElement("button");
  element.type = "button";
  element.textContent = label;
  if (className) {
    element.className = className;
  }
  element.addEventListener("click", onClick);
  return element;
}

function formatTime(seconds) {
  if (!seconds) {
    return "";
  }
  const date = new Date(seconds * 1000);
  return date.toLocaleString("ru-RU", { dateStyle: "short", timeStyle: "short" });
}

function memoryRow(row) {
  const item = document.createElement("li");
  const topic = document.createElement("h2");
  topic.textContent = row.topic;
  item.append(topic);

  const meta = document.createElement("p");
  meta.className = "meta";
  if (row.source_url) {
    const link = document.createElement("a");
    link.href = row.source_url;
    link.target = "_blank";
    link.rel = "noopener noreferrer";
    link.textContent = "источник";
    meta.append(link, document.createTextNode(" · "));
  }
  meta.append(document.createTextNode(formatTime(row.updated_at)));
  item.append(meta);

  const editable = Boolean(row.source_url);
  if (!editable) {
    const note = document.createElement("p");
    note.className = "meta";
    note.textContent = "Записи владельца правятся только через remember_self.";
    item.append(note);
  }

  const editor = document.createElement("textarea");
  editor.rows = 3;
  editor.value = row.fact;
  editor.maxLength = row.fact_max_chars;
  editor.readOnly = !editable;
  item.append(editor);

  if (!editable) {
    return item;
  }

  const actions = document.createElement("div");
  actions.className = "actions";
  const save = button("Сохранить", "", async () => {
    save.disabled = true;
    try {
      const updated = await api("/api/memory/" + encodeURIComponent(row.topic), {
        method: "PUT",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ fact: editor.value })
      });
      await loadMemory();
      setStatus("Сохранено: " + row.topic, false);
    } catch (error) {
      setStatus(error.message, true);
    } finally {
      save.disabled = false;
    }
  });
  const remove = button("Удалить", "secondary", async () => {
    if (!window.confirm("Удалить запись «" + row.topic + "»?")) {
      return;
    }
    remove.disabled = true;
    try {
      await api("/api/memory/" + encodeURIComponent(row.topic), { method: "DELETE" });
      item.remove();
      memoryEmpty.hidden = topicsEl.children.length > 0;
      setStatus("Удалено: " + row.topic, false);
    } catch (error) {
      remove.disabled = false;
      setStatus(error.message, true);
    }
  });
  actions.append(save, remove);
  item.append(actions);
  return item;
}

async function loadMemory() {
  setStatus("Загружаю память…", false);
  try {
    const rows = await api("/api/memory", { method: "GET" });
    topicsEl.replaceChildren();
    for (const row of rows) {
      topicsEl.append(memoryRow(row));
    }
    memoryEmpty.hidden = rows.length > 0;
    setStatus("", false);
  } catch (error) {
    setStatus(error.message, true);
  }
}

function settingRow(entry) {
  const item = document.createElement("li");
  const title = document.createElement("h2");
  title.textContent = entry.label;
  item.append(title);
  const key = document.createElement("p");
  key.className = "meta";
  key.textContent = entry.key;
  item.append(key);
  if (entry.env_shadowed && entry.env_variable) {
    const shadow = document.createElement("p");
    shadow.className = "meta";
    shadow.textContent = "Задано переменной окружения " + entry.env_variable + "; правка файла не действует.";
    item.append(shadow);
  }

  const input = document.createElement("input");
  input.type = entry.kind === "int" ? "number" : "text";
  if (entry.kind === "int") {
    input.step = "1";
    input.min = "0";
  }
  input.value = String(entry.value);
  input.disabled = entry.env_shadowed;
  item.append(input);

  const actions = document.createElement("div");
  actions.className = "actions";
  const save = button("Сохранить", "", async () => {
    let value = input.value.trim();
    if (entry.kind === "int") {
      if (!/^\d+$/.test(value)) {
        setStatus("Нужно целое число", true);
        return;
      }
      value = Number(value);
    }
    save.disabled = true;
    try {
      await api("/api/config", {
        method: "PUT",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ key: entry.key, value: value })
      });
      setStatus("Записано в config.toml; применится после рестарта", false);
    } catch (error) {
      setStatus(error.message, true);
    } finally {
      save.disabled = false;
    }
  });
  save.disabled = entry.env_shadowed;
  actions.append(save);
  item.append(actions);
  return item;
}

async function loadConfig() {
  setStatus("Загружаю настройки…", false);
  try {
    const rows = await api("/api/config", { method: "GET" });
    settingsEl.replaceChildren();
    for (const entry of rows) {
      settingsEl.append(settingRow(entry));
    }
    setStatus("", false);
  } catch (error) {
    setStatus(error.message, true);
  }
}

function selectTab(memory) {
  tabMemory.setAttribute("aria-selected", memory ? "true" : "false");
  tabConfig.setAttribute("aria-selected", memory ? "false" : "true");
  panelMemory.hidden = !memory;
  panelConfig.hidden = memory;
  if (memory) {
    loadMemory();
  } else {
    loadConfig();
  }
}

tabMemory.addEventListener("click", () => selectTab(true));
tabConfig.addEventListener("click", () => selectTab(false));

if (!initData) {
  setStatus("Откройте эту страницу через Telegram: без initData доступа нет.", true);
  tabMemory.disabled = true;
  tabConfig.disabled = true;
} else {
  selectTab(true);
}
