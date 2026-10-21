// 选项页：只保留一个开关，改动即时保存。
const DEFAULTS = { intercept: true };

const box = document.getElementById("intercept");
const status = document.getElementById("status");
document.getElementById("extid").textContent = chrome.runtime.id;

chrome.storage.local.get(DEFAULTS, (v) => {
  box.checked = v.intercept !== false;
});

box.addEventListener("change", () => {
  chrome.storage.local.set({ intercept: box.checked });
  status.textContent = box.checked ? "已开启下载接管" : "已关闭下载接管";
});

chrome.storage.local.get(["lastError", "lastErrorAt"], (v) => {
  if (!v.lastError) return;
  const when = v.lastErrorAt ? new Date(v.lastErrorAt).toLocaleString() : "";
  status.textContent = `最近一次连接失败（${when}）：${v.lastError}`;
});
