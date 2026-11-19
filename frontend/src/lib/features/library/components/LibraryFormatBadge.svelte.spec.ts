import { page } from 'vitest/browser';
import { describe, expect, it } from 'vitest';
import { render } from 'vitest-browser-svelte';
import LibraryFormatBadge from './LibraryFormatBadge.svelte';

describe('LibraryFormatBadge.svelte', () => {
	it('ignores bitrate on lossless codecs', async () => {
		const cases: [string, number, string][] = [
			['flac', 980, 'FLAC'],
			['FLAC', 1024, 'FLAC'],
			['wav', 1411, 'WAV'],
			['alac', 900, 'ALAC']
		];
		for (const [format, bitrate, expected] of cases) {
			const view = await render(LibraryFormatBadge, { props: { format, bitrate } });
			await expect.element(page.getByText(expected, { exact: true })).toBeVisible();
			view.unmount();
		}
	});

	it('formats lossy codecs with a space and rounded bitrate', async () => {
		const cases: [string, number, string][] = [
			['mp3', 320, 'MP3 320'],
			['MP3', 255.6, 'MP3 256'],
			['ogg', 192, 'OGG 192'],
			['oga', 160, 'OGG 160'],
			['opus', 128, 'OPUS 128'],
			['m4a', 256, 'M4A 256'],
			['aac', 192, 'M4A 192'],
			['mp4', 256, 'M4A 256']
		];
		for (const [format, bitrate, expected] of cases) {
			const view = await render(LibraryFormatBadge, { props: { format, bitrate } });
			await expect.element(page.getByText(expected, { exact: true })).toBeVisible();
			view.unmount();
		}
	});

	it('renders codec only when bitrate is missing, zero, or negative', async () => {
		const bitrates: (number | null | undefined)[] = [undefined, null, 0, -128, 0.4];
		for (const bitrate of bitrates) {
			const view = await render(LibraryFormatBadge, { props: { format: 'mp3', bitrate } });
			await expect.element(page.getByText('MP3', { exact: true })).toBeVisible();
			view.unmount();
		}
	});

	it('renders MIXED without a bitrate and nothing when format is absent', async () => {
		const mixed = await render(LibraryFormatBadge, { props: { format: 'mixed', bitrate: 320 } });
		await expect.element(page.getByText('MIXED', { exact: true })).toBeVisible();
		mixed.unmount();

		const empty = await render(LibraryFormatBadge, { props: { format: null, bitrate: 320 } });
		expect(empty.container.textContent?.trim()).toBe('');
		empty.unmount();
	});
});
