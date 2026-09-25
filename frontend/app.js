(function () {
  var FitCtor =
    window.FitAddon && window.FitAddon.FitAddon
      ? window.FitAddon.FitAddon
      : window.FitAddon;

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

  window.addEventListener("resize", sendResize);
})();
