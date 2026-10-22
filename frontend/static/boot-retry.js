// The page's boot script downloads the app's code before the app exists, so
// the app's own error hook (src/hooks.client.ts) never hears when that
// download fails. A network change while the page loads aborts it, and the
// page stays blank. This reloads the page once, a second later, when that
// happens before the app has drawn anything.
//
// It shares its session key and timings with reloadAfterFailedModuleLoad in
// src/lib/utils/pageErrors.ts, so the two together reload at most once a
// minute and an outage that lasts cannot loop.
(function () {
	var RELOAD_KEY = 'dn-module-reload-at';
	var RELOAD_WINDOW_MS = 60000;
	var RELOAD_DELAY_MS = 1000;
	// Each browser words a failed module download its own way.
	var FAILED_MODULE_LOAD = /dynamically imported module|Importing a module script failed/;

	window.addEventListener('unhandledrejection', function (event) {
		var error = event.reason;
		if (!(error instanceof TypeError) || !FAILED_MODULE_LOAD.test(error.message)) return;
		// Once the app has drawn, its own hook handles failed downloads.
		var root = document.getElementById('app-root');
		if (!root || root.querySelector(':scope > :not(script)')) return;
		try {
			var now = Date.now();
			var last = Number(sessionStorage.getItem(RELOAD_KEY) || 0);
			if (now - last < RELOAD_WINDOW_MS) return;
			sessionStorage.setItem(RELOAD_KEY, String(now));
		} catch (_) {
			return;
		}
		setTimeout(function () {
			location.reload();
		}, RELOAD_DELAY_MS);
	});
})();
