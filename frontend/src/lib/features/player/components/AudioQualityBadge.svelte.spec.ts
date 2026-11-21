import { page } from 'vitest/browser';
import { render } from 'vitest-browser-svelte';
import { describe, expect, it } from 'vitest';

import AudioQualityBadge from './AudioQualityBadge.svelte';

describe('AudioQualityBadge', () => {
	it('puts bitrate, sample rate and channels directly on the codec hover title in compact mode', async () => {
		await render(AudioQualityBadge, {
			codec: 'flac',
			bitrate: 940,
			sampleRate: 44100,
			audioChannels: 2,
			compact: true
		});

		await expect
			.element(page.getByTitle('Lossless · 940 kbps · 44.1 kHz · Stereo'))
			.toHaveTextContent('flac');
		await expect.element(page.getByText('940 kbps')).not.toBeInTheDocument();
		await expect.element(page.getByText('44.1 kHz')).not.toBeInTheDocument();
	});

	it('renders integer kHz without a decimal, skips non-positive values, and shows non-compact chips', async () => {
		await render(AudioQualityBadge, {
			codec: 'mp3',
			bitrate: 320,
			sampleRate: 48000,
			audioChannels: 1,
			container: 'mpeg',
			compact: false
		});

		await expect.element(page.getByTitle('Lossy')).toHaveTextContent('mp3');
		await expect.element(page.getByText('320 kbps')).toBeInTheDocument();
		await expect.element(page.getByText('48 kHz')).toBeInTheDocument();
		await expect.element(page.getByText('Mono')).toBeInTheDocument();
		await expect.element(page.getByText('mpeg')).toBeInTheDocument();

		await render(AudioQualityBadge, {
			codec: 'aac',
			bitrate: 0,
			sampleRate: 0,
			audioChannels: 0,
			compact: true
		});

		await expect.element(page.getByTitle('Lossy').nth(1)).toHaveTextContent('aac');
	});

	it('renders nothing in compact mode when both codec and converted are absent', async () => {
		const { container } = await render(AudioQualityBadge, {
			bitrate: 320,
			sampleRate: 44100,
			compact: true
		});

		expect(container.textContent).toBe('');
		expect(container.children.length).toBe(0);
	});
});
