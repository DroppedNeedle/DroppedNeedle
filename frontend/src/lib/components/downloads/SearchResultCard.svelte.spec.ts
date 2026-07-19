import { page } from '@vitest/browser/context';
import { describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

import type { ScoredCandidate } from '$lib/types';

import SearchResultCard from './SearchResultCard.svelte';

type RenderOpts = Parameters<typeof render<typeof SearchResultCard>>[1];

function renderCard(props: Record<string, unknown>) {
	return render(SearchResultCard, { props } as unknown as RenderOpts);
}

function makeCandidate(overrides: Partial<ScoredCandidate> = {}): ScoredCandidate {
	return {
		username: 'alice',
		parent_directory: 'Radiohead - OK Computer (1997)',
		files: [
			{
				username: 'alice',
				filename: 'Radiohead/OK Computer/01 Airbag.flac',
				parent_directory: 'Radiohead - OK Computer (1997)',
				size: 30_000_000,
				extension: 'flac',
				bitrate: null,
				bit_depth: 16,
				sample_rate: 44100,
				duration: 284,
				has_free_slot: true,
				upload_speed: 2_000_000
			}
		],
		coherence: 0.95,
		file_confidence: 0.9,
		final_score: 0.88,
		tier: 'manual',
		...overrides
	};
}

describe('SearchResultCard.svelte', () => {
	it('renders folder, peer, score percentage and format', async () => {
		renderCard({ candidate: makeCandidate() });
		await expect.element(page.getByText('Radiohead - OK Computer (1997)')).toBeInTheDocument();
		await expect.element(page.getByText('alice')).toBeInTheDocument();
		await expect.element(page.getByText('88%')).toBeInTheDocument();
		await expect.element(page.getByText('FLAC')).toBeInTheDocument();
	});

	it('calls onPick when Pick is clicked', async () => {
		const onPick = vi.fn();
		renderCard({ candidate: makeCandidate(), onPick });
		await page.getByRole('button', { name: /Pick candidate from alice/ }).click();
		expect(onPick).toHaveBeenCalledOnce();
	});

	it('locks the Pick button when disabled (double-pick guard)', async () => {
		const onPick = vi.fn();
		renderCard({ candidate: makeCandidate(), onPick, disabled: true });
		// a disabled button can't dispatch onclick, so this proves a second pick can't fire
		await expect
			.element(page.getByRole('button', { name: /Pick candidate from alice/ }))
			.toBeDisabled();
		expect(onPick).not.toHaveBeenCalled();
	});

	it('exposes the score breakdown via a tooltip', async () => {
		renderCard({ candidate: makeCandidate() });
		await expect.element(page.getByText('88%')).toBeInTheDocument();
		const tip = document.querySelector('[data-tip]');
		expect(tip?.getAttribute('data-tip')).toContain('Coherence');
	});

	it('renders the Usenet variant with indexer, format and size', async () => {
		const usenet = makeCandidate({
			source: 'usenet',
			username: '',
			files: [],
			usenet_release: {
				indexer_id: 'ds',
				indexer_name: 'DrunkenSlug',
				guid: 'g',
				title: 'Radiohead-OK_Computer-FLAC-1997',
				nzb_url: 'https://idx/nzb',
				size_bytes: 2_315_726_631,
				category_ids: [3040],
				grabs: 205,
				files: 113,
				usenet_date: null
			}
		});
		renderCard({ candidate: usenet, albumTitle: 'OK Computer' });
		await expect.element(page.getByText('OK Computer')).toBeInTheDocument(); // clean album heading
		await expect.element(page.getByText('DrunkenSlug')).toBeInTheDocument();
		// the format badge is exactly "FLAC" (the release title also contains "FLAC").
		await expect.element(page.getByText('FLAC', { exact: true })).toBeInTheDocument();
		await expect.element(page.getByText('2.2 GB')).toBeInTheDocument();
	});

	it('shows "unknown" format when an obfuscated title has no quality category', async () => {
		const usenet = makeCandidate({
			source: 'usenet',
			username: '',
			files: [],
			usenet_release: {
				indexer_id: 'ds',
				indexer_name: 'DS',
				guid: 'g',
				title: 'aHR0cHM6 scrambled xQ',
				nzb_url: 'https://idx/nzb',
				size_bytes: 400_000_000,
				category_ids: [],
				grabs: null,
				files: null,
				usenet_date: null
			}
		});
		renderCard({ candidate: usenet, albumTitle: 'Some Album' });
		await expect.element(page.getByText('unknown')).toBeInTheDocument();
	});
});

it('renders the torrent variant with indexer, format, size, and seeders', async () => {
	const torrent = makeCandidate({
		source: 'torrent',
		username: '',
		files: [],
		torrent_release: {
			indexer_id: 'red',
			indexer_name: 'RED',
			guid: 't1',
			title: 'Radiohead - OK Computer FLAC',
			download_url: 'https://prowlarr/download/1',
			magnet_url: 'magnet:?xt=urn:btih:abc',
			info_hash: 'abc',
			size_bytes: 1_500_000_000,
			category_ids: [3040],
			seeders: 17
		}
	});
	renderCard({ candidate: torrent, albumTitle: 'OK Computer' });
	await expect.element(page.getByText('RED')).toBeInTheDocument();
	await expect.element(page.getByText('FLAC', { exact: true })).toBeInTheDocument();
	await expect.element(page.getByText('1.4 GB')).toBeInTheDocument();
	await expect.element(page.getByText('17')).toBeInTheDocument();
	await expect
		.element(page.getByRole('button', { name: 'Pick release from RED' }))
		.toBeInTheDocument();
});
