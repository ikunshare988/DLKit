// DLKit 浏览器接管：右键菜单、下载拦截、页面解析。
// 所有请求都通过 native messaging 交给本机的 DLKit，扩展本身不下载文件。

const HOST = "com.dlkit.bridge";
const DEFAULTS = { intercept: true };

function badge(text, color) {
  chrome.action.setBadgeText({ text });
  if (color) chrome.action.setBadgeBackgroundColor({ color });
}

function markOk() {
  badge("", null);
  chrome.action.setTitle({ title: "DLKit 浏览器接管" });
}

function markFail(message) {
  badge("!", "#d9536f");
  chrome.action.setTitle({ title: "DLKit 未连接：" + message });
  chrome.storage.local.set({ lastError: message, lastErrorAt: Date.now() });
}

// 发一条消息给 DLKit，返回 Promise
function sendNative(msg) {
  return new Promise((resolve, reject) => {
    chrome.runtime.sendNativeMessage(HOST, msg, (resp) => {
      if (chrome.runtime.lastError) {
        reject(new Error(chrome.runtime.lastError.message));
        return;
      }
      if (!resp || resp.ok === false) {
        reject(new Error((resp && resp.message) || "DLKit 没有响应"));
        return;
      }
      resolve(resp);
    });
  });
}

async function handoff(msg) {
  try {
    await sendNative(msg);
    markOk();
  } catch (e) {
    markFail(e.message || String(e));
  }
}

function createMenus() {
  chrome.contextMenus.removeAll(() => {
    chrome.contextMenus.create({
      id: "dlkit-link",
      title: "用 DLKit 下载此链接",
      contexts: ["link"]
    });
    chrome.contextMenus.create({
      id: "dlkit-image",
      title: "用 DLKit 下载此图片",
      contexts: ["image"]
    });
    chrome.contextMenus.create({
      id: "dlkit-page",
      title: "用 DLKit 解析此页",
      contexts: ["page"]
    });
  });
}

chrome.runtime.onInstalled.addListener(() => {
  createMenus();
  chrome.storage.local.get(DEFAULTS, (v) => {
    chrome.storage.local.set({ intercept: v.intercept !== false });
  });
});

chrome.contextMenus.onClicked.addListener((info) => {
  if (info.menuItemId === "dlkit-page") {
    if (!info.pageUrl) return;
    handoff({ v: 1, action: "scan", source: "chrome", url: info.pageUrl });
    return;
  }
  const url =
    info.menuItemId === "dlkit-image" ? info.srcUrl : info.linkUrl;
  if (!url) return;
  handoff({
    v: 1,
    action: "download",
    source: "chrome",
    items: [{ url, referer: info.pageUrl || "", filename: "" }]
  });
});

// 浏览器开始下载时取消，并交给 DLKit。用户可在扩展选项里关闭。
chrome.downloads.onCreated.addListener((item) => {
  if (!item.url || !/^https?:/i.test(item.url)) return;
  chrome.storage.local.get(DEFAULTS, async (cfg) => {
    if (cfg.intercept === false) return;
    try {
      await chrome.downloads.cancel(item.id);
      await chrome.downloads.erase({ id: item.id });
    } catch (e) {
      // 取消失败就让它继续走浏览器，不再重复接管
      return;
    }
    handoff({
      v: 1,
      action: "download",
      source: "chrome",
      items: [
        {
          url: item.url,
          referer: item.referrer || "",
          filename: item.filename || ""
        }
      ]
    });
  });
});
