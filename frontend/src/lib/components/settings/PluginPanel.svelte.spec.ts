import { page } from 'vitest/browser';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

const h = vi.hoisted(() => ({ head: vi.fn(), list: vi.fn(), address: '' }));

vi.mock('$lib/api/client', () => ({
	api: { global: { head: h.head, v3: { GET: h.list } } }
}));
vi.mock('$lib/queries/plugins/endpoints', () => ({
	PLUGIN_ENDPOINTS: { panelPage: () => h.address, list: () => '/api/v3/plugins' }
}));
vi.mock('$lib/queries/plugins/PluginSourceQueries.svelte', () => ({
	fetchPluginSources: vi.fn()
}));

import PluginPanel from './PluginPanel.svelte';

type Heard = { from: string; asked?: boolean; reply?: unknown };

/**
 * A page for a sandboxed frame: it asks the parent for `health.get`, says it
 * has asked, and passes on whatever the parent answers.
 */
function framePage(from: string): string {
	const script =
		`addEventListener('message',(e)=>parent.postMessage({from:'${from}',reply:e.data},'*'));` +
		`parent.postMessage({id:1,method:'health.get'},'*');` +
		`parent.postMessage({from:'${from}',asked:true},'*');`;
	return 'data:text/html,' + encodeURIComponent(`<!doctype html><script>${script}</` + `script>`);
}

describe('PluginPanel', () => {
	const heard: Heard[] = [];
	const listen = (event: MessageEvent): void => {
		const data = event.data as Heard | null;
		if (data && typeof data.from === 'string') heard.push(data);
	};
	let stranger: HTMLIFrameElement | null = null;

	afterEach(() => {
		window.removeEventListener('message', listen);
		stranger?.remove();
		stranger = null;
		heard.length = 0;
		h.head.mockReset();
		h.list.mockReset();
	});

	it('frames the served page sandboxed and answers only that frame', async () => {
		h.head.mockResolvedValue({ ok: true, status: 200 });
		h.list.mockResolvedValue({
			plugins: [
				{
					name: 'toy',
					enabled: true,
					error: null,
					active_capabilities: ['indexer'],
					settings_values: {}
				}
			]
		});
		h.address = framePage('panel');
		window.addEventListener('message', listen);

		await render(PluginPanel, { pluginName: 'toy', displayName: 'Toy' });

		const frame = page.getByTitle('Toy panel');
		await expect.element(frame).toHaveAttribute('sandbox', 'allow-scripts');
		await expect.element(frame).toHaveAttribute('src', h.address);
		await expect.element(frame).not.toHaveAttribute('srcdoc');
		await expect
			.poll(() => heard.find((message) => message.from === 'panel' && message.reply))
			.toEqual({
				from: 'panel',
				reply: {
					id: 1,
					result: { enabled: true, error: null, active_capabilities: ['indexer'] }
				}
			});

		// Another sandboxed frame has the same opaque origin, so only the source
		// check tells it from the panel.
		stranger = document.createElement('iframe');
		stranger.setAttribute('sandbox', 'allow-scripts');
		stranger.src = framePage('stranger');
		document.body.append(stranger);
		await expect
			.poll(() => heard.some((message) => message.from === 'stranger' && message.asked))
			.toBe(true);

		expect(h.list).toHaveBeenCalledTimes(1);
		expect(heard.some((message) => message.from === 'stranger' && message.reply)).toBe(false);
	});
});
