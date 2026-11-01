// HttpCatalog panel bundle (example): counter + sources.list render.
// Runs sandboxed in an iframe; talks to the host only via postMessage RPC.
let count = 0, nextId = 0;
const pending = new Map();
const $ = (id) => document.getElementById(id) || document.body.appendChild(Object.assign(document.createElement("div"), { id }));
window.addEventListener("message", (e) => {
  if (e.source !== parent) return;
  const { id, result, error } = e.data || {};
  if (!pending.has(id)) return;
  pending.get(id)(error || null, result);
  pending.delete(id);
});
function rpc(method, params) {
  return new Promise((done) => {
    const id = ++nextId;
    pending.set(id, (err, res) => done(err ? { error: err } : res));
    parent.postMessage({ id, method, params: params || {} }, "*");
  });
}
$("count").textContent = "click to count";
$("count").onclick = () => { $("count").textContent = `clicked ${++count}x`; };
rpc("sources.list", {}).then((res) => { $("sources").textContent = JSON.stringify((res && res.sources) || res); });
