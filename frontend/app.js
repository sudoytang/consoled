(function () {
  var FitCtor =
    window.FitAddon && window.FitAddon.FitAddon
      ? window.FitAddon.FitAddon
      : window.FitAddon;

  var DEFAULT_TITLE = "console";
  var TITLE_MAX_LEN = 128;

  var term = new Terminal({
    cursorBlink: true,
    fontFamily: "ui-monospace, SFMono-Regular, Menlo, Consolas, monospace",
    theme: { background: "#000000", foreground: "#e6e6e6" },
  });
  var fit = new FitCtor();
  term.loadAddon(fit);
  term.open(document.getElementById("terminal"));

  var proto = window.location.protocol === "https:" ? "wss:" : "ws:";
  var ws = new WebSocket(proto + "//" + window.location.host + "/console");
  ws.binaryType = "arraybuffer";

  function sanitizeTitle(raw) {
    var s = String(raw == null ? "" : raw);
    // Strip ASCII/C1 controls and other Unicode control chars; keep printable text.
    s = s.replace(/[\u0000-\u001F\u007F-\u009F\u200B-\u200F\u202A-\u202E\u2060-\u206F\uFEFF]/g, "");
    s = s.replace(/\s+/g, " ").trim();
    if (s.length > TITLE_MAX_LEN) {
      s = s.slice(0, TITLE_MAX_LEN);
    }
    return s;
  }

  function applyTitle(raw) {
    var s = sanitizeTitle(raw);
    document.title = s ? "consoled — " + s : DEFAULT_TITLE;
  }

  function sendResize() {
    fit.fit();
    if (ws.readyState === WebSocket.OPEN) {
      ws.send(
        JSON.stringify({
          type: "resize",
          cols: term.cols,
          rows: term.rows,
        })
      );
    }
  }

  ws.onopen = function () {
    sendResize();
  };

  ws.onmessage = function (ev) {
    if (ev.data instanceof ArrayBuffer) {
      term.write(new Uint8Array(ev.data));
    }
  };

  ws.onclose = function () {
    applyTitle("");
    term.writeln("");
    term.writeln("[disconnected]");
  };

  ws.onerror = function () {
    term.writeln("");
    term.writeln("[connection error]");
  };

  term.onData(function (data) {
    if (ws.readyState === WebSocket.OPEN) {
      ws.send(new TextEncoder().encode(data));
    }
  });

  term.onTitleChange(function (t) {
    applyTitle(t);
  });

  window.addEventListener("resize", sendResize);
})();
